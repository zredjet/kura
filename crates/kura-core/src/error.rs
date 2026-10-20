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

/// ライブラリの DB を新しい版にできなかった理由（docs/data-model.md §5）。当てる前に断ったもの（`Newer`・`NoSpace`・
/// `Backup*`）はライブラリを変えていない。当てる途中の失敗（`Apply*`）は、失敗した 1 つだけが戻り、それより前に当てたものは
/// 残る（`reached` が `from` より大きければ途中の版）。
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
    /// 当てている途中でディスクの空きが足りなくなった（`migration` は戻した。`from` は当てる前の版、`reached` は今の版）。
    #[error(
        "ディスクの空き容量が足りず、ライブラリを新しい版にできませんでした。空きを作ってから開き直してください（{}）",
        where_it_stands(*.from, *.reached, .migration, .detail)
    )]
    ApplyNoSpace {
        migration: String,
        detail: String,
        from: usize,
        reached: usize,
    },
    /// 当てられなかった（`migration` は戻した。`from` は当てる前の版、`reached` は今の版）。
    #[error(
        "ライブラリを新しい版にできませんでした（{}）",
        where_it_stands(*.from, *.reached, .migration, .detail)
    )]
    Apply {
        migration: String,
        detail: String,
        from: usize,
        reached: usize,
    },
}

/// 当てる途中で失敗したときの、ライブラリの今の版の説明。前の版から 1 つでも当てていれば、前の版の Kura ではもう開けない
/// （新しい版のスキーマとして断る）ので、そう言い、当てる前のバックアップの場所を添える。
fn where_it_stands(from: usize, reached: usize, migration: &str, detail: &str) -> String {
    if from > 0 && reached > from {
        format!(
            "途中の版（スキーマ {reached}）まで当てたので、前の版の Kura ではもう開けません。原因を取り除いて開き直すと続きを当てます。当てる前のバックアップがライブラリの backups にあります。{migration}：{detail}"
        )
    } else {
        format!("ライブラリは前の版のまま残っています。{migration}：{detail}")
    }
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

    /// 当てる失敗から作る（`migration` は失敗したマイグレーションの名前、`from` は当てる前の版、`reached` は失敗したときの版）。
    /// 説明に SQL の本文は入れない（[`CoreError::short`]）。
    pub fn apply(migration: &str, err: &CoreError, from: usize, reached: usize) -> Self {
        let (migration, detail) = (migration.to_string(), err.short());
        if err.is_no_space() {
            Self::ApplyNoSpace {
                migration,
                detail,
                from,
                reached,
            }
        } else {
            Self::Apply {
                migration,
                detail,
                from,
                reached,
            }
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

    /// 画面に出す短い説明。SQL の位置を指すエラー（rusqlite の `SqlInputError`）は、SQL の本文を落として SQLite の言葉と
    /// 位置だけにする（マイグレーションなら数 KB の本文が入る）。ほかは `to_string` と同じ。
    pub fn short(&self) -> String {
        match self {
            Self::Db(rusqlite::Error::SqlInputError { msg, offset, .. }) => {
                format!("データベースの操作に失敗しました: {msg}（SQL の {offset} バイト目）")
            }
            other => other.to_string(),
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

    /// SQL の位置を指すエラーは、SQL の本文を落として SQLite の言葉と位置だけにする。
    #[test]
    fn short_drops_the_sql_text() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = "CREATE TABLE t (x); CREATE TABLE t (y); -- ここから下は長い本文";
        let err = CoreError::from(conn.execute_batch(sql).unwrap_err());
        assert!(err.to_string().contains("長い本文"), "{err}");
        let short = err.short();
        assert!(short.contains("already exists"), "{short}");
        assert!(!short.contains("長い本文"), "{short}");
        let other = CoreError::Invalid("x".into());
        assert_eq!(other.short(), other.to_string());
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
        let apply = UpgradeError::apply("0007_blobs_and_kinds", &other, 6, 6);
        assert_eq!(apply.kind(), "apply");
        assert!(
            apply.to_string().contains("0007_blobs_and_kinds"),
            "{apply}"
        );
        assert!(apply.to_string().contains("前の版のまま"), "{apply}");
        // 途中の版まで当てたら、前の版のままとは言わない
        let partial = UpgradeError::apply("0009_analysis", &other, 6, 8);
        assert_eq!(partial.kind(), "apply");
        assert!(!partial.to_string().contains("前の版のまま"), "{partial}");
        assert!(partial.to_string().contains("スキーマ 8"), "{partial}");
        let partial_full = UpgradeError::apply("0009_analysis", &full, 6, 8);
        assert_eq!(partial_full.kind(), "noSpace");
        assert!(
            partial_full.to_string().contains("空き容量"),
            "{partial_full}"
        );
        assert!(
            partial_full.to_string().contains("前の版の Kura"),
            "{partial_full}"
        );
        // 新しいライブラリ（版 0）は「前の版の Kura」と言わない
        let fresh = UpgradeError::apply("0009_analysis", &other, 0, 8);
        assert!(!fresh.to_string().contains("前の版の Kura"), "{fresh}");
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
