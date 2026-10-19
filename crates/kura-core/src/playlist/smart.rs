//! スマートプレイリスト（docs/playlists.md §4）。
//!
//! ユニット単位の判定（§4.2）は `unit_matches`（純粋関数）で行う。条件は検索の AST を、タグ・フォルダを
//! ID の集合に、メンバーのどれかで判定する項目をアイテムの集合に置き換えた `UnitCond` にしてから渡す。

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::model::UnitMatch;
use crate::import::MediaKind;
use crate::query::QueryContext;
use crate::query::ast::{FieldCond, Op, PlayedSpec, UnitSpec};
use crate::query::compile::date_matches;
use crate::query::{Expr, parse};
use crate::text::nfc;
use crate::{CoreError, Result};

/// 条件のつなぎ方（ルールの「次の すべて／いずれか に一致」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleMatch {
    #[default]
    All,
    Any,
}

/// スマートの条件（`playlists.rule_json`）：検索の文字列を条件ごとに持つ（ルール編集の 1 行 = 1 条件）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmartRule {
    #[serde(rename = "match", default)]
    pub match_mode: RuleMatch,
    #[serde(default)]
    pub clauses: Vec<String>,
}

impl SmartRule {
    /// 条件の AST。空の条件は飛ばし、何もなければ「すべて」。解析できない条件があれば Invalid。
    pub fn expr(&self) -> Result<Expr> {
        let mut parts = Vec::new();
        for clause in self.clauses.iter().filter(|c| !c.trim().is_empty()) {
            let parsed = parse(clause)
                .map_err(|e| CoreError::Invalid(format!("条件「{clause}」：{}", e.message)))?;
            parts.push(parsed.expr);
        }
        Ok(match (self.match_mode, parts.len()) {
            (_, 1) => parts.pop().unwrap_or_else(Expr::all),
            (RuleMatch::Any, n) if n > 0 => Expr::Or(parts),
            _ => Expr::And(parts),
        })
    }
}

/// ユニットで判定できる形にした条件。
#[derive(Debug, Clone, PartialEq)]
pub enum UnitCond {
    And(Vec<UnitCond>),
    Or(Vec<UnitCond>),
    Not(Box<UnitCond>),
    /// タグ（そのタグと子孫のタグの ID）。
    Tags(HashSet<i64>),
    /// 語（ユニットの名前・URL・メモに含む）。
    Text(String),
    /// ユニットの集計値・属性で判定する項目。
    Field(FieldCond),
    /// メンバーのどれかがこのアイテムの集合に入っていれば真（撮影日・向き・取得元など、§4.2 にない項目）。
    AnyMember(HashSet<i64>),
    /// フォルダ：参照フォルダか、メンバーのどれかがこれらのフォルダにある。
    Folder {
        folders: HashSet<i64>,
        items: HashSet<i64>,
    },
}

/// 判定に使うユニットの事実。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UnitFacts {
    /// ユニット自体のタグ。
    pub own_tags: HashSet<i64>,
    /// メンバー（並び順）と、それぞれのタグ。
    pub members: Vec<i64>,
    pub member_tags: Vec<HashSet<i64>>,
    /// メンバーの種類（スマートの候補では再生できるメンバーだけ。フォント・3D を数えない）。
    pub kinds: HashSet<MediaKind>,
    /// 集計の評価・合計時間（§2.4）。
    pub rating: i64,
    pub duration_ms: i64,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
    pub created_at: i64,
    pub name: String,
    pub source_url: Option<String>,
    pub note: Option<String>,
    /// folder ユニットの参照フォルダ。
    pub folders: HashSet<i64>,
}

