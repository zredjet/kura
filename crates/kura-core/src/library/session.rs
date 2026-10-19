//! 開いているライブラリとそのジョブ。開くときの手順（staging の掃除 → 整合性チェック → ジョブの再開）をまとめる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::Library;
use super::backup;
use super::integrity::{IntegrityReport, check_files, clean_staging};
use super::ledger;
use super::upgrade::UpgradeHooks;
use crate::Result;
use crate::capture::CaptureHandler;
use crate::download::{DownloadHandler, work_dir_name};
use crate::edit::{ENGINE_WORK_DIR, EngineFactory, EngineServices};
use crate::export::ExportHandler;
use crate::import::ImportHandler;
use crate::jobs::{JobKind, JobManager};
use crate::media::{MediaBackend, ThumbnailHandler};
use crate::playback::{PlaybackSupport, ProxyHandler, ProxyShared, ProxyTools, proxy};

pub use crate::jobs::EventSink;

/// ライブラリを開くときにアプリから渡す、OS に依る実装（kura-core は OS を知らない。macOS は kura-mac で作る）。
#[derive(Clone)]
pub struct Services {
    /// メディア情報とサムネイル。
    pub media: Arc<dyn MediaBackend>,
    /// エンジンの部品（解析・エンジンのサムネイル・試聴の PCM・アニメの帯が使う実行の場とコーデックの組）を作る口。部品は
    /// 開いたライブラリごとに作り（[`LibrarySession::engine`]）、最初に使うときまで作らない。None ならエンジンを使わない。
    pub engine: Option<Arc<dyn EngineFactory>>,
    /// DB を新しい版にするときの、空き容量の確かめと始まり・終わり・失敗の知らせ。
    pub upgrade: UpgradeHooks,
}

impl Services {
    /// メディアの実装だけ（エンジンなし、空きを確かめず、知らせない）。
    pub fn new(media: Arc<dyn MediaBackend>) -> Self {
        Self {
            media,
            engine: None,
            upgrade: UpgradeHooks::default(),
        }
    }

    /// エンジンの部品を作る口を付ける。
    pub fn with_engine(mut self, factory: Arc<dyn EngineFactory>) -> Self {
        self.engine = Some(factory);
        self
    }

    /// DB を新しい版にするときの確かめと知らせを付ける。
    pub fn with_upgrade(mut self, hooks: UpgradeHooks) -> Self {
        self.upgrade = hooks;
        self
    }
}

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
    /// このライブラリのエンジンの部品（中身は最初に使うときに作る）。
    engine: Option<Arc<EngineServices>>,
    integrity: Arc<Mutex<Option<IntegrityReport>>>,
    /// DB の定期バックアップ（閉じると止める）。
    backup: Mutex<backup::Scheduler>,
    startup: Mutex<Option<JoinHandle<()>>>,
}

impl LibrarySession {
    /// 既存のライブラリを開く（新しい版にするときは `services.upgrade` で空きを確かめて知らせる）。
    pub fn open(path: &Path, sink: EventSink, services: Services) -> Result<Self> {
        let library = Library::open_with(path, &services.upgrade)?;
        Self::start(library, sink, services)
    }

    /// 新しいライブラリを作って開く。
    pub fn create(path: &Path, sink: EventSink, services: Services) -> Result<Self> {
        Self::start(Library::create(path)?, sink, services)
    }

