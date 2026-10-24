//! アプリの状態（開いているライブラリとアプリ全体の設定）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use kura_core::capture::LoginBrowser;
use kura_core::edit::EngineFactory;
use kura_core::library::{
    EventSink, LibraryLock, LibrarySession, Services, UpgradeEvent, UpgradeHooks, is_library,
    upgrade::UpgradeNotifyFn,
};
use kura_core::playback::PlaybackSupport;

use crate::config::{self, AppConfig, DownloadPrefs, PlayerPrefs};
use crate::error::{AppError, AppErrorCode};

/// 開く・作るライブラリ（[`AppState::replace_session`]）。
#[derive(Debug, Clone, Copy)]
pub enum LibraryTarget<'a> {
    /// 既存のライブラリを開く。
    Open(&'a Path),
    /// 新しいライブラリを作って開く。
    Create(&'a Path),
}

/// Tauri の状態として持つもの。
pub struct AppState {
    session: RwLock<Option<Arc<LibrarySession>>>,
    config_path: PathBuf,
    config: Mutex<AppConfig>,
    /// フロントが起動時に作る再生可否の表（ライブラリを開き直したら渡し直す）。
    support: RwLock<Option<PlaybackSupport>>,
    /// アプリが開いたログイン用の Chrome（DevTools でつないでいる。ADR-111）。
    login: Arc<Mutex<Option<LoginBrowser>>>,
    /// エンジンの部品を作る口。ライブラリを開くたびに渡し、ライブラリごとの部品を作らせる。
    engine: Arc<dyn EngineFactory>,
    /// ライブラリを開く・作る・戻すのを 1 つずつにする鍵（同じライブラリを並んで開くと、新しい版にする前のバックアップが
    /// ぶつかって両方失敗する。起動の restore と開く操作が重なっても、2 回目は当て終えた版を開く）。
    opening: Mutex<()>,
}

impl AppState {
    /// 設定ファイルの場所を受け取って作る（エンジンの部品は macOS の実装で作る）。
    pub fn new(config_path: PathBuf) -> Self {
        Self::with_engine(config_path, Arc::new(crate::engine::MacEngineFactory))
    }

    /// 設定ファイルの場所と、エンジンの部品を作る口から作る。
    pub fn with_engine(config_path: PathBuf, engine: Arc<dyn EngineFactory>) -> Self {
        let config = config::load(&config_path);
        kura_core::tools::set_overrides(config.tools.clone());
        Self {
            session: RwLock::new(None),
            config_path,
            config: Mutex::new(config),
            support: RwLock::new(None),
            login: Arc::new(Mutex::new(None)),
            engine,
            opening: Mutex::new(()),
        }
    }

    /// ライブラリを開く・作る・戻す間持つ鍵（[`AppState::replace_session`] は自分で取る。restore のように「開いているか」を
    /// 確かめてから開くときは、これを持ったまま [`AppState::replace_session_locked`] を呼ぶ）。
    pub fn opening(&self) -> MutexGuard<'_, ()> {
        self.opening.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// ライブラリを開く・作るときに渡すもの：メディアの実装（macOS と ffmpeg）、エンジンの部品を作る口、DB を新しい版に
    /// するときの空きの確かめ（kura-mac の `volume_capacity`）とイベント `library://upgrade`。
    pub fn services(&self, app: &tauri::AppHandle) -> Services {
        Services::new(kura_mac::media_backend())
            .with_engine(self.engine.clone())
            .with_upgrade(UpgradeHooks {
                free_space: Some(Arc::new(|path: &Path| {
                    kura_mac::volume_capacity(path).map(|v| v.available_bytes)
                })),
                notify: Some(upgrade_notify(app.clone())),
            })
    }

    /// ログイン用の Chrome の置き場所。
    pub fn login_slot(&self) -> Arc<Mutex<Option<LoginBrowser>>> {
        self.login.clone()
    }

    /// ログイン用の Chrome を閉じる（アプリを終えるとき。DevTools のポートを開いたままにしない）。
    pub fn close_login(&self) {
        let browser = self.login.lock().ok().and_then(|mut slot| slot.take());
        drop(browser);
    }

    /// 開いているライブラリ。なければ NotFound。
    pub fn session(&self) -> Result<Arc<LibrarySession>, AppError> {
        self.session
            .read()
            .map_err(|_| AppError::internal("状態を読めません"))?
            .clone()
            .ok_or_else(|| AppError::new(AppErrorCode::NotFound, "ライブラリが開かれていません"))
    }

    /// 開いているライブラリ（なければ None）。
    pub fn current(&self) -> Option<Arc<LibrarySession>> {
        self.session.read().ok().and_then(|s| s.clone())
    }

    /// 前回開いたライブラリ。
    pub fn last_library(&self) -> Option<PathBuf> {
        self.config.lock().ok().and_then(|c| c.last_library.clone())
    }

    /// ライブラリを開く（作る）。前のライブラリは閉じ、`remember` なら場所を「前回のライブラリ」として保存する。開く・作る・
    /// 戻すは 1 つずつ（[`AppState::opening`] の鍵）。別のライブラリへ切り替えるときは、前のライブラリを閉じる前に開く先の
    /// ロックを取り、ほかのアプリが開いていれば前のライブラリを残したまま `LibraryInUse` を返す（M12）。
    pub fn replace_session(
        &self,
        target: LibraryTarget<'_>,
        sink: EventSink,
        services: Services,
        remember: bool,
    ) -> Result<Arc<LibrarySession>, AppError> {
        let _opening = self.opening();
        self.replace_session_locked(target, sink, services, remember)
    }

    /// [`AppState::replace_session`] の、鍵（[`AppState::opening`]）を持っている呼び手のためのもの。
    pub fn replace_session_locked(
        &self,
        target: LibraryTarget<'_>,
        sink: EventSink,
        services: Services,
        remember: bool,
    ) -> Result<Arc<LibrarySession>, AppError> {
        let session = match target {
            LibraryTarget::Open(path) => {
                // 閉じる前に取る（取れなければ、前のライブラリを閉じずに断る）
                let lock = self.lock_before_switching(path)?;
                self.close();
                match lock {
                    Some(lock) => LibrarySession::open_locked(lock, sink, services)?,
                    None => LibrarySession::open(path, sink, services)?,
                }
            }
            // 作るのはまだない場所なので、ほかのアプリが開いていることはない（作るのに失敗すれば前のライブラリは閉じている。
            // フロントは初回の画面に戻って理由を出す）
            LibraryTarget::Create(path) => {
                self.close();
                LibrarySession::create(path, sink, services)?
            }
        };
        let session = Arc::new(session);
        let root = session.library().layout().root().to_path_buf();
        if let Ok(mut slot) = self.session.write() {
            *slot = Some(session.clone());
        }
        if remember {
            self.remember(&root);
        }
        if let Some(support) = self.support.read().ok().and_then(|s| s.clone())
            && let Err(err) = session.set_playback_support(support)
        {
            tracing::warn!(%err, "再生可否の表をライブラリに渡せませんでした");
        }
        Ok(session)
    }

    /// 再生可否の表を受け取り、開いているライブラリにも渡す。
    pub fn set_playback_support(&self, support: PlaybackSupport) -> Result<(), AppError> {
        if let Ok(mut slot) = self.support.write() {
            *slot = Some(support.clone());
        }
        if let Some(session) = self.current() {
            session.set_playback_support(support)?;
        }
        Ok(())
    }

    /// 再生の設定。
    pub fn player_prefs(&self) -> PlayerPrefs {
        self.config
            .lock()
            .map(|c| c.player.clone())
            .unwrap_or_default()
    }

    /// 再生の設定を変えて保存する。
    pub fn set_player_prefs(&self, prefs: PlayerPrefs) -> Result<PlayerPrefs, AppError> {
        let prefs = prefs.clamped();
        let mut config = self
            .config
            .lock()
            .map_err(|_| AppError::internal("設定を読めません"))?;
        config.player = prefs.clone();
        config::save(&self.config_path, &config)
            .map_err(|err| AppError::internal(format!("設定を保存できませんでした: {err}")))?;
        Ok(prefs)
    }

    /// ダウンロードの設定。
    pub fn download_prefs(&self) -> DownloadPrefs {
        self.config
            .lock()
            .map(|c| c.download.clone())
            .unwrap_or_default()
    }

    /// ダウンロードの設定を変えて保存する（規則は確かめてから）。
    pub fn set_download_prefs(&self, prefs: DownloadPrefs) -> Result<DownloadPrefs, AppError> {
        for rule in &prefs.rules {
            kura_core::download::validate_rule(rule)?;
        }
        self.update_config(|c| c.download = prefs.clone())?;
        Ok(prefs)
    }

    /// キャプチャ用の Chrome のプロファイル（アプリの設定のフォルダの中。docs/architecture.md §10）。
    pub fn capture_profile_dir(&self) -> PathBuf {
        let base = self
            .config_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        kura_core::capture::profile_dir(&base)
    }

    /// 外部ツールの場所を指定する（None で指定をやめる）。
    pub fn set_tool_path(&self, tool: &str, path: Option<PathBuf>) -> Result<(), AppError> {
        let tools = self.update_config(|c| {
            match &path {
                Some(path) => c.tools.insert(tool.to_string(), path.clone()),
                None => c.tools.remove(tool),
            };
            c.tools.clone()
        })?;
        kura_core::tools::set_overrides(tools);
        // 開いているライブラリのエンジンのコーデックの組を、次に使うときに作り直す（ffmpeg を探し直す）
        if let Some(session) = self.current() {
            session.reload_tools();
        }
        Ok(())
    }

    /// 設定を変えて保存する。
    fn update_config<T>(&self, change: impl FnOnce(&mut AppConfig) -> T) -> Result<T, AppError> {
        let mut config = self
            .config
            .lock()
            .map_err(|_| AppError::internal("設定を読めません"))?;
        let out = change(&mut config);
        config::save(&self.config_path, &config)
            .map_err(|err| AppError::internal(format!("設定を保存できませんでした: {err}")))?;
        Ok(out)
    }

    /// 別のライブラリへ切り替える前に、開く先 `path` のライブラリのロックを取る（取れなければ `LibraryInUse` などで、前の
    /// ライブラリはそのまま）。何も開いていないとき（開く処理が取る）、開いているのと同じライブラリのとき（場所の書き方が
    /// 違っても。閉じれば自分のロックが外れる）、ライブラリでない場所（開く処理が NotFound で断る。ロックファイルを作らない）は
    /// 取らずに None。
    fn lock_before_switching(&self, path: &Path) -> Result<Option<LibraryLock>, AppError> {
        let Some(current) = self.current() else {
            return Ok(None);
        };
        if !is_library(path) || current.library().lock().is_for(path) {
            return Ok(None);
        }
        Ok(Some(LibraryLock::acquire(path)?))
    }

    /// 開いているライブラリを閉じる（実行中のジョブは次回の起動で再開する）。
    pub fn close(&self) {
        let previous = self.session.write().ok().and_then(|mut s| s.take());
        if let Some(previous) = previous {
            previous.close();
        }
    }

    fn remember(&self, root: &Path) {
        let Ok(mut config) = self.config.lock() else {
            return;
        };
        config.last_library = Some(root.to_path_buf());
        if let Err(err) = config::save(&self.config_path, &config) {
            tracing::warn!(%err, "アプリの設定を保存できませんでした");
        }
    }
}

/// DB を新しい版にする知らせを、イベント `library://upgrade` にしてフロントへ送る。
pub fn upgrade_notify(app: tauri::AppHandle) -> Arc<UpgradeNotifyFn> {
    Arc::new(move |event: &UpgradeEvent| {
        let payload = crate::dto::LibraryUpgradeEvent::from(event);
        if let Err(err) = tauri_specta::Event::emit(&payload, &app) {
            tracing::warn!(%err, "ライブラリの更新の知らせを送れませんでした");
        }
    })
}

/// 進捗イベントの送り先を作る。
pub fn event_sink(app: tauri::AppHandle) -> EventSink {
    Arc::new(move |event| {
        let payload = crate::dto::JobProgress::from(event);
        if let Err(err) = tauri_specta::Event::emit(&payload, &app) {
            tracing::warn!(%err, "進捗イベントを送れませんでした");
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use kura_core::edit::{CodecSet, RuntimeConfig};
    use kura_core::media::NoBackend;

    use super::*;

    /// コーデックの組を作った回数を数える偽のファクトリ。
    #[derive(Default)]
    struct Counting(AtomicUsize);

    impl EngineFactory for Counting {
        fn runtime_config(&self) -> RuntimeConfig {
            RuntimeConfig {
                cores: Some(2),
                ..RuntimeConfig::default()
            }
        }

        fn codecs(&self, _work_dir: &Path) -> CodecSet {
            self.0.fetch_add(1, Ordering::SeqCst);
            CodecSet::default()
        }
    }

    /// 1 つ前の版（最後のマイグレーション 0010 を当てる前）のライブラリを作る。
    fn library_before_last_migration(root: &Path) {
        let library = kura_core::library::Library::create(root).unwrap();
        let before = kura_core::db::MIGRATIONS.len() - 1;
        library
            .db()
            .write(move |tx| {
                tx.execute_batch(&format!(
                    "DROP TRIGGER items_blob_id_ai; DROP TRIGGER items_blob_id_au; DROP INDEX idx_items_blob;
                     ALTER TABLE items DROP COLUMN blob_id; PRAGMA user_version = {before};"
                ))?;
                Ok(())
            })
            .unwrap();
    }

    /// 同じライブラリを並んで開いても（起動の restore と開く操作が重なるなど）、開く・作る・戻すは 1 つの鍵で順に行うので、
    /// 新しい版にするのと、その前のバックアップは 1 回だけ（2 回目は当て終えた版を開く）。
    #[test]
    fn opening_the_same_library_in_parallel_upgrades_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("L.kuralib");
        library_before_last_migration(&root);
        let state =
            AppState::with_engine(dir.path().join("app.json"), Arc::new(Counting::default()));
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = events.clone();
        let notify: Arc<UpgradeNotifyFn> = Arc::new(move |e: &UpgradeEvent| {
            let name = match e {
                UpgradeEvent::Started { .. } => "started",
                UpgradeEvent::Finished { .. } => "finished",
                UpgradeEvent::Failed { .. } => "failed",
            };
            seen.lock().unwrap().push(name.to_string());
        });
        let services = || {
            Services::new(Arc::new(NoBackend)).with_upgrade(UpgradeHooks {
                free_space: None,
                notify: Some(notify.clone()),
            })
        };
        let barrier = std::sync::Barrier::new(2);
        let upgraded: Vec<bool> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        barrier.wait();
                        let session = state
                            .replace_session(
                                LibraryTarget::Open(&root),
                                Arc::new(|_| {}),
                                services(),
                                false,
                            )
                            .unwrap();
                        session.library().migration().upgraded()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["started", "finished"],
            "新しい版にするのは 1 回だけ"
        );
        assert_eq!(upgraded.iter().filter(|u| **u).count(), 1);
        state.close();
    }

    /// 外部ツールの場所を変えると（tools_set_path）、開いているライブラリのエンジンのコーデックの組を作り直す。
    #[test]
    fn setting_a_tool_path_rebuilds_the_engine_codecs() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(Counting::default());
        let state = AppState::with_engine(dir.path().join("app.json"), factory.clone());
        let session = state
            .replace_session(
                LibraryTarget::Create(&dir.path().join("L")),
                Arc::new(|_| {}),
                Services::new(Arc::new(NoBackend)).with_engine(factory.clone()),
                false,
            )
            .unwrap();
        let engine = session.engine().unwrap().clone();
        let before = engine.codecs();
        assert_eq!(factory.0.load(Ordering::SeqCst), 1);

        let tool = dir.path().join("mpv");
        std::fs::write(&tool, "").unwrap();
        state.set_tool_path("mpv", Some(tool)).unwrap();
        let after = engine.codecs();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(factory.0.load(Ordering::SeqCst), 2);
        state.set_tool_path("mpv", None).unwrap();
        state.close();
    }

    /// 別のライブラリへ切り替えるとき、ほかのアプリが開いていれば（ロックを持っていれば）`LibraryInUse` で断り、今の
    /// ライブラリは開いたまま使える。放されれば切り替えられ、前のライブラリのロックは閉じたときに外れる（握った Arc が残って
    /// いても）。開いているのと同じライブラリは、場所の書き方が違っても開き直せる。
    #[test]
    fn switching_to_a_library_held_elsewhere_keeps_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("A.kuralib"), dir.path().join("B.kuralib"));
        drop(kura_core::library::Library::create(&b).unwrap());
        let state =
            AppState::with_engine(dir.path().join("app.json"), Arc::new(Counting::default()));
        let services = || Services::new(Arc::new(NoBackend));
        let open = |target| state.replace_session(target, Arc::new(|_| {}), services(), true);
        let first = open(LibraryTarget::Create(&a)).unwrap();
        first.wait_ready();

        // ほかのアプリの代わりに、同じプロセスで B を開いておく
        let other = kura_core::library::Library::open(&b).unwrap();
        let err = open(LibraryTarget::Open(&b)).err().unwrap();
        assert_eq!(err.code, AppErrorCode::LibraryInUse);
        let current = state.session().unwrap();
        assert!(Arc::ptr_eq(&current, &first), "今のライブラリが残る");
        assert_eq!(current.info().unwrap().item_count, 0, "DB も開いたまま");
        assert_eq!(state.last_library().as_deref(), Some(a.as_path()));

        // 同じライブラリを別の書き方（シンボリックリンク）で開き直すのは、自分のロックに当たらない
        let link = dir.path().join("link.kuralib");
        std::os::unix::fs::symlink(&a, &link).unwrap();
        let reopened = open(LibraryTarget::Open(&link)).unwrap();
        assert!(!Arc::ptr_eq(&reopened, &first));
        reopened.wait_ready();

        drop(other);
        let switched = open(LibraryTarget::Open(&b)).unwrap();
        switched.wait_ready();
        assert_eq!(state.last_library().as_deref(), Some(b.as_path()));
        // 前のライブラリ（A）は閉じてロックが外れている（Arc を握っていても）
        drop(kura_core::library::Library::open(&a).unwrap());
        drop((first, reopened));
        state.close();
    }
}
