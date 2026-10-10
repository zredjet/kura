//! フロントへ返すエラー（`AppError`）。

use kura_core::CoreError;
use serde::Serialize;

/// エラーの種類。フロントはこの値で分岐する（`docs/architecture.md` §3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
pub enum AppErrorCode {
    NotFound,
    InvalidQuery,
    ToolMissing,
    Io,
    Db,
    Conflict,
}

/// コマンドが返すエラー。JSON では `{ code, message }` になる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type, thiserror::Error)]
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
}

impl From<CoreError> for AppError {
    fn from(err: CoreError) -> Self {
        let code = match &err {
            CoreError::NotFound(_) => AppErrorCode::NotFound,
            CoreError::Conflict(_) => AppErrorCode::Conflict,
            CoreError::Io(_) => AppErrorCode::Io,
        };
        Self::new(code, err.to_string())
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
