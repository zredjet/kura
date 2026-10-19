//! 長い処理（取り込み・サムネイル・プロキシ・ダウンロード・キャプチャ・書き出し・解析）を jobs テーブル経由で動かす。
//!
//! - 種類ごとに同時実行数を決める（docs/architecture.md §5）。
//! - 譲る相手（[`JobKind::yields_to`]）が動いている・待っている間は取り出さず、動いていれば区切りで譲らせる
//!   （`JobEnd::Yield` で queued に戻る。一時停止にはしない）。
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
    /// 書き出し（FR-SYS-02）。
    Export,
    /// 解析（M11。画像・音声の事実と波形の段。取り込み・サムネイルに譲る）。
    Analyze,
}

impl JobKind {
    /// すべての種類（テストと一覧用）。
    pub const ALL: [Self; 7] = [
        Self::Import,
        Self::Thumbnail,
        Self::Proxy,
        Self::Download,
        Self::Capture,
        Self::Export,
        Self::Analyze,
    ];

    /// DB に保存する名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Thumbnail => "thumbnail",
            Self::Proxy => "proxy",
            Self::Download => "download",
            Self::Capture => "capture",
            Self::Export => "export",
            Self::Analyze => "analyze",
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
            "export" => Self::Export,
            "analyze" => Self::Analyze,
            _ => return None,
        })
    }

    /// 同時に動かす数（download 2、ほかは 1）。サムネイルは 1 本のジョブの中でコア数の並列にする
    /// （未作成のものをまとめて処理するので、複数動かすと同じアイテムを取り合う）。解析も 1 本（書き手を 1 つにする）。
    pub fn concurrency(self) -> usize {
        match self {
            Self::Download => 2,
            Self::Import
            | Self::Thumbnail
            | Self::Proxy
            | Self::Capture
            | Self::Export
            | Self::Analyze => 1,
        }
    }

    /// 譲る相手：この種類のジョブが（登録されていて）動いている・待っている間は、この種類を取り出さず、動いていれば
    /// `JobContext::should_yield` で知らせて区切りで `JobEnd::Yield` を返させる。解析は取り込み（NFR-05）とサムネイル
    /// （NFR-04）に譲る。ダウンロードはネットワーク待ちなので譲らない。種類を足したら必ずここに書く（網羅の match。
    /// M12 の保存・M14 の一括も同じ一括のプールと予算を使うので、足すときに解析が譲るかを決める）。
    pub fn yields_to(self) -> &'static [JobKind] {
        match self {
            Self::Analyze => &[Self::Import, Self::Thumbnail],
            Self::Import
            | Self::Thumbnail
            | Self::Proxy
            | Self::Download
            | Self::Capture
            | Self::Export => &[],
        }
    }

    /// 一時停止中のこの種類のジョブがあるとき、`ensure_queued` で新しく積まない（利用者の一時停止を、次の取り込みが
    /// 勝手に外さない）。解析だけ（ほかは取り込みのたびに積み直す今の振る舞いのまま）。
    pub fn keeps_pause(self) -> bool {
        match self {
            Self::Analyze => true,
            Self::Import
            | Self::Thumbnail
            | Self::Proxy
            | Self::Download
            | Self::Capture
            | Self::Export => false,
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

    /// `ALL` がすべての種類を持つこと（種類を足したら、ここの match で気づく）。
    fn listed(kind: JobKind) -> bool {
        match kind {
            JobKind::Import
            | JobKind::Thumbnail
            | JobKind::Proxy
            | JobKind::Download
            | JobKind::Capture
            | JobKind::Export
            | JobKind::Analyze => JobKind::ALL.contains(&kind),
        }
    }

    #[test]
    fn names_round_trip() {
        for kind in JobKind::ALL {
            assert!(listed(kind));
            assert_eq!(JobKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(JobKind::parse("analyze"), Some(JobKind::Analyze));
        assert_eq!(JobKind::parse("edit"), None);
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
        assert_eq!(JobKind::Thumbnail.concurrency(), 1);
        assert_eq!(JobKind::Analyze.concurrency(), 1);
    }

    /// 譲る相手の表（解析だけが、取り込みとサムネイルに譲る）。自分に譲らず、譲り合いの輪がない。
    #[test]
    fn yields_to_table() {
        let table: Vec<(JobKind, &[JobKind])> =
            JobKind::ALL.iter().map(|k| (*k, k.yields_to())).collect();
        assert_eq!(
            table,
            vec![
                (JobKind::Import, &[][..]),
                (JobKind::Thumbnail, &[][..]),
                (JobKind::Proxy, &[][..]),
                (JobKind::Download, &[][..]),
                (JobKind::Capture, &[][..]),
                (JobKind::Export, &[][..]),
                (JobKind::Analyze, &[JobKind::Import, JobKind::Thumbnail][..]),
            ]
        );
        for kind in JobKind::ALL {
            assert!(!kind.yields_to().contains(&kind), "{kind:?}");
            for other in kind.yields_to() {
                assert!(!other.yields_to().contains(&kind), "{kind:?} ⇄ {other:?}");
            }
        }
        let keeps: Vec<JobKind> = JobKind::ALL
            .into_iter()
            .filter(|k| k.keeps_pause())
            .collect();
        assert_eq!(keeps, vec![JobKind::Analyze]);
    }
}
