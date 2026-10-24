//! 編集（docs/editing.md）のうち、エンジン（kura-engine）を動かす側の部品。エディタのセッション・保存・一括のジョブは
//! M12〜M14 でここに足す。

pub mod anchors;
pub mod runtime;
pub mod services;
pub mod store;
#[cfg(test)]
pub(crate) mod testing;

pub use runtime::{EngineRuntime, RuntimeConfig, pool_sizes};
pub use services::{CodecSet, ENGINE_WORK_DIR, EngineFactory, EngineServices};

use kura_engine::EngineError;

use crate::CoreError;

/// エンジンのエラーを kura-core のエラーにする（`what` は何をしようとしたか）。文書・部品の誤りは Invalid、上限は TooLarge、
/// 扱えない形式は Unsupported、記録との食い違いは Mismatch、中止は Invalid（呼び手が先に中止を見分ける）。
pub(crate) fn from_engine(what: &str, e: EngineError) -> CoreError {
    let message = format!("{what}：{e}");
    match e.root() {
        EngineError::Limit(_) => CoreError::TooLarge(message),
        EngineError::Unsupported(_) | EngineError::CodecMissing { .. } => {
            CoreError::Unsupported(message)
        }
        EngineError::Mismatch(_) => CoreError::Mismatch(message),
        EngineError::Io(_) => CoreError::Io(std::io::Error::other(message)),
        EngineError::Invalid(_)
        | EngineError::Canceled
        | EngineError::Decode { .. }
        | EngineError::Encode { .. }
        | EngineError::Internal(_)
        | EngineError::AtNode { .. } => CoreError::Invalid(message),
    }
}
