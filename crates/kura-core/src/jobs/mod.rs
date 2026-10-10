//! 長い処理（取り込み・サムネイル・プロキシ・ダウンロード・キャプチャ）を jobs テーブル経由で動かす。
//!
//! - 種類ごとに同時実行数を決める（docs/architecture.md §5）。
//! - 進捗はイベントとして 200ms 以上の間隔で送る。
//! - アプリが落ちて `running` のまま残ったジョブは、次に開いたときに `queued` に戻して再開する。

mod manager;
mod store;
mod throttle;

pub use manager::{Control, EventSink, JobContext, JobEnd, JobHandler, JobManager};
pub use store::{JobFilter, JobRecord, save_partial};
pub use throttle::Throttle;

use serde::{Deserialize, Serialize};

/// ジョブの ID（jobs.id）。
pub type JobId = i64;

/// ジョブの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Import,
    Thumbnail,
    Proxy,
    Download,
    Capture,
}

impl JobKind {
    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Thumbnail => "thumbnail",
            Self::Proxy => "proxy",
            Self::Download => "download",
            Self::Capture => "capture",
        }
    }

    /// DB の名前から戻す。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "import" => Self::Import,
            "thumbnail" => Self::Thumbnail,
            "proxy" => Self::Proxy,
            "download" => Self::Download,
            "capture" => Self::Capture,
            _ => return None,
        })
    }

    /// 同時に動かす数（import 1、thumbnail = CPU コア数、proxy 1、download 2、capture 1）。
    pub fn concurrency(self) -> usize {
        match self {
            Self::Thumbnail => std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            Self::Download => 2,
            Self::Import | Self::Proxy | Self::Capture => 1,
        }
    }
}

/// ジョブの状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Canceled,
}

impl JobState {
    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    /// DB の名前から戻す。
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }

    /// 終わった状態か（done / failed / canceled）。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Canceled)
    }
}

/// フロントへ送る進捗（`job://progress`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    pub id: JobId,
    pub kind: JobKind,
    pub state: JobState,
    /// 0.0〜1.0。わからなければ None。
    pub progress: Option<f64>,
    /// 画面に出す一行（例：「38 / 122 項目」）。
    pub message: Option<String>,
    /// 終わったときの結果（種類ごとの JSON）。
    pub result_json: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for kind in [
            JobKind::Import,
            JobKind::Thumbnail,
            JobKind::Proxy,
            JobKind::Download,
            JobKind::Capture,
        ] {
            assert_eq!(JobKind::parse(kind.as_str()), Some(kind));
        }
        for state in [
            JobState::Queued,
            JobState::Running,
            JobState::Paused,
            JobState::Done,
            JobState::Failed,
            JobState::Canceled,
        ] {
            assert_eq!(JobState::parse(state.as_str()), Some(state));
        }
    }

    #[test]
    fn concurrency_follows_architecture() {
        assert_eq!(JobKind::Import.concurrency(), 1);
        assert_eq!(JobKind::Download.concurrency(), 2);
        assert!(JobKind::Thumbnail.concurrency() >= 1);
    }
}