    fn start(library: Library, sink: EventSink, services: Services) -> Result<Self> {
        let Services {
            media: backend,
            engine,
            ..
        } = services;
        let library = Arc::new(library);
        // エンジンの部品はこのライブラリのもの（ffmpeg の一時ファイルは staging/engine。下の掃除で前回の残りが消える）。
        // 実行の場とコーデックの組は、ジョブ・IPC が最初に使うときに作る（起動の道に載せない）
        let engine = engine.map(|factory| {
            Arc::new(EngineServices::new(
                factory,
                library.layout().staging_dir().join(ENGINE_WORK_DIR),
            ))
        });
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
                let checked = check_files(lib.db(), lib.layout()).and_then(|(missing, orphans)| {
                    Ok((missing, orphans, lib.db().read(ledger::unreferenced)?))
                });
                let report = match checked {
                    Ok((missing_files, orphan_files, unreferenced_blobs)) => IntegrityReport {
                        staging_removed,
                        missing_files,
                        orphan_files,
                        unreferenced_blobs,
                    },
                    Err(err) => {
                        tracing::warn!(%err, "整合性チェックに失敗しました");
                        IntegrityReport {
                            staging_removed,
                            ..IntegrityReport::default()
                        }
                    }
                };
                if report.missing_files > 0
                    || report.orphan_files > 0
                    || report.unreferenced_blobs > 0
                {
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
            engine,
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

    /// このライブラリのエンジンの部品（エンジンを使わないときは None）。ライブラリを開き直すと別物になる。
    pub fn engine(&self) -> Option<&Arc<EngineServices>> {
        self.engine.as_ref()
    }

    /// 外部ツールの場所が変わった：エンジンのコーデックの組を、次に使うときに作り直す（ffmpeg を探し直す）。
    pub fn reload_tools(&self) {
        if let Some(engine) = &self.engine {
            engine.reload_codecs();
        }
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
        // ffmpeg の一時ファイルの置き場が空なら消す（次に開くときの掃除で「書きかけ」に数えないように）。中身が残っていれば
        // 次に開くときの掃除で消す
        if let Some(engine) = &self.engine {
            match std::fs::remove_dir(engine.work_dir()) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    tracing::debug!(%err, "エンジンの一時ファイルの置き場を残しました");
                }
            }
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
    use crate::edit::services::fake::FakeEngineFactory;
    use crate::import::{ImportInput, ImportOptions, start_import};
    use crate::library::UpgradeEvent;
    use crate::media::fake::FakeBackend;
    use std::time::Duration;

    fn sink() -> EventSink {
        Arc::new(|_| {})
    }

    fn services() -> Services {
        Services::new(Arc::new(FakeBackend))
    }

    #[test]
    fn create_then_reopen_reports_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Kura Library");
        {
            let session = LibrarySession::create(&path, sink(), services()).unwrap();
            session.wait_ready();
            let info = session.info().unwrap();
            assert_eq!(info.name, "Kura Library");
            assert_eq!(info.item_count, 0);
            assert_eq!(info.integrity, Some(IntegrityReport::default()));
        }
        let path = dir.path().join("Kura Library.kuralib");
        std::fs::write(path.join("staging/leftover"), "x").unwrap();
        let session = LibrarySession::open(&path, sink(), services()).unwrap();
        session.wait_ready();
        assert_eq!(session.integrity().unwrap().staging_removed, 1);
    }

    #[test]
    fn imports_through_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.jpg"), "a").unwrap();
        let session = LibrarySession::create(&dir.path().join("L"), sink(), services()).unwrap();
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

    /// エンジンの部品はライブラリごとに作り、開いただけ（整合性チェック・ジョブの開始まで）では中身を作らない。
    /// ライブラリを切り替えると別物になり、ffmpeg の一時ファイルはそのライブラリの staging/engine に置く。
    #[test]
    fn the_engine_is_per_library_and_built_on_first_use() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(FakeEngineFactory::default());
        let a = LibrarySession::create(
            &dir.path().join("A"),
            sink(),
            services().with_engine(factory.clone()),
        )
        .unwrap();
        a.wait_ready();
        assert!(a.jobs().wait_idle(Duration::from_secs(10)));
        let engine_a = a.engine().unwrap().clone();
        assert_eq!((factory.runtimes(), factory.codec_builds()), (0, 0));
        assert!(engine_a.runtime_if_built().is_none());
        assert!(engine_a.codecs_if_built().is_none());
        let staging_a = dir.path().join("A.kuralib/staging/engine");
        assert_eq!(engine_a.work_dir(), staging_a);

        engine_a.runtime().unwrap();
        engine_a.codecs();
        assert_eq!((factory.runtimes(), factory.codec_builds()), (1, 1));
        drop(a);

        let b = LibrarySession::create(
            &dir.path().join("B"),
            sink(),
            services().with_engine(factory.clone()),
        )
        .unwrap();
        b.wait_ready();
        let engine_b = b.engine().unwrap();
        assert!(!Arc::ptr_eq(&engine_a, engine_b));
        assert!(engine_b.runtime_if_built().is_none());
        assert!(engine_b.codecs_if_built().is_none());
        engine_b.codecs();
        assert_eq!(
            factory.work_dirs(),
            [staging_a, dir.path().join("B.kuralib/staging/engine")]
        );
        drop(b);

        // 同じライブラリを開き直しても、前の部品は使わない
        let again = LibrarySession::open(
            &dir.path().join("A.kuralib"),
            sink(),
            services().with_engine(factory.clone()),
        )
        .unwrap();
        assert!(!Arc::ptr_eq(&engine_a, again.engine().unwrap()));
        assert!(again.engine().unwrap().runtime_if_built().is_none());
    }

