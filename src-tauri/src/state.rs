//! アプリの状態（開いているライブラリとアプリ全体の設定）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use kura_core::library::{EventSink, LibrarySession};
use kura_core::playback::PlaybackSupport;

use crate::config::{self, AppConfig, PlayerPrefs};
use crate::error::{AppError, AppErrorCode};

/// Tauri の状態として持つもの。
pub struct AppState {
    session: RwLock<Option<Arc<LibrarySession>>>,
    config_path: PathBuf,
    config: Mutex<AppConfig>,
    /// フロントが起動時に作る再生可否の表（ライブラリを開き直したら渡し直す）。
    support: RwLock<Option<PlaybackSupport>>,
}

impl AppState {
    /// 設定ファイルの場所を受け取って作る。
    pub fn new(config_path: PathBuf) -> Self {
        let config = config::load(&config_path);
        Self {
            session: RwLock::new(None),
            config_path,
            config: Mutex::new(config),
            support: RwLock::new(None),
        }
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

/// 進捗イベントの送り先を作る。
pub fn event_sink(app: tauri::AppHandle) -> EventSink {
    Arc::new(move |event| {
        let payload = crate::dto::JobProgress::from(event);
        if let Err(err) = tauri_specta::Event::emit(&payload, &app) {
            tracing::warn!(%err, "進捗イベントを送れませんでした");
        }
    })
}
