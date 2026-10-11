//! 開いているライブラリとそのジョブ。開くときの手順（staging の掃除 → 整合性チェック → ジョブの再開）をまとめる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::Library;
use super::backup;
use super::integrity::{IntegrityReport, check_files, clean_staging};
use crate::Result;
use crate::capture::CaptureHandler;
use crate::download::{DownloadHandler, work_dir_name};
use crate::export::ExportHandler;
use crate::import::ImportHandler;
use crate::jobs::{JobKind, JobManager};
use crate::media::{MediaBackend, ThumbnailHandler};
use crate::playback::{PlaybackSupport, ProxyHandler, ProxyShared, ProxyTools, proxy};

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
    proxy: Arc<ProxyShared>,
    integrity: Arc<Mutex<Option<IntegrityReport>>>,
    /// DB の定期バックアップ（閉じると止める）。
    backup: Mutex<backup::Scheduler>,
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
        // ジョブを動かす前に書きかけを消す（再開する取り込みは最初からコピーし直す）。
        // 待機中・一時停止中のダウンロードの作業フォルダは、続きから取るので残す
        let keep: Vec<String> = library.db().read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id FROM jobs WHERE kind = 'download' AND state IN ('queued', 'running', 'paused')",
            )?;
            let ids = stmt.query_map([], |row| row.get::<_, i64>(0))?;
            Ok(ids
                .map(|id| id.map(work_dir_name))
                .collect::<std::result::Result<_, _>>()?)
        })?;
        let staging_removed = clean_staging(library.layout(), &keep)?;
        let proxy = Arc::new(ProxyShared::default());
        let jobs = Arc::new(JobManager::new(
            library.clone(),
            vec![
                (
                    JobKind::Import,
                    Arc::new(ImportHandler::new(backend.clone())),
                ),
                (
                    JobKind::Download,
                    Arc::new(DownloadHandler::new(backend.clone())),
                ),
                (
                    JobKind::Capture,
                    Arc::new(CaptureHandler::new(backend.clone())),
                ),
                (JobKind::Thumbnail, Arc::new(ThumbnailHandler::new(backend))),
                (JobKind::Export, Arc::new(ExportHandler)),
                (
                    JobKind::Proxy,
                    Arc::new(ProxyHandler::new(ProxyTools::detect(), proxy.clone())),
                ),
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

        let backup = Mutex::new(backup::Scheduler::start(library.layout().clone()));
        Ok(Self {
            library,
            jobs,
            proxy,
            integrity,
            backup,
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

    /// 再生可否の表を受け取る。前回と変わっていれば判定をやり直し、判定・作成を待つものがあればプロキシのジョブを積む。
    pub fn set_playback_support(&self, support: PlaybackSupport) -> Result<()> {
        let fingerprint = support.fingerprint();
        self.proxy.set_support(support);
        let todo = self
            .library
            .db()
            .write(move |tx| proxy::sync_support(tx, &fingerprint))?;
        if todo {
            self.jobs.ensure_queued(JobKind::Proxy, "{}")?;
        }
        Ok(())
    }

    /// 再生キューの今と次のアイテムのプロキシを先に作る。`force` なら、元のまま再生できなかったものを作成待ちに戻す。
    pub fn request_proxies(&self, ids: Vec<i64>, force: bool) -> Result<()> {
        self.proxy.prioritize(ids.clone());
        let todo = if force {
            self.library
                .db()
                .write(move |tx| proxy::force_pending(tx, &ids))?
        } else {
            self.library.db().read(|conn| proxy::any_todo(conn, &ids))?
        };
        if todo {
            self.jobs.ensure_queued(JobKind::Proxy, "{}")?;
        }
        Ok(())
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
        if let Ok(mut backup) = self.backup.lock() {
            backup.stop();
        }
    }

    /// いまのバックアップを作る（設定画面の「今すぐバックアップ」）。
    pub fn backup_now(&self) -> Result<backup::BackupInfo> {
        backup::create(self.library.layout(), crate::time::now_ms())
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
