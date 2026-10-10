//! アプリの状態（開いているライブラリとアプリ全体の設定）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use kura_core::library::{EventSink, LibrarySession};

use crate::config::{self, AppConfig};
use crate::error::{AppError, AppErrorCode};

/// Tauri の状態として持つもの。
pub struct AppState {
    session: RwLock<Option<Arc<LibrarySession>>>,
    config_path: PathBuf,
    config: Mutex<AppConfig>,
}

impl AppState {
    /// 設定ファイルの場所を受け取って作る。
    pub fn new(config_path: PathBuf) -> Self {
        let config = config::load(&config_path);
        Self {
            session: RwLock::new(None),
            config_path,
            config: Mutex::new(config),
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

    /// ライブラリを開く（作る）。前のライブラリは閉じ、場所を「前回のライブラリ」として保存する。
    pub fn replace_session(
        &self,
        open: impl FnOnce() -> kura_core::Result<LibrarySession>,
    ) -> Result<Arc<LibrarySession>, AppError> {
        self.close();
        let session = Arc::new(open()?);
        let root = session.library().layout().root().to_path_buf();
        if let Ok(mut slot) = self.session.write() {
            *slot = Some(session.clone());
        }
        self.remember(&root);
        Ok(session)
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
