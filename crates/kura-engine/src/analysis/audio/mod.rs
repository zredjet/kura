//! 音声の解析の一覧。解析を足すときは、ファイルを 1 つ作り、ここの `mod` と `register_analyses!` の `current` に
//! 1 行ずつ足す（画像の一覧 `analysis/image/mod.rs` とは別のファイル）。

pub mod loop_points;
pub mod loudness;
pub mod loudness_gating;
pub mod peaks;
pub mod silence;

use serde::{Deserialize, Serialize};

/// 音声の区間 [start, end)（フレーム。結果の型に使う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameSpan {
    /// 始まり（含む）。
    pub start: u64,
    /// 終わり（含まない）。
    pub end: u64,
}

crate::analysis::register_analyses! {
    current: [
        loudness_gating::LoudnessGating,
        loudness::LoudnessAnalysis,
        peaks::PeaksAnalysis,
        silence::SilenceAnalysis,
        loop_points::LoopAnalysis,
    ],
    legacy: [],
    dropped: [],
}
