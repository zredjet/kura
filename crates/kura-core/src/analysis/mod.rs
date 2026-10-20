//! 解析（M11。docs/data-model.md §3・§4、docs/editing.md §11、decisions の決定 2・5・7）。
//!
//! - 画像・音声の中身ごとの事実（透過・色数・ドット絵の倍率・アニメ・ラウドネス・ループ点・無音・波形の段）を、kura-engine の
//!   解析器で求めて `analysis_results`（正本。中身 × 解析 ID × 版 × 正規の引数）と `media_facts`（検索と表示の列）に置く。
//! - 計画（[`plan`]）と投影（[`facts`]）は純粋な関数、読み書きは [`store`]、ジョブは [`job`]。
pub mod facts;
pub mod job;
#[cfg(test)]
mod job_tests;
pub mod plan;
pub mod store;

pub use job::{AnalyzeHandler, AnalyzeShared};
