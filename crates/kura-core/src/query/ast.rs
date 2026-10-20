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

/// 透過の指定（`透過:` の値。M11）。解析の事実 `media_facts.alpha`（0 なし・1 二値・2 半透明あり）と比べる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaSpec {
    /// 透明な画素がある（二値か半透明）。`透過:あり`。
    Any,
    /// 透明な画素がない。`透過:なし`。
    None,
    /// 完全に透明か不透明の画素だけ。`透過:二値`。
    Binary,
    /// 半透明の画素がある。`透過:半透明`。
    Partial,
}

impl AlphaSpec {
    /// `media_facts.alpha` の値の集合。
    pub fn levels(self) -> &'static [i64] {
        match self {
            Self::Any => &[1, 2],
            Self::None => &[0],
            Self::Binary => &[1],
            Self::Partial => &[2],
        }
    }
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
    // ---- 素材（M11）。`Pot` 以外は解析の事実（`media_facts`）で決まる。まだ解析していないもの・
    // 計れない値は、`:`・`=`・`!=`・大小の比較のどれにも一致しない（除外 `-` だけが含む。§4）----
    /// 透過（画像）。
    Alpha {
        op: Op,
        value: AlphaSpec,
    },
    /// ドット絵（画像）。実効値「素材の設定 ?? 自動の判定」と比べる。
    Pixel {
        op: Op,
        value: bool,
    },
    /// 色数（画像。1〜257、257 は 256 より多い）。
    Colors {
        op: Op,
        value: i64,
    },
    /// 幅と高さがどちらも 2 のべき乗（画像。解析は要らず、幅・高さから求める）。
    Pot {
        op: Op,
        value: bool,
    },
    /// サンプリングレート（音声。Hz）。
    Rate {
        op: Op,
        value: i64,
    },
    /// ビット深度（音声。非可逆の形式は計れない）。
    Bits {
        op: Op,
        value: i64,
    },
    /// チャンネル数（音声）。
    Ch {
        op: Op,
        value: i64,
    },
    /// 積分ラウドネス（音声。LUFS の 1/1000 の整数。大小の比較だけ）。
    Lufs {
        op: Op,
        value: i64,
    },
    /// トゥルーピーク（音声。dBTP の 1/1000 の整数。大小の比較だけ）。
    Peak {
        op: Op,
        value: i64,
    },
    /// ループ点がある（音声）。
    Loop {
        op: Op,
        value: bool,
    },
    /// アニメ（画像。コマが 2 つ以上）。
    Anim {
        op: Op,
        value: bool,
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
            | Self::Orient { op, .. }
            | Self::Alpha { op, .. }
            | Self::Pixel { op, .. }
            | Self::Colors { op, .. }
            | Self::Pot { op, .. }
            | Self::Rate { op, .. }
            | Self::Bits { op, .. }
            | Self::Ch { op, .. }
            | Self::Lufs { op, .. }
            | Self::Peak { op, .. }
            | Self::Loop { op, .. }
            | Self::Anim { op, .. } => *op,
        }
    }

    /// 解析の事実で決まる種類（解析で決まるフィールドでなければ None）。まだ解析していないアイテムは
    /// この種類のものだけが、解析が終わると一致が変わりうる（パレットの「未解析 N 件」）。
    pub fn analyzed_kind(&self) -> Option<KindValue> {
        match self {
            Self::Alpha { .. } | Self::Pixel { .. } | Self::Colors { .. } | Self::Anim { .. } => {
                Some(KindValue::Image)
            }
            Self::Rate { .. }
            | Self::Bits { .. }
            | Self::Ch { .. }
            | Self::Lufs { .. }
            | Self::Peak { .. }
            | Self::Loop { .. } => Some(KindValue::Audio),
            Self::Type { .. }
            | Self::Rating { .. }
            | Self::Dur { .. }
            | Self::Added { .. }
            | Self::Taken { .. }
            | Self::Played { .. }
            | Self::Plays { .. }
            | Self::Folder { .. }
            | Self::Unit { .. }
            | Self::Source { .. }
            | Self::Name { .. }
            | Self::Url { .. }
            | Self::Note { .. }
            | Self::Tags { .. }
            | Self::W { .. }
            | Self::H { .. }
            | Self::Orient { .. }
            | Self::Pot { .. } => None,
        }
    }
}

impl Expr {
    /// 解析の事実で決まるフィールド（透過・ループ・ラウドネスなど）を使っているか。
    pub fn uses_analysis(&self) -> bool {
        match self {
            Expr::And(items) | Expr::Or(items) => items.iter().any(Expr::uses_analysis),
            Expr::Not(inner) => inner.uses_analysis(),
            Expr::Tag(_) | Expr::Text(_) => false,
            Expr::Field(cond) => cond.analyzed_kind().is_some(),
        }
    }

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

    /// 素材のフィールド（M11）も同じ形で往復する。足しただけなので、保存済みの rule_json はそのまま読める。
    #[test]
    fn asset_fields_serialize_like_the_spec() {
        let expr = Expr::And(vec![
            Expr::Field(FieldCond::Alpha {
                op: Op::Has,
                value: AlphaSpec::Any,
            }),
            Expr::Not(Box::new(Expr::Field(FieldCond::Alpha {
                op: Op::Eq,
                value: AlphaSpec::Partial,
            }))),
            Expr::Field(FieldCond::Pixel {
                op: Op::Has,
                value: true,
            }),
            Expr::Field(FieldCond::Colors {
                op: Op::Le,
                value: 16,
            }),
            Expr::Field(FieldCond::Pot {
                op: Op::Ne,
                value: true,
            }),
            Expr::Field(FieldCond::Rate {
                op: Op::Ge,
                value: 44_100,
            }),
            Expr::Field(FieldCond::Bits {
                op: Op::Eq,
                value: 24,
            }),
            Expr::Field(FieldCond::Ch {
                op: Op::Has,
                value: 2,
            }),
            Expr::Field(FieldCond::Lufs {
                op: Op::Gt,
                value: -14_000,
            }),
            Expr::Field(FieldCond::Peak {
                op: Op::Le,
                value: -1_000,
            }),
            Expr::Field(FieldCond::Loop {
                op: Op::Has,
                value: true,
            }),
            Expr::Field(FieldCond::Anim {
                op: Op::Has,
                value: false,
            }),
        ]);
        let json = serde_json::to_value(&expr).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "and": [
                { "field": "alpha", "op": "has", "value": "any" },
                { "not": { "field": "alpha", "op": "eq", "value": "partial" } },
                { "field": "pixel", "op": "has", "value": true },
                { "field": "colors", "op": "le", "value": 16 },
                { "field": "pot", "op": "ne", "value": true },
                { "field": "rate", "op": "ge", "value": 44100 },
                { "field": "bits", "op": "eq", "value": 24 },
                { "field": "ch", "op": "has", "value": 2 },
                { "field": "lufs", "op": "gt", "value": -14000 },
                { "field": "peak", "op": "le", "value": -1000 },
                { "field": "loop", "op": "has", "value": true },
                { "field": "anim", "op": "has", "value": false }
            ] })
        );
        let back: Expr = serde_json::from_value(json).unwrap();
        assert_eq!(back, expr);
        // 解析の事実で決まるのは Pot 以外
        assert!(expr.uses_analysis());
        assert!(
            !Expr::Field(FieldCond::Pot {
                op: Op::Has,
                value: true
            })
            .uses_analysis()
        );
        assert!(
            !Expr::Field(FieldCond::Rating {
                op: Op::Ge,
                value: 4
            })
            .uses_analysis()
        );
    }
}
