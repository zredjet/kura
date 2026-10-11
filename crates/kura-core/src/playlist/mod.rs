//! プレイリスト（docs/playlists.md §3〜§6）。
//!
//! `resolve`（エントリの展開）と `build_queue`（キューの生成）は DB に触れない純粋関数。
//! DB との読み書きは `store`、スマートプレイリストの生成は `smart`。

pub mod add;
pub mod build;
pub mod generate;
pub mod model;
pub mod play;
pub mod resolve;
pub mod smart;
pub mod store;

pub use build::build_queue;
pub use model::{
    Entry, EntryTarget, Granularity, LimitKind, PlaylistKind, QueueOptions, RepeatMode, UnitMatch,
};
pub use resolve::{ResolvedEntry, Snapshot, UnitData, resolve};

#[cfg(test)]
#[path = "acceptance_tests.rs"]
mod acceptance_tests;

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
