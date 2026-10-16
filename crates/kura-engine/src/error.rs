//! エラーと検証の指摘（docs/editing.md §13）。

use std::fmt;

use serde::Serialize;

use crate::doc::NodeId;

/// 指摘の重さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Severity {
    /// 実行できない。
    Error,
    /// 実行できるが、知らせることがある（メタデータを落とした、など）。
    Warning,
}

/// 指摘の種類。画面の文言はこの値から引く。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum IssueCode {
    /// 文書の形が正しくない（ID の重複・前にないノードの参照など）。
    BadGraph,
    /// 知らない操作。
    UnknownOp,
    /// 操作の版が新しすぎる、または移せない古い版。
    UnknownVersion,
    /// 文書のスキーマが新しすぎる。
    SchemaTooNew,
    /// 引数が読めない。
    BadParams,
    /// 引数が範囲の外。
    OutOfRange,
    /// 入力の種類・数が操作に合わない。
    BadInput,
    /// 大きすぎる（上限）。
    TooLarge,
    /// メタデータを落とした（警告）。
    MetadataDropped,
}

/// 検証の指摘 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub node: Option<NodeId>,
    pub param: Option<String>,
    pub code: IssueCode,
    pub severity: Severity,
    pub message: String,
}

impl Issue {
    /// エラーの指摘。
    pub fn error(code: IssueCode, message: impl Into<String>) -> Self {
        Self {
            node: None,
            param: None,
            code,
            severity: Severity::Error,
            message: message.into(),
        }
    }

    /// 警告の指摘。
    pub fn warning(code: IssueCode, message: impl Into<String>) -> Self {
        Self {
            node: None,
            param: None,
            code,
            severity: Severity::Warning,
            message: message.into(),
        }
    }

    /// どのノードの指摘かを付ける。
    pub fn at(mut self, node: NodeId) -> Self {
        self.node = Some(node);
        self
    }

    /// どの引数の指摘かを付ける。
    pub fn param(mut self, key: impl Into<String>) -> Self {
        self.param = Some(key.into());
        self
    }
}

/// 指摘の集まり。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Issues(pub Vec<Issue>);

impl Issues {
    /// 指摘なし。
    pub fn none() -> Self {
        Self(Vec::new())
    }

    /// 指摘 1 つ。
    pub fn one(issue: Issue) -> Self {
        Self(vec![issue])
    }

    /// 足す。
    pub fn push(&mut self, issue: Issue) {
        self.0.push(issue);
    }

    /// まとめて足す。
    pub fn extend(&mut self, other: Issues) {
        self.0.extend(other.0);
    }

    /// エラーが 1 つでもあれば true。
    pub fn has_errors(&self) -> bool {
        self.0.iter().any(|i| i.severity == Severity::Error)
    }

    /// 指摘がなければ true。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// すべての指摘に、どのノードのものかを付ける（すでに付いているものは変えない）。
    pub fn at(mut self, node: NodeId) -> Self {
        for issue in &mut self.0 {
            issue.node.get_or_insert(node);
        }
        self
    }
}

impl fmt::Display for Issues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let messages: Vec<&str> = self.0.iter().map(|i| i.message.as_str()).collect();
        f.write_str(&messages.join(" / "))
    }
}

/// エンジンのエラー。kura-core の `CoreError` へは code で変換する。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("文書が正しくありません: {0}")]
    Invalid(Issues),
    #[error("扱えません: {0}")]
    Unsupported(String),
    #[error("大きすぎます: {0}")]
    Limit(String),
    #[error("中止しました")]
    Canceled,
    #[error("読めませんでした（{codec}）: {message}")]
    Decode { codec: String, message: String },
    #[error("書けませんでした（{codec}）: {message}")]
    Encode { codec: String, message: String },
    #[error("{tool} が見つかりません")]
    CodecMissing { tool: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("内部の誤り: {0}")]
    Internal(String),
}

impl EngineError {
    /// 種類を表す短い名前（AppError の code に使う）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "Invalid",
            Self::Unsupported(_) => "Unsupported",
            Self::Limit(_) => "TooLarge",
            Self::Canceled => "Canceled",
            Self::Decode { .. } | Self::Encode { .. } | Self::Io(_) => "Io",
            Self::CodecMissing { .. } => "ToolMissing",
            Self::Internal(_) => "Internal",
        }
    }
}

impl From<Issues> for EngineError {
    fn from(issues: Issues) -> Self {
        Self::Invalid(issues)
    }
}
