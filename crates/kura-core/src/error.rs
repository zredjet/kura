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
    /// ファイル操作の失敗。
    #[error("ファイル操作に失敗しました: {0}")]
    Io(#[from] std::io::Error),
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
