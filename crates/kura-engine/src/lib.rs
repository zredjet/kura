//! Kura の編集の操作エンジン（docs/editing.md）。
//!
//! OS・DB・Tauri を知らない純粋な層。文書（操作のノードの並び）を元の値に当てて、新しい値を作る。
//! 同じ文書・同じ元なら、いつでも同じ結果になる（決定性。docs/editing.md §8）。
//! 決定性の規則は `clippy.toml` で機械的に強制している（.claude/rules/engine.md）。

#![forbid(unsafe_code)]

pub mod analysis;
pub mod audio;
pub mod budget;
pub mod codec;
pub mod determinism;
pub mod digest;
pub mod doc;
pub mod doc_parts;
pub mod error;
pub mod geom;
pub mod imaging;
pub mod limits;
pub mod mapping;
pub mod math;
pub mod model;
pub mod monitor;
pub mod op;
pub mod ops;
pub mod paint;
pub mod render;
pub mod session;
pub mod shape;
pub mod testfiles;
pub mod testgen;
pub mod testing;
pub mod value;

pub use analysis::{Analysis, AnalysisRequest, Analyzed};
pub use digest::Digest;
pub use doc::{Graph, Node, NodeId};
pub use error::{EngineError, Issue, IssueCode, Issues, Severity};
pub use limits::Limits;
pub use monitor::{Monitor, NoMonitor};
pub use render::{Engine, Plan, Rendered};
pub use shape::{Dim, Shape, ValueKind};
pub use value::Value;

/// このクレートの版（kura-core の SVG の働き手が、親と子が同じビルドかを照らす印に入れる。M11 の決定 23）。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
