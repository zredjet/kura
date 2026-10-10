//! OS の表示設定に関するコマンド。

use serde::Serialize;

use crate::AppError;

/// OS の表示設定。
#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct Appearance {
    /// 「透明度を下げる」がオンか。オンならガラスを不透明な面に置き換える。
    pub reduce_transparency: bool,
}

/// OS の表示設定を返す。AppKit を呼ぶので同期コマンド（メインスレッドで実行）にしている。
#[tauri::command]
#[specta::specta]
pub fn appearance_get() -> Result<Appearance, AppError> {
    let appearance = Appearance {
        reduce_transparency: kura_mac::reduce_transparency(),
    };
    tracing::debug!(?appearance, "appearance_get");
    Ok(appearance)
}
