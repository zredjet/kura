//! フロントへ返すエラー（`AppError`）。

use kura_core::{CoreError, UpgradeError};
use serde::{Deserialize, Serialize};

/// エラーの種類。フロントはこの値で分岐する（`docs/architecture.md` §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
pub enum AppErrorCode {
    NotFound,
    InvalidQuery,
    Invalid,
    ToolMissing,
    /// 外部ツールが失敗した。
    ToolFailed,
    Io,
    Db,
    Conflict,
    /// アプリ内部の想定外の失敗。
    Internal,
    /// ライブラリが新しい版の Kura で作られている（開けない。Kura を新しくする）。
    LibraryTooNew,
    /// ディスクの空きが足りず、ライブラリを新しい版にできない（当てる前の見積もり・バックアップ・当てる途中）。
    /// ライブラリは前の版のまま。
    NoSpace,
    /// ライブラリを新しい版にする前のバックアップを作れない（ライブラリは変えていない）。
    BackupFailed,
    /// ライブラリを新しい版にできない（戻した。ライブラリは前の版のまま）。
    UpgradeFailed,
}

/// コマンドが返すエラー。JSON では `{ code, message }` になる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct AppError {
    pub code: AppErrorCode,
    pub message: String,
}

impl AppError {
    /// 種類と説明からエラーを作る。
    pub fn new(code: AppErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 想定外の失敗。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(AppErrorCode::Internal, message)
    }
}

impl From<CoreError> for AppError {
    fn from(err: CoreError) -> Self {
        let code = match &err {
            CoreError::NotFound(_) => AppErrorCode::NotFound,
            CoreError::Conflict(_) => AppErrorCode::Conflict,
            CoreError::Invalid(_) => AppErrorCode::Invalid,
            CoreError::Io(_) => AppErrorCode::Io,
            CoreError::Db(_) | CoreError::Json(_) | CoreError::DbClosed => AppErrorCode::Db,
            CoreError::ToolMissing(_) => AppErrorCode::ToolMissing,
            CoreError::Tool(_) => AppErrorCode::ToolFailed,
            // ライブラリを新しい版にできなかった（種類でフロントが文言を選ぶ。説明は CoreError が持つ。docs/data-model.md §5）
            CoreError::Upgrade(err) => upgrade_code(err),
        };
        Self::new(code, err.to_string())
    }
}

/// ライブラリを新しい版にできなかった理由の種類。
fn upgrade_code(err: &UpgradeError) -> AppErrorCode {
    match err {
        UpgradeError::Newer { .. } => AppErrorCode::LibraryTooNew,
        UpgradeError::NoSpace { .. }
        | UpgradeError::BackupNoSpace { .. }
        | UpgradeError::ApplyNoSpace { .. } => AppErrorCode::NoSpace,
        UpgradeError::Backup { .. } => AppErrorCode::BackupFailed,
        UpgradeError::Apply { .. } => AppErrorCode::UpgradeFailed,
    }
}

impl From<tauri::Error> for AppError {
    fn from(err: tauri::Error) -> Self {
        Self::internal(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_maps_to_code() {
        let err: AppError = CoreError::NotFound("item 42".into()).into();
        assert_eq!(err.code, AppErrorCode::NotFound);
        assert_eq!(err.message, "見つかりません: item 42");

        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let err: AppError = CoreError::Io(io).into();
        assert_eq!(err.code, AppErrorCode::Io);

        let err: AppError = CoreError::Invalid("x".into()).into();
        assert_eq!(err.code, AppErrorCode::Invalid);
        let err: AppError = CoreError::DbClosed.into();
        assert_eq!(err.code, AppErrorCode::Db);
    }

    /// ライブラリを新しい版にできなかった理由は、フロントが文言を選べる種類と、分かる説明で返す。
    #[test]
    fn upgrade_errors_have_their_own_codes() {
        let cases = [
            (
                UpgradeError::Newer {
                    found: 12,
                    supported: 9,
                },
                AppErrorCode::LibraryTooNew,
                "新しい版の Kura",
            ),
            (
                UpgradeError::NoSpace {
                    needed: 300_000_000,
                    available: 1,
                },
                AppErrorCode::NoSpace,
                "空き容量",
            ),
            (
                UpgradeError::BackupNoSpace { detail: "x".into() },
                AppErrorCode::NoSpace,
                "空き容量",
            ),
            (
                UpgradeError::ApplyNoSpace {
                    migration: "0007_blobs_and_kinds".into(),
                    detail: "x".into(),
                },
                AppErrorCode::NoSpace,
                "前の版のまま",
            ),
            (
                UpgradeError::Backup { detail: "x".into() },
                AppErrorCode::BackupFailed,
                "バックアップ",
            ),
            (
                UpgradeError::Apply {
                    migration: "0007_blobs_and_kinds".into(),
                    detail: "x".into(),
                },
                AppErrorCode::UpgradeFailed,
                "0007_blobs_and_kinds",
            ),
        ];
        for (err, code, words) in cases {
            let app: AppError = CoreError::Upgrade(err).into();
            assert_eq!(app.code, code);
            assert!(app.message.contains(words), "{}", app.message);
        }
        let json = serde_json::to_value(AppErrorCode::NoSpace).unwrap();
        assert_eq!(json, serde_json::json!("NoSpace"));
    }

    #[test]
    fn serializes_as_code_and_message() {
        let err = AppError::new(AppErrorCode::Conflict, "同じ名前があります");
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "code": "Conflict", "message": "同じ名前があります" })
        );
    }
}
