//! 編集（docs/editing.md）のうち、エンジン（kura-engine）を動かす側の部品。文書の部品（[`store`]）・版（[`versions`]）・
//! アンカー（[`anchors`]）・保存のジョブ（[`save`]。上書き・別名・版に戻す。M12 の段 2）・下書き（[`drafts`]）・エディタの
//! セッション（[`editor`]。M12 の段 4）。一括のジョブは M14 でここに足す。

pub mod anchors;
pub mod drafts;
pub mod editor;
pub mod runtime;
pub mod save;
#[cfg(test)]
mod save_tests;
pub mod services;
pub mod store;
#[cfg(test)]
pub(crate) mod testing;
pub mod versions;

pub use runtime::{EngineRuntime, RuntimeConfig, pool_sizes};
pub use save::{Base, EditHandler, SaveAction, SaveRequest, start_save};
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
