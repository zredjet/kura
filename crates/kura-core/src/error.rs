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
    /// 外部ツールが見つからない（名前）。
    #[error("{0} が見つかりません。外部ツールの欄の案内に従って入れてください")]
    ToolMissing(String),
    /// 外部ツールが失敗した（画面に出す説明）。
    #[error("{0}")]
    Tool(String),
    /// ライブラリの DB を新しい版にできなかった（開くのを失敗にする。画面に出す説明）。
    #[error("{0}")]
    Upgrade(#[from] UpgradeError),
}

/// ライブラリの DB を新しい版にできなかった理由（docs/data-model.md §5）。どれでもライブラリは前の版のまま残る。
#[derive(Debug, Clone, thiserror::Error)]
pub enum UpgradeError {
    /// DB の方が新しい（新しい版の Kura で作られた）。
    #[error(
        "このライブラリは新しい版の Kura で作られています（スキーマ {found}。この Kura が扱えるのは {supported} まで）。Kura を新しくしてから開いてください"
    )]
    Newer { found: usize, supported: usize },
    /// 当てる前に確かめた空きが、見積もり（`library::upgrade::space_needed`）に足りない（バックアップも取っていない）。
    #[error(
        "ディスクの空き容量が足りないため、ライブラリを新しい版にできません。あと約 {} MB の空きを作ってから開き直してください（ライブラリは変えていません。要る空きは約 {} MB、いまの空きは約 {} MB）",
        megabytes(.needed.saturating_sub(*.available)),
        megabytes(*.needed),
        megabytes(*.available)
    )]
    NoSpace { needed: u64, available: u64 },
    /// 当てる前のバックアップを作れなかった（ディスクの空きが足りない）。
    #[error(
        "ライブラリを新しい版にする前のバックアップを作れませんでした。ディスクの空き容量が足りません。空きを作ってから開き直してください（ライブラリは変えていません。{detail}）"
    )]
    BackupNoSpace { detail: String },
    /// 当てる前のバックアップを作れなかった。
    #[error(
        "ライブラリを新しい版にする前のバックアップを作れませんでした（ライブラリは変えていません。{detail}）"
    )]
    Backup { detail: String },
    /// 当てている途中でディスクの空きが足りなくなった。
    #[error(
        "ディスクの空き容量が足りず、ライブラリを新しい版にできませんでした。空きを作ってから開き直してください（ライブラリは前の版のまま残っています。{migration}：{detail}）"
    )]
    ApplyNoSpace { migration: String, detail: String },
    /// 当てられなかった（戻した）。
    #[error(
        "ライブラリを新しい版にできませんでした（ライブラリは前の版のまま残っています。{migration}：{detail}）"
    )]
    Apply { migration: String, detail: String },
}

impl UpgradeError {
    /// 理由の種類（`newer`・`noSpace`・`backup`・`apply`。画面の出し分けに使う）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Newer { .. } => "newer",
            Self::NoSpace { .. } | Self::BackupNoSpace { .. } | Self::ApplyNoSpace { .. } => {
                "noSpace"
            }
            Self::Backup { .. } => "backup",
            Self::Apply { .. } => "apply",
        }
    }

    /// 当てる前のバックアップの失敗から作る。
    pub fn backup(err: &CoreError) -> Self {
        let detail = err.to_string();
        if err.is_no_space() {
            Self::BackupNoSpace { detail }
        } else {
            Self::Backup { detail }
        }
    }

    /// 当てる失敗から作る（`migration` は失敗したマイグレーションの名前）。
    pub fn apply(migration: &str, err: &CoreError) -> Self {
        let (migration, detail) = (migration.to_string(), err.to_string());
        if err.is_no_space() {
            Self::ApplyNoSpace { migration, detail }
        } else {
            Self::Apply { migration, detail }
        }
    }
}

/// バイト数を、切り上げた MB（10^6。Finder の表示と同じ）にする。
fn megabytes(bytes: u64) -> u64 {
    bytes.div_ceil(1_000_000)
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
            Self::ToolMissing(_) => "ToolMissing",
            Self::Tool(_) => "ToolFailed",
            Self::Upgrade(_) => "Upgrade",
        }
    }

    /// ディスクの空きが足りないことによる失敗か（ENOSPC・SQLITE_FULL）。
    pub fn is_no_space(&self) -> bool {
        /// macOS と Linux の ENOSPC。
        const ENOSPC: i32 = 28;
        match self {
            Self::Io(err) => {
                err.kind() == std::io::ErrorKind::StorageFull || err.raw_os_error() == Some(ENOSPC)
            }
            Self::Db(rusqlite::Error::SqliteFailure(err, _)) => {
                err.code == rusqlite::ErrorCode::DiskFull
            }
            _ => false,
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

    #[test]
    fn upgrade_errors_say_what_happened_and_tell_no_space_apart() {
        let full = CoreError::Io(std::io::Error::from(std::io::ErrorKind::StorageFull));
        assert!(full.is_no_space());
        let enospc = CoreError::Io(std::io::Error::from_raw_os_error(28));
        assert!(enospc.is_no_space());
        let disk_full = CoreError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        ));
        assert!(disk_full.is_no_space());
        let other = CoreError::Invalid("x".into());
        assert!(!other.is_no_space());

        let backup = UpgradeError::backup(&full);
        assert_eq!(backup.kind(), "noSpace");
        assert!(backup.to_string().contains("空き容量"), "{backup}");
        assert_eq!(UpgradeError::backup(&other).kind(), "backup");
        let apply = UpgradeError::apply("0007_blobs_and_kinds", &other);
        assert_eq!(apply.kind(), "apply");
        assert!(
            apply.to_string().contains("0007_blobs_and_kinds"),
            "{apply}"
        );
        assert!(apply.to_string().contains("前の版のまま"), "{apply}");
        let short = UpgradeError::NoSpace {
            needed: 300_000_000,
            available: 120_500_000,
        };
        assert_eq!(short.kind(), "noSpace");
        assert!(short.to_string().contains("あと約 180 MB"), "{short}");
        assert!(short.to_string().contains("変えていません"), "{short}");
        let newer = CoreError::from(UpgradeError::Newer {
            found: 12,
            supported: 9,
        });
        assert_eq!(newer.code(), "Upgrade");
        assert!(newer.to_string().contains("新しい版の Kura"), "{newer}");
    }
}