    /// 外部ツールの場所を変えたら、開いているライブラリのコーデックの組を作り直す（ffmpeg を探し直す）。
    #[test]
    fn reloading_tools_rebuilds_the_codec_set() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(FakeEngineFactory::default());
        let session = LibrarySession::create(
            &dir.path().join("L"),
            sink(),
            services().with_engine(factory.clone()),
        )
        .unwrap();
        let engine = session.engine().unwrap();
        let runtime = engine.runtime().unwrap().clone();
        let before = engine.codecs();
        session.reload_tools();
        let after = engine.codecs();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(factory.codec_builds(), 2);
        assert!(Arc::ptr_eq(engine.runtime().unwrap(), &runtime));
    }

    #[test]
    fn without_a_factory_there_is_no_engine() {
        let dir = tempfile::tempdir().unwrap();
        let session = LibrarySession::create(&dir.path().join("L"), sink(), services()).unwrap();
        assert!(session.engine().is_none());
        session.reload_tools();
    }

    /// ffmpeg の一時ファイルの置き場は、閉じるときに空なら消し（次に開くときに書きかけと数えない）、残っていれば次に開くときの
    /// 掃除で消す。
    #[test]
    fn the_engine_work_dir_is_removed_when_empty_and_cleaned_when_left() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(FakeEngineFactory::default());
        let services = || services().with_engine(factory.clone());
        let path = dir.path().join("L.kuralib");
        let session = LibrarySession::create(&path, sink(), services()).unwrap();
        let work = session.engine().unwrap().work_dir().to_path_buf();
        std::fs::create_dir_all(&work).unwrap();
        session.close();
        assert!(!work.exists());
        drop(session);

        let session = LibrarySession::open(&path, sink(), services()).unwrap();
        session.wait_ready();
        assert_eq!(session.integrity().unwrap().staging_removed, 0);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("ffmpeg-1.wav"), "x").unwrap();
        drop(session);
        assert!(work.join("ffmpeg-1.wav").exists());

        let session = LibrarySession::open(&path, sink(), services()).unwrap();
        session.wait_ready();
        assert_eq!(session.integrity().unwrap().staging_removed, 1);
        assert!(!work.exists());
    }

    /// 開くときに、新しい版にするための確かめと知らせを Library に渡す。
    #[test]
    fn opening_passes_the_upgrade_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        std::fs::create_dir_all(&root).unwrap();
        {
            let mut conn = rusqlite::Connection::open(root.join(crate::library::DB_FILE)).unwrap();
            crate::db::apply_pragmas(&conn).unwrap();
            crate::db::migrate_until(&mut conn, crate::db::MIGRATIONS.len() - 1).unwrap();
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let hooks = UpgradeHooks {
            free_space: None,
            notify: Some(Arc::new(move |e: &UpgradeEvent| {
                seen.lock().unwrap().push(e.clone());
            })),
        };
        let session = LibrarySession::open(&root, sink(), services().with_upgrade(hooks)).unwrap();
        let events = events.lock().unwrap();
        assert!(
            matches!(
                events.as_slice(),
                [UpgradeEvent::Started { .. }, UpgradeEvent::Finished { .. }]
            ),
            "{events:?}"
        );
        assert!(session.library().migration().upgraded());
    }
}
