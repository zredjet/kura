//! 編集（docs/editing.md）のうち、エンジン（kura-engine）を動かす側の部品。エディタのセッション・保存・一括のジョブは
//! M12〜M14 でここに足す。

pub mod runtime;
pub mod services;

pub use runtime::{EngineRuntime, RuntimeConfig, pool_sizes};
pub use services::{CodecSet, ENGINE_WORK_DIR, EngineFactory, EngineServices};
