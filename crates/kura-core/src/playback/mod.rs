//! 再生（docs/architecture.md §8、docs/playlists.md §6）：再生可否の表とプロキシ、再生済みの記録。

pub mod proxy;
pub mod report;
pub mod support;

pub use proxy::{ProxyHandler, ProxyShared, ProxyTools};
pub use report::{PlayReport, judge, record};
pub use support::{Container, PlaybackSupport, ProxyPlan, SupportEntry};
