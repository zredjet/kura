//! kura-core 共通のエラー型。

/// kura-core の処理で起きるエラー。Tauri 境界で `AppError` に変換される。
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// 指定したものが見つからない。
    #[error("見つかりません: {0}")]
    NotFound(String),
    /// 既存のものと競合する（同名・同一など）。
    #[error("競合しています: {0}")]
    Conflict(String),
    /// 入力が正しくない。
    #[error("入力が正しくありません: {0}")]
    Invalid(String),
    /// ファイル操作の失敗。
    #[error("ファイル操作に失敗しました: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite の失敗。
    #[error("データベースの操作に失敗しました: {0}")]
    Db(#[from] rusqlite::Error),
    /// JSON の読み書きの失敗（ジョブの入力・結果など）。
    #[error("データの読み書きに失敗しました: {0}")]
    Json(#[from] serde_json::Error),
    /// DB の書き込みスレッドが止まっている。
    #[error("データベースが閉じられています")]
    DbClosed,
}

impl CoreError {
    /// エラーの種類を表す短い名前（jobs.error_code・AppError.code に使う）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "NotFound",
            Self::Conflict(_) => "Conflict",
            Self::Invalid(_) => "Invalid",
            Self::Io(_) => "Io",
            Self::Db(_) | Self::Json(_) | Self::DbClosed => "Db",
        }
    }
}

impl From<walkdir::Error> for CoreError {
    fn from(err: walkdir::Error) -> Self {
        Self::Io(err.into())
    }
}

/// kura-core の `Result`。
pub type Result<T, E = CoreError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    fn open_missing() -> Result<()> {
        std::fs::File::open("/kura-core-test/does-not-exist")?;
        Ok(())
    }

    #[test]
    fn io_error_converts_with_question_mark() {
        let err = open_missing().unwrap_err();
        assert!(matches!(err, CoreError::Io(ref e) if e.kind() == std::io::ErrorKind::NotFound));
    }

    #[test]
    fn message_includes_detail() {
        let err = CoreError::NotFound("item 42".into());
        assert_eq!(err.to_string(), "見つかりません: item 42");
    }
}
