//! 開いているライブラリとそのジョブ。開くときの手順（staging の掃除 → 整合性チェック → ジョブの再開）をまとめる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::Library;
use super::integrity::{IntegrityReport, check_files, clean_staging};
use crate::Result;
use crate::import::ImportHandler;
use crate::jobs::{JobKind, JobManager};
use crate::media::{MediaBackend, ThumbnailHandler};

pub use crate::jobs::EventSink;

/// ライブラリの概要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryInfo {
    pub name: String,
    pub path: PathBuf,
    /// ゴミ箱を除いたアイテム数。
    pub item_count: u64,
    /// 整合性チェックの結果（終わるまでは None）。
    pub integrity: Option<IntegrityReport>,
}

/// 開いているライブラリ。閉じる（drop する）と実行中のジョブを止め、次回の起動で再開する。
pub struct LibrarySession {
    library: Arc<Library>,
    jobs: Arc<JobManager>,
    integrity: Arc<Mutex<Option<IntegrityReport>>>,
    startup: Mutex<Option<JoinHandle<()>>>,
}

impl LibrarySession {
    /// 既存のライブラリを開く。メディア情報とサムネイルは `backend` で作る。
    pub fn open(path: &Path, sink: EventSink, backend: Arc<dyn MediaBackend>) -> Result<Self> {
        Self::start(Library::open(path)?, sink, backend)
    }

    /// 新しいライブラリを作って開く。
    pub fn create(path: &Path, sink: EventSink, backend: Arc<dyn MediaBackend>) -> Result<Self> {
        Self::start(Library::create(path)?, sink, backend)
    }

    fn start(library: Library, sink: EventSink, backend: Arc<dyn MediaBackend>) -> Result<Self> {
        let library = Arc::new(library);
        // ジョブを動かす前に書きかけを消す（再開するジョブは最初からコピーし直す）
        let staging_removed = clean_staging(library.layout())?;
        let jobs = Arc::new(JobManager::new(
            library.clone(),
            vec![
                (
                    JobKind::Import,
                    Arc::new(ImportHandler::new(backend.clone())),
                ),
                (JobKind::Thumbnail, Arc::new(ThumbnailHandler::new(backend))),
            ],
            sink,
        )?);
        let integrity = Arc::new(Mutex::new(None));

        // 整合性チェックが終わってからジョブを動かす（チェック中に取り込みが進むと食い違いを誤って数えるため）
        let (lib, jobs2, slot) = (library.clone(), jobs.clone(), integrity.clone());
        let startup = std::thread::Builder::new()
            .name("kura-startup".into())
            .spawn(move || {
                let report = match check_files(lib.db(), lib.layout()) {
                    Ok((missing_files, orphan_files)) => IntegrityReport {
                        staging_removed,
                        missing_files,
                        orphan_files,
                    },
                    Err(err) => {
                        tracing::warn!(%err, "整合性チェックに失敗しました");
                        IntegrityReport {
                            staging_removed,
                            ..IntegrityReport::default()
                        }
                    }
                };
                if report.missing_files > 0 || report.orphan_files > 0 {
                    tracing::warn!(?report, "DB とファイルに食い違いがあります");
                } else {
                    tracing::info!(?report, "整合性チェックが終わりました");
                }
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some(report);
                }
                if let Err(err) = jobs2.start() {
                    tracing::error!(%err, "ジョブを開始できませんでした");
                }
                // サムネイルが未作成のもの（前回の続き・M1 で取り込んだものなど）があれば作る
                let pending = lib.db().read(|conn| {
                    let mut stmt = conn.prepare_cached(
                        "SELECT EXISTS (SELECT 1 FROM items WHERE thumb_state = 'pending')",
                    )?;
                    Ok(stmt.query_row([], |row| row.get::<_, bool>(0))?)
                });
                if matches!(pending, Ok(true))
                    && let Err(err) = jobs2.ensure_queued(JobKind::Thumbnail, "{}")
                {
                    tracing::error!(%err, "サムネイルのジョブを積めませんでした");
                }
            })?;

        Ok(Self {
            library,
            jobs,
            integrity,
            startup: Mutex::new(Some(startup)),
        })
    }

    /// ライブラリ。
    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    /// ジョブ。
    pub fn jobs(&self) -> &JobManager {
        &self.jobs
    }

    /// 整合性チェックの結果（終わるまでは None）。
    pub fn integrity(&self) -> Option<IntegrityReport> {
        self.integrity.lock().ok().and_then(|r| r.clone())
    }

    /// 概要。
    pub fn info(&self) -> Result<LibraryInfo> {
        Ok(LibraryInfo {
            name: self.library.name(),
            path: self.library.layout().root().to_path_buf(),
            item_count: self.library.item_count()?,
            integrity: self.integrity(),
        })
    }

    /// 開く手順（整合性チェックとジョブの開始）が終わるまで待つ。
    pub fn wait_ready(&self) {
        if let Ok(mut slot) = self.startup.lock()
            && let Some(handle) = slot.take()
            && handle.join().is_err()
        {
            tracing::error!("起動時の処理が異常終了しました");
        }
    }

    /// 閉じる。実行中のジョブは次回の起動で再開する。
    pub fn close(&self) {
        self.wait_ready();
        self.jobs.shutdown();
    }
}

impl Drop for LibrarySession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{ImportInput, ImportOptions, start_import};
    use crate::media::fake::FakeBackend;
    use std::time::Duration;

    fn sink() -> EventSink {
        Arc::new(|_| {})
    }

    fn backend() -> Arc<dyn MediaBackend> {
        Arc::new(FakeBackend)
    }

    #[test]
    fn create_then_reopen_reports_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Kura Library");
        {
            let session = LibrarySession::create(&path, sink(), backend()).unwrap();
            session.wait_ready();
            let info = session.info().unwrap();
            assert_eq!(info.name, "Kura Library");
            assert_eq!(info.item_count, 0);
            assert_eq!(info.integrity, Some(IntegrityReport::default()));
        }
        let path = dir.path().join("Kura Library.kuralib");
        std::fs::write(path.join("staging/leftover"), "x").unwrap();
        let session = LibrarySession::open(&path, sink(), backend()).unwrap();
        session.wait_ready();
        assert_eq!(session.integrity().unwrap().staging_removed, 1);
    }

    #[test]
    fn imports_through_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.jpg"), "a").unwrap();
        let session = LibrarySession::create(&dir.path().join("L"), sink(), backend()).unwrap();
        start_import(
            session.jobs(),
            &ImportInput {
                paths: vec![src],
                options: ImportOptions::default(),
            },
        )
        .unwrap();
        session.wait_ready();
        assert!(session.jobs().wait_idle(Duration::from_secs(10)));
        assert_eq!(session.info().unwrap().item_count, 1);
        // 取り込みのあとサムネイルまで作られる
        let state: String = session
            .library()
            .db()
            .read(|c| Ok(c.query_row("SELECT thumb_state FROM items", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(state, "ready");
    }
}