/// ユニットが条件を満たすか（§4.2）。
pub fn unit_matches(
    cond: &UnitCond,
    facts: &UnitFacts,
    unit_match: UnitMatch,
    ctx: &QueryContext,
) -> bool {
    match cond {
        UnitCond::And(parts) => parts
            .iter()
            .all(|c| unit_matches(c, facts, unit_match, ctx)),
        UnitCond::Or(parts) => parts
            .iter()
            .any(|c| unit_matches(c, facts, unit_match, ctx)),
        UnitCond::Not(inner) => !unit_matches(inner, facts, unit_match, ctx),
        UnitCond::Tags(ids) => {
            let hit = |tags: &HashSet<i64>| !tags.is_disjoint(ids);
            hit(&facts.own_tags)
                || match unit_match {
                    UnitMatch::Any => facts.member_tags.iter().any(hit),
                    UnitMatch::All => {
                        !facts.member_tags.is_empty() && facts.member_tags.iter().all(hit)
                    }
                    UnitMatch::UnitOnly => false,
                }
        }
        UnitCond::Text(word) => [
            Some(facts.name.as_str()),
            facts.source_url.as_deref(),
            facts.note.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|text| contains(text, word)),
        UnitCond::AnyMember(items) => facts.members.iter().any(|id| items.contains(id)),
        UnitCond::Folder { folders, items } => {
            !facts.folders.is_disjoint(folders) || facts.members.iter().any(|id| items.contains(id))
        }
        UnitCond::Field(field) => field_matches(field, facts, ctx),
    }
}

/// 大文字・小文字を区別せずに含むか（NFC にそろえて比べる）。
fn contains(text: &str, needle: &str) -> bool {
    nfc(text)
        .to_lowercase()
        .contains(&nfc(needle).to_lowercase())
}

fn compare(a: i64, op: Op, b: i64) -> bool {
    match op {
        Op::Has | Op::Eq => a == b,
        Op::Ne => a != b,
        Op::Gt => a > b,
        Op::Ge => a >= b,
        Op::Lt => a < b,
        Op::Le => a <= b,
    }
}

/// 文字列の条件（`:` は含む、`=` は一致、`!=` は含まない。検索と同じ）。
fn text_matches(value: Option<&str>, op: Op, needle: &str) -> bool {
    let Some(value) = value else {
        return op == Op::Ne;
    };
    match op {
        Op::Eq => nfc(value).to_lowercase() == nfc(needle).to_lowercase(),
        Op::Ne => !contains(value, needle),
        _ => contains(value, needle),
    }
}

