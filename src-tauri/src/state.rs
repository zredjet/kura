//! アプリの状態（開いているライブラリとアプリ全体の設定）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use kura_core::capture::LoginBrowser;
use kura_core::edit::EngineFactory;
use kura_core::library::{
    EventSink, LibrarySession, Services, UpgradeEvent, UpgradeHooks, upgrade::UpgradeNotifyFn,
};
use kura_core::playback::PlaybackSupport;

use crate::config::{self, AppConfig, DownloadPrefs, PlayerPrefs};
use crate::error::{AppError, AppErrorCode};

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
        }
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

    /// ライブラリを開く（作る）。前のライブラリは閉じ、`remember` なら場所を「前回のライブラリ」として保存する。
    pub fn replace_session(
        &self,
        open: impl FnOnce() -> kura_core::Result<LibrarySession>,
        remember: bool,
    ) -> Result<Arc<LibrarySession>, AppError> {
        self.close();
        let session = Arc::new(open()?);
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

    /// 外部ツールの場所を変えると（tools_set_path）、開いているライブラリのエンジンのコーデックの組を作り直す。
    #[test]
    fn setting_a_tool_path_rebuilds_the_engine_codecs() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(Counting::default());
        let state = AppState::with_engine(dir.path().join("app.json"), factory.clone());
        let session = state
            .replace_session(
                || {
                    LibrarySession::create(
                        &dir.path().join("L"),
                        Arc::new(|_| {}),
                        Services::new(Arc::new(NoBackend)).with_engine(factory.clone()),
                    )
                },
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
}
