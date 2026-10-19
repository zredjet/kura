//! 検索言語の AST（docs/search-query.md §4）。JSON にしてスマートプレイリストの `rule_json` に保存する。
//!
//! ```json
//! { "and": [
//!   { "tag": { "ns": "", "name": "夕景" } },
//!   { "field": "type", "op": "has", "value": ["image", "video"] },
//!   { "not": { "tag": { "ns": "", "name": "未整理" } } },
//!   { "field": "rating", "op": "ge", "value": 4 }
//! ] }
//! ```

use serde::{Deserialize, Serialize};

/// 条件の式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expr {
    /// すべてを満たす（空なら全件）。
    And(Vec<Expr>),
    /// どれかを満たす。
    Or(Vec<Expr>),
    /// 満たさない。
    Not(Box<Expr>),
    /// タグ。名前空間なしの語は、同じ名前のタグ（別名を含む）がなければ全文検索になる。
    Tag(TagTerm),
    /// 引用符で囲んだ語（全文検索）。
    Text(String),
    /// フィールドの条件（`{"field": "...", "op": "...", "value": ...}`）。
    #[serde(untagged)]
    Field(FieldCond),
}

impl Expr {
    /// 何でも満たす式（空の条件）。
    pub fn all() -> Self {
        Self::And(Vec::new())
    }
}

/// タグの語（`ns` が空なら名前空間なし）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagTerm {
    pub ns: String,
    pub name: String,
}

/// 比較の仕方。`:` は `has`、`=` は `eq`（フォルダの子孫を含むか、ユニットの「含む」と「だけ」を分けるため。ADR-51）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// `:`
    Has,
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
}

impl Op {
    /// 入力での書き方。
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Has => ":",
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
        }
    }

    /// 大小の比較か。
    pub fn is_comparison(self) -> bool {
        matches!(self, Self::Gt | Self::Ge | Self::Lt | Self::Le)
    }
}

/// アイテムの種類（`種類:`・`type:` の値）。種類の表は 1 つ（`import::MediaKind`）で、JSON の名前は `items.kind` と同じ。
pub use crate::import::MediaKind as KindValue;

/// 年月日（端末の時差での日付）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CivilDate {
    pub y: i32,
    pub m: u32,
    pub d: u32,
}

/// 日付の指定。相対の指定は相対のまま持ち、検索するときに今の時刻で解決する（§4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateSpec {
    Today,
    Yesterday,
    /// N 日以内（今から N×24 時間以内）。
    WithinDays(u32),
    /// N 日以上前（今から N×24 時間より前）。
    OlderThanDays(u32),
    /// `from` の日から `to` の日の終わりまで（`2026-10` は 10 月 1 日〜31 日）。
    Range {
        from: CivilDate,
        to: CivilDate,
    },
}

/// 再生の指定（`再生:未` か日付）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayedSpec {
    Never,
    Date(DateSpec),
}

/// ユニットの指定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitSpec {
    /// どれかのユニットに入っている（`ユニット:あり`）。
    Any,
    /// どのユニットにも入っていない（`ユニット:なし`）。
    None,
    /// その名前のユニットに入っている。
    Name(String),
}

/// 向き。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Portrait,
    Landscape,
    Square,
}

/// フィールドの条件（§3）。JSON では `{"field": "<英語名>", "op": "...", "value": ...}`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "field", rename_all = "snake_case")]
pub enum FieldCond {
    Type {
        op: Op,
        value: Vec<KindValue>,
    },
    Rating {
        op: Op,
        value: i64,
    },
    /// 長さ（ミリ秒）。画像は対象外。
    Dur {
        op: Op,
        value: i64,
    },
    Added {
        op: Op,
        value: DateSpec,
    },
    Taken {
        op: Op,
        value: DateSpec,
    },
    Played {
        op: Op,
        value: PlayedSpec,
    },
    Plays {
        op: Op,
        value: i64,
    },
    /// フォルダのパス（いちばん上から）。`has` は子孫を含む、`eq` はそのフォルダだけ。
    Folder {
        op: Op,
        value: Vec<String>,
    },
    Unit {
        op: Op,
        value: UnitSpec,
    },
    Source {
        op: Op,
        value: String,
    },
    /// タイトルと元ファイル名。
    Name {
        op: Op,
        value: String,
    },
    Url {
        op: Op,
        value: String,
    },
    Note {
        op: Op,
        value: String,
    },
    Tags {
        op: Op,
        value: i64,
    },
    W {
        op: Op,
        value: i64,
    },
    H {
        op: Op,
        value: i64,
    },
    Orient {
        op: Op,
        value: Orientation,
    },
}

impl FieldCond {
    /// 比較の仕方。
    pub fn op(&self) -> Op {
        match self {
            Self::Type { op, .. }
            | Self::Rating { op, .. }
            | Self::Dur { op, .. }
            | Self::Added { op, .. }
            | Self::Taken { op, .. }
            | Self::Played { op, .. }
            | Self::Plays { op, .. }
            | Self::Folder { op, .. }
            | Self::Unit { op, .. }
            | Self::Source { op, .. }
            | Self::Name { op, .. }
            | Self::Url { op, .. }
            | Self::Note { op, .. }
            | Self::Tags { op, .. }
            | Self::W { op, .. }
            | Self::H { op, .. }
            | Self::Orient { op, .. } => *op,
        }
    }
}

impl Expr {
    /// タグとして判定する語（素の語・名前空間付きの語と、引用符で囲んだ語）。引用符で囲んだ語も
    /// 名前空間なしの語として、別名 → 正式タグ → 全文検索の順に判定する（search-query.md §2。ADR-127）。
    pub fn tag_term(&self) -> Option<std::borrow::Cow<'_, TagTerm>> {
        match self {
            Expr::Tag(term) => Some(std::borrow::Cow::Borrowed(term)),
            Expr::Text(text) => Some(std::borrow::Cow::Owned(TagTerm {
                ns: String::new(),
                name: text.clone(),
            })),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_like_the_spec() {
        let expr = Expr::And(vec![
            Expr::Tag(TagTerm {
                ns: String::new(),
                name: "夕景".into(),
            }),
            Expr::Field(FieldCond::Type {
                op: Op::Has,
                value: vec![KindValue::Image, KindValue::Video],
            }),
            Expr::Not(Box::new(Expr::Tag(TagTerm {
                ns: String::new(),
                name: "未整理".into(),
            }))),
            Expr::Field(FieldCond::Rating {
                op: Op::Ge,
                value: 4,
            }),
            Expr::Field(FieldCond::Added {
                op: Op::Has,
                value: DateSpec::WithinDays(7),
            }),
        ]);
        let json = serde_json::to_value(&expr).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "and": [
                { "tag": { "ns": "", "name": "夕景" } },
                { "field": "type", "op": "has", "value": ["image", "video"] },
                { "not": { "tag": { "ns": "", "name": "未整理" } } },
                { "field": "rating", "op": "ge", "value": 4 },
                { "field": "added", "op": "has", "value": { "within_days": 7 } }
            ] })
        );
        let back: Expr = serde_json::from_value(json).unwrap();
        assert_eq!(back, expr);
    }
}