/// ユニットの集計値・属性で判定する項目（§4.2）。
fn field_matches(field: &FieldCond, facts: &UnitFacts, ctx: &QueryContext) -> bool {
    match field {
        // `種類:動画` はメンバーに動画を含む。`種類=動画` はメンバーが動画だけ
        FieldCond::Type { op, value } => {
            let has_any = facts.kinds.iter().any(|k| value.contains(k));
            match op {
                Op::Has => has_any,
                Op::Eq => !facts.kinds.is_empty() && facts.kinds.iter().all(|k| value.contains(k)),
                Op::Ne => !has_any,
                _ => false,
            }
        }
        FieldCond::Rating { op, value } => compare(facts.rating, *op, *value),
        FieldCond::Dur { op, value } => compare(facts.duration_ms, *op, *value),
        FieldCond::Plays { op, value } => compare(facts.play_count, *op, *value),
        FieldCond::Added { op, value } => date_matches(Some(facts.created_at), *op, value, ctx),
        FieldCond::Played { op, value } => match value {
            PlayedSpec::Never => (facts.last_played_at.is_none()) != (*op == Op::Ne),
            PlayedSpec::Date(spec) => date_matches(facts.last_played_at, *op, spec, ctx),
        },
        FieldCond::Name { op, value } => text_matches(Some(&facts.name), *op, value),
        FieldCond::Url { op, value } => text_matches(facts.source_url.as_deref(), *op, value),
        FieldCond::Note { op, value } => text_matches(facts.note.as_deref(), *op, value),
        // 候補のユニット自体：「ユニット:あり」は真、「なし」は偽、名前はユニットの名前
        FieldCond::Unit { op, value } => {
            let hit = match value {
                UnitSpec::Any => true,
                UnitSpec::None => false,
                UnitSpec::Name(name) => contains(&facts.name, name),
            };
            hit != (*op == Op::Ne)
        }
        // 撮影日・向き・取得元・サイズ・タグの数・フォルダは、メンバーで判定する形（AnyMember / Folder）にしてから渡す
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::ast::{DateSpec, KindValue};

    const DAY: i64 = 24 * 60 * 60 * 1000;

    fn ctx() -> QueryContext {
        QueryContext {
            now_ms: 100 * DAY,
            utc_offset_minutes: 540,
        }
    }

    fn facts() -> UnitFacts {
        UnitFacts {
            members: vec![1, 2, 3],
            member_tags: vec![HashSet::new(); 3],
            kinds: HashSet::from([MediaKind::Image, MediaKind::Audio]),
            rating: 4,
            duration_ms: 90_000,
            play_count: 2,
            last_played_at: Some(98 * DAY),
            created_at: 50 * DAY,
            name: "鎌倉 2026".into(),
            source_url: Some("https://example.com/kamakura".into()),
            note: None,
            folders: HashSet::from([10]),
            ..UnitFacts::default()
        }
    }

    fn field(f: FieldCond) -> bool {
        unit_matches(&UnitCond::Field(f), &facts(), UnitMatch::Any, &ctx())
    }

    #[test]
    fn kinds_has_means_contains_and_eq_means_only() {
        let kinds = |op, value: &[KindValue]| {
            field(FieldCond::Type {
                op,
                value: value.to_vec(),
            })
        };
        assert!(kinds(Op::Has, &[KindValue::Audio]));
        assert!(!kinds(Op::Has, &[KindValue::Video]));
        assert!(!kinds(Op::Eq, &[KindValue::Image]));
        assert!(kinds(Op::Eq, &[KindValue::Image, KindValue::Audio]));
        assert!(kinds(Op::Ne, &[KindValue::Video]));
    }

    #[test]
    fn aggregates_dates_and_texts() {
        assert!(field(FieldCond::Rating {
            op: Op::Ge,
            value: 4
        }));
        assert!(!field(FieldCond::Dur {
            op: Op::Gt,
            value: 90_000
        }));
        assert!(field(FieldCond::Plays {
            op: Op::Eq,
            value: 2
        }));
        assert!(field(FieldCond::Added {
            op: Op::Has,
            value: DateSpec::OlderThanDays(30)
        }));
        assert!(field(FieldCond::Played {
            op: Op::Has,
            value: PlayedSpec::Date(DateSpec::WithinDays(7))
        }));
        assert!(!field(FieldCond::Played {
            op: Op::Has,
            value: PlayedSpec::Never
        }));
        assert!(field(FieldCond::Name {
            op: Op::Has,
            value: "かまくら".replace("かまくら", "鎌倉")
        }));
        assert!(field(FieldCond::Url {
            op: Op::Has,
            value: "KAMAKURA".into()
        }));
        assert!(field(FieldCond::Note {
            op: Op::Ne,
            value: "x".into()
        }));
        assert!(field(FieldCond::Unit {
            op: Op::Has,
            value: UnitSpec::Any
        }));
        let text = UnitCond::Text("2026".into());
        assert!(unit_matches(&text, &facts(), UnitMatch::Any, &ctx()));
    }

    #[test]
    fn members_and_folders() {
        let any = UnitCond::AnyMember(HashSet::from([3, 9]));
        assert!(unit_matches(&any, &facts(), UnitMatch::Any, &ctx()));
        let folder = |folders: &[i64], items: &[i64]| UnitCond::Folder {
            folders: folders.iter().copied().collect(),
            items: items.iter().copied().collect(),
        };
        assert!(unit_matches(
            &folder(&[10], &[]),
            &facts(),
            UnitMatch::Any,
            &ctx()
        ));
        assert!(unit_matches(
            &folder(&[11], &[2]),
            &facts(),
            UnitMatch::Any,
            &ctx()
        ));
        assert!(!unit_matches(
            &folder(&[11], &[9]),
            &facts(),
            UnitMatch::Any,
            &ctx()
        ));
    }
}
