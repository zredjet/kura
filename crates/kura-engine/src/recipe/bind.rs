//! 結び付け（docs/editing.md §4.3。M14 段 1）：レシピの見本の値を、アイテムの値（素材の設定・秒）で上書きして、
//! 具体的な値だけの文書にする。
//!
//! - `Engine::instantiate(base, recipe, 入力の形, Facts)`：レシピを確かめ（`check_recipe`）、頭の文書に継ぎ足し
//!   （`doc::compose`）、結び付けのあるノードを文書の順に 1 つずつ、そのノードの入力の形（前の部分の文書を compile した
//!   推論）から値を作って引数を上書きし、そのノードまでを compile して確かめる。返すのは継いだ文書と、読んだ値の記録。
//! - 素材の設定（格子・ピボット・ループ）は頭（今のファイル）の座標・フレームで書かれているので、頭の出力からそのノードの
//!   入力 0 までのレシピのノードの写し方（格子・ピボットは `SpaceMap`、ループは `TimeMap`）がすべて恒等でなければ断る
//!   （`Moved`）。外したノードは恒等。コマの時間と秒は座標を持たないので見ない（秒はノードの入力のレートと長さで直す）。
//! - 純粋：同じ（base・レシピ・入力の形・`Facts`）から同じ文書と同じ記録。乱数・時刻・環境を読まず、並びは文書の順と
//!   結び付けの並び。浮動小数はピボットの `f64::floor` だけ（IEEE で一意）、秒の直しは整数（i128）。種は変えない。
//! - 版の文書には具体的な値だけが残る。作り直しは文書だけから描き、結び付けを解き直さない。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{
    Binding, MAX_BINDINGS_BYTES, RangePart, Recipe, RecipeKind, SettingKey, Source, TimeAnchor,
};
use crate::digest::{Digest, canonical_within, hash_canonical};
use crate::doc::{
    self, Expand, FormatChoice, Graph, InputId, NodeId, OutputId, OutputSpec, PortRef,
};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{BindKind, ParamKind, ParamSpec, Registry, Unit};
use crate::ops::audio::set_loop::FrameRange;
use crate::ops::util::GridParam;
use crate::render::Engine;
use crate::shape::Shape;

/// アイテムの素材の設定（kura-core が `asset_settings` から読んで渡す。ない欄・壊れた欄は None）。頭（今のファイル）の
/// 座標・フレーム。大きさ・レート・長さは入れない（結び付けたノードの入力の形から取る）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Facts {
    /// 格子（設定の `count` は除く）。
    #[serde(default)]
    pub grid: Option<GridParam>,
    /// ピボット（有限。小数・負も来る）。
    #[serde(default)]
    pub pivot: Option<PivotFact>,
    /// 試聴のループ（start < end）。
    #[serde(default)]
    pub loop_range: Option<FrameRange>,
    /// コマの時間（ms。1 以上）。
    #[serde(default)]
    pub frame_ms: Option<u32>,
}

/// 素材の設定のピボット（画素の角を 0 とする座標。小数・負も来る）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PivotFact {
    pub x: f64,
    pub y: f64,
}

impl Facts {
    /// `kura-facts-v1` のダイジェスト（読んでから書くまでに設定が変わったかを照らす。段 4）。ピボットが有限でなければ `Invalid`。
    pub fn digest(&self) -> Result<Digest, EngineError> {
        if let Some(p) = self.pivot
            && !(p.x.is_finite() && p.y.is_finite())
        {
            return Err(EngineError::Invalid(Issues::one(Issue::error(
                IssueCode::OutOfRange,
                "ピボットの値が有限ではありません",
            ))));
        }
        hash_canonical("kura-facts-v1", self)
    }
}

/// 出どころの記録 1 つ（kura-core が版の記録に残す。作り直しには使わない）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoundValue {
    /// 継いだ文書のノード（版の文書の ID）。
    pub node: NodeId,
    pub op: String,
    pub v: u16,
    pub param: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<RangePart>,
    /// レシピの結び付けのとおり。
    pub from: Source,
    /// 読んだ値：設定は渡された値（`{"x":7.5,"y":3.0}` など）、秒は `{"rate":44100,"frames":N}`（長さが決まらなければ null）。
    #[cfg_attr(feature = "specta", specta(type = crate::specta_types::JsonValue))]
    pub read: serde_json::Value,
    /// 書いた値（`{"x":7,"y":3}`・66150 など）。
    #[cfg_attr(feature = "specta", specta(type = crate::specta_types::JsonValue))]
    pub value: serde_json::Value,
}

/// 出どころの記録の正規の JSON（`MAX_BINDINGS_BYTES` まで）。
pub fn bound_json(list: &[BoundValue]) -> Result<Vec<u8>, EngineError> {
    canonical_within(&list, MAX_BINDINGS_BYTES)
}

/// レシピを当てた文書と、読んだ値の記録。
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// 継いで結び付けた文書（具体的な値だけ）。
    pub graph: Graph,
    pub bound: Vec<BoundValue>,
}

/// 結び付けの誤り。`node` はレシピの文書の ID（画面はレシピのノードとして出す）。指摘（`Issues`）のノードは継いだ文書の ID。
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// N → 1 のレシピは 1 件ずつ当てられない。
    #[error("N → 1 のレシピは 1 件ずつ当てられません")]
    Kind,
    /// `check_recipe`・`compose` が断った（規則の版・大きすぎる・形）。
    #[error("レシピを当てられません: {0}")]
    Recipe(EngineError),
    /// 素材の設定に値がない。
    #[error("ノード {} の {param} に使う素材の設定（{key:?}）がありません", node.0)]
    Missing {
        node: NodeId,
        param: String,
        key: SettingKey,
    },
    /// 前の操作（`by`）で座標・時刻が変わるので、頭の座標の素材の設定を使えない。
    #[error("ノード {} の {param} の前で、ノード {} が座標・時刻を変えます", node.0, by.0)]
    Moved {
        node: NodeId,
        param: String,
        by: NodeId,
    },
    /// 入力の長さが決まらないので、終わりからの秒を直せない。
    #[error("ノード {} の {param}：入力の長さが決まらないので、終わりからの秒を直せません", node.0)]
    NoLength { node: NodeId, param: String },
    /// 秒を音声でない入力に当てた。
    #[error("ノード {} の {param}：入力が音声でないので、秒を使えません", node.0)]
    NotAudio { node: NodeId, param: String },
    /// 直した値が引数の範囲・操作の検証を通らない。
    #[error("ノード {} の {param}：{issues}", node.0)]
    OutOfRange {
        node: NodeId,
        param: String,
        issues: Issues,
    },
    /// 結び付けたノードより前が、このアイテムで推論できない。
    #[error("結び付けたノードより前を、このアイテムに当てられません: {0}")]
    Invalid(Issues),
}

/// 出どころの種類と引数の種類の組（docs/editing.md §4.3。`tests/param_origin.rs` の E53 と同じ表）。
fn fits(kind: BindKind, spec: &ParamSpec) -> bool {
    match kind {
        BindKind::Grid => matches!(spec.kind, ParamKind::Grid),
        BindKind::Pivot => matches!(spec.kind, ParamKind::Point),
        BindKind::Loop => matches!(spec.kind, ParamKind::FrameRange),
        BindKind::FrameMs => matches!(spec.kind, ParamKind::Int { .. }) && spec.unit == Unit::Ms,
        BindKind::Time => {
            (matches!(spec.kind, ParamKind::Int { .. }) && spec.unit == Unit::Samples)
                || matches!(spec.kind, ParamKind::FrameRange)
        }
    }
}

/// 結び付け 1 つを登録表で確かめる（ノードの版の実装の記述にあり、出どころの種類が `binds` にあり、表の組に合う）。
/// ないノード・外したノード・重なりは `Recipe::shape_issues` が見る。
pub(super) fn check_binding(reg: &Registry, graph: &Graph, b: &Binding, issues: &mut Issues) {
    let Some(node) = graph.nodes.iter().find(|n| n.id == b.node) else {
        return;
    };
    let at =
        |code: IssueCode, msg: String| Issue::error(code, msg).at(b.node).param(b.param.clone());
    // 直に実装のある版だけ（実装のない古い版は、引数の形が今の記述と違いうる）
    let Some(imp) = reg.get(&node.op, node.v) else {
        if reg.status(&node.op, node.v).is_ok() {
            issues.push(at(
                IssueCode::BadParams,
                "古い版の操作には結び付けを置けません。見本で開いて更新してください".into(),
            ));
        }
        return;
    };
    let schema = imp.schema();
    let Some(spec) = schema.param(&b.param) else {
        issues.push(at(
            IssueCode::BadParams,
            format!("{} に結び付ける引数 {} がありません", node.op, b.param),
        ));
        return;
    };
    let kind = b.from.bind_kind();
    if !spec.binds.contains(&kind) || !fits(kind, spec) {
        issues.push(at(
            IssueCode::BadParams,
            format!(
                "{} の {} は {kind:?} に結び付けられません",
                node.op, b.param
            ),
        ));
        return;
    }
    let range = matches!(spec.kind, ParamKind::FrameRange);
    match (&b.from, b.part) {
        (Source::Time { .. }, Some(_)) if !range => issues.push(at(
            IssueCode::BadParams,
            "区間でない引数の片方には結び付けられません".into(),
        )),
        (Source::Time { .. }, None) if range => issues.push(at(
            IssueCode::BadParams,
            "区間の引数に秒を結び付けるときは start か end を選びます".into(),
        )),
        (_, Some(_)) => {
            let sample = node.params.get(&b.param);
            if !sample.is_some_and(serde_json::Value::is_object) {
                issues.push(at(
                    IssueCode::BadParams,
                    "区間の片方を結び付けるには、見本の区間が要ります".into(),
                ));
            }
        }
        _ => {}
    }
}

/// `g` のノード `upto` 個だけを残し、出力を `from` にした文書（前の部分の文書）。出力は形式を決めず量子化しない。
fn prefix(g: &Graph, upto: usize, from: PortRef) -> Graph {
    Graph {
        schema: g.schema,
        rules: g.rules,
        inputs: g.inputs.clone(),
        nodes: g.nodes.iter().take(upto).cloned().collect(),
        outputs: vec![OutputSpec {
            id: OutputId(0),
            from,
            format: FormatChoice::Keep,
            quantize: None,
            expand: Expand::One,
        }],
        next_id: g.next_id,
    }
}

/// 引数の JSON の `param`（`part` があればその中の `start`／`end`）を `value` で上書きする。引数が null ならオブジェクトにする。
fn write_param(
    params: &mut serde_json::Value,
    param: &str,
    part: Option<RangePart>,
    value: serde_json::Value,
) -> bool {
    if params.is_null() {
        *params = serde_json::Value::Object(serde_json::Map::new());
    }
    let Some(obj) = params.as_object_mut() else {
        return false;
    };
    match part {
        None => {
            obj.insert(param.to_string(), value);
            true
        }
        Some(part) => match obj
            .get_mut(param)
            .and_then(serde_json::Value::as_object_mut)
        {
            Some(range) => {
                range.insert(part.key().to_string(), value);
                true
            }
            None => false,
        },
    }
}

/// ピボットの小数を整数の点にする（切り捨て。有限で ±2^31 の中だけ）。
fn floor_point(p: PivotFact) -> Option<(i64, i64)> {
    let limit = crate::geom::COORD_LIMIT as f64;
    let one = |v: f64| {
        let f = v.floor();
        (f.is_finite() && (-limit..=limit).contains(&f)).then_some(f as i64)
    };
    Some((one(p.x)?, one(p.y)?))
}

impl Engine {
    /// レシピをアイテムに当てる（継ぎ足しと結び付け。モジュールの冒頭）。`inputs` は `base` の入力の形（kura-core が頭の
    /// 入力のヘッダから渡すもの）。結び付けのないレシピでは `compose` と同じ文書を返す（compile しない）。
    pub fn instantiate(
        &self,
        base: &Graph,
        recipe: &Recipe,
        inputs: &BTreeMap<InputId, Shape>,
        facts: &Facts,
    ) -> Result<Instance, BindError> {
        if recipe.kind != RecipeKind::Each {
            return Err(BindError::Kind);
        }
        self.check_recipe(recipe).map_err(BindError::Recipe)?;
        let mut g = doc::compose(base, &recipe.graph).map_err(BindError::Recipe)?;
        let mut bound = Vec::new();
        if recipe.bindings.is_empty() {
            return Ok(Instance { graph: g, bound });
        }
        let offset = base.next_id;
        let shift = |id: NodeId| {
            id.0.checked_add(offset).map(NodeId).ok_or_else(|| {
                BindError::Recipe(EngineError::Limit("ノードの ID を使い切りました".into()))
            })
        };
        // 結び付けのあるノードを文書の順に 1 つずつ
        for (index, rnode) in recipe.graph.nodes.iter().enumerate() {
            let mine: Vec<&Binding> = recipe
                .bindings
                .iter()
                .filter(|b| b.node == rnode.id)
                .collect();
            let Some(first) = mine.first() else {
                continue;
            };
            let id = shift(rnode.id)?;
            let pos = base.nodes.len() + index;
            let Some(input0) = g.nodes.get(pos).and_then(|n| n.inputs.first()).copied() else {
                return Err(BindError::Invalid(Issues::one(
                    Issue::error(
                        IssueCode::BadInput,
                        "入力のないノードには結び付けられません",
                    )
                    .at(id),
                )));
            };
            // 1：入力の形（前の部分の文書の推論。前のノードの結び付けは済んでいる）
            let before = self
                .compile(&prefix(&g, pos, input0), inputs)
                .map_err(BindError::Invalid)?;
            let Some((_, shape)) = before.output_shapes().first().copied() else {
                return Err(BindError::Invalid(Issues::one(Issue::error(
                    IssueCode::BadGraph,
                    "入力の形が分かりません",
                ))));
            };
            for b in &mine {
                // 2：値を作る
                let (read, value) =
                    self.bound_value(b, rnode.id, &shape, facts, &g, &before, input0, offset)?;
                // 3：引数を上書きし、記録を足す
                let Some(node) = g.nodes.get_mut(pos) else {
                    return Err(BindError::Invalid(Issues::none()));
                };
                if !write_param(&mut node.params, &b.param, b.part, value.clone()) {
                    return Err(BindError::OutOfRange {
                        node: rnode.id,
                        param: b.param.clone(),
                        issues: Issues::one(
                            Issue::error(IssueCode::BadParams, "結び付ける引数の形が違います")
                                .at(id)
                                .param(b.param.clone()),
                        ),
                    });
                }
                bound.push(BoundValue {
                    node: id,
                    op: rnode.op.clone(),
                    v: rnode.v,
                    param: b.param.clone(),
                    part: b.part,
                    from: b.from,
                    read,
                    value,
                });
            }
            // 4：そのノードまでを確かめる（引数の範囲・操作の検証）
            if let Err(issues) = self.compile(&prefix(&g, pos + 1, PortRef::Node { id }), inputs) {
                let param = issues
                    .0
                    .iter()
                    .find_map(|i| {
                        i.param
                            .as_deref()
                            .filter(|p| mine.iter().any(|b| b.param == *p))
                    })
                    .unwrap_or(first.param.as_str())
                    .to_string();
                return Err(BindError::OutOfRange {
                    node: rnode.id,
                    param,
                    issues,
                });
            }
        }
        crate::digest::canonical_len_within(&g, doc::MAX_DOC_BYTES).map_err(|e| {
            BindError::Recipe(match e {
                EngineError::Limit(m) => EngineError::Limit(format!(
                    "結び付けた文書を保存の形（正規の JSON）にできません：{m}"
                )),
                other => other,
            })
        })?;
        Ok(Instance { graph: g, bound })
    }

    /// 結び付け 1 つの値（読んだ値, 書く値）。`shape` はノードの入力 0 の形、`before` は前の部分の計画。
    #[allow(clippy::too_many_arguments)]
    fn bound_value(
        &self,
        b: &Binding,
        node: NodeId,
        shape: &Shape,
        facts: &Facts,
        g: &Graph,
        before: &crate::render::Plan,
        input0: PortRef,
        offset: u32,
    ) -> Result<(serde_json::Value, serde_json::Value), BindError> {
        let missing = |key: SettingKey| BindError::Missing {
            node,
            param: b.param.clone(),
            key,
        };
        let out_of_range = |msg: &str| BindError::OutOfRange {
            node,
            param: b.param.clone(),
            issues: Issues::one(
                Issue::error(IssueCode::OutOfRange, msg)
                    .at(NodeId(node.0.saturating_add(offset)))
                    .param(b.param.clone()),
            ),
        };
        match b.from {
            Source::Setting { key } => {
                if key != SettingKey::FrameMs {
                    moved(b, node, g, before, input0, offset, key == SettingKey::Loop)?;
                }
                match key {
                    SettingKey::Grid => {
                        let grid = facts.grid.ok_or_else(|| missing(key))?;
                        let v = to_json(&grid)?;
                        Ok((v.clone(), v))
                    }
                    SettingKey::Pivot => {
                        let p = facts.pivot.ok_or_else(|| missing(key))?;
                        let (x, y) = floor_point(p)
                            .ok_or_else(|| out_of_range("ピボットの値が範囲の外です"))?;
                        Ok((
                            serde_json::json!({ "x": p.x, "y": p.y }),
                            serde_json::json!({ "x": x, "y": y }),
                        ))
                    }
                    SettingKey::Loop => {
                        let r = facts.loop_range.ok_or_else(|| missing(key))?;
                        let v = to_json(&r)?;
                        Ok((v.clone(), v))
                    }
                    SettingKey::FrameMs => {
                        let ms = facts.frame_ms.ok_or_else(|| missing(key))?;
                        Ok((serde_json::json!(ms), serde_json::json!(ms)))
                    }
                }
            }
            Source::Time { ms, anchor } => {
                let Shape::Audio { rate, frames, .. } = *shape else {
                    return Err(BindError::NotAudio {
                        node,
                        param: b.param.clone(),
                    });
                };
                let read = serde_json::json!({ "rate": rate, "frames": frames.exact() });
                let at = crate::geom::div_round_half_even(i128::from(ms) * i128::from(rate), 1000)
                    .ok_or_else(|| out_of_range("秒をフレームに直せません"))?;
                let f = match anchor {
                    TimeAnchor::Start => at,
                    TimeAnchor::End => {
                        let Some(len) = frames.exact() else {
                            return Err(BindError::NoLength {
                                node,
                                param: b.param.clone(),
                            });
                        };
                        let len = i64::try_from(len)
                            .map_err(|_| out_of_range("音声の長さが大きすぎます"))?;
                        len.checked_sub(at)
                            .filter(|f| *f >= 0)
                            .ok_or_else(|| out_of_range("終わりからの秒が音声より長いです"))?
                    }
                };
                Ok((read, serde_json::json!(f)))
            }
        }
    }
}

/// 頭の出力からノードの入力 0 までのレシピのノード（ID が `offset` 以上）の写し方が恒等か（格子・ピボットは `SpaceMap`、
/// ループは `TimeMap`）。違えば、最初に動かしたノード（文書の順で最も前のもの）の `Moved`。
fn moved(
    b: &Binding,
    node: NodeId,
    g: &Graph,
    before: &crate::render::Plan,
    input0: PortRef,
    offset: u32,
    time: bool,
) -> Result<(), BindError> {
    let mut at = input0;
    let mut by = None;
    // ノードの数より長くはたどらない（前のノードしか指せないので必ず終わる）
    for _ in 0..=g.nodes.len() {
        let PortRef::Node { id } = at else { break };
        if id.0 < offset {
            break;
        }
        let Some((space, time_map)) = before.node_maps(id) else {
            break;
        };
        let identity = if time {
            time_map == TimeMap::Identity
        } else {
            space == SpaceMap::Identity
        };
        if !identity {
            by = Some(id);
        }
        match g
            .nodes
            .iter()
            .find(|n| n.id == id)
            .and_then(|n| n.inputs.first())
        {
            Some(next) => at = *next,
            None => break,
        }
    }
    match by {
        Some(by) => Err(BindError::Moved {
            node,
            param: b.param.clone(),
            by: NodeId(by.0 - offset),
        }),
        None => Ok(()),
    }
}

/// 値を JSON にする。
fn to_json<T: Serialize>(v: &T) -> Result<serde_json::Value, BindError> {
    serde_json::to_value(v).map_err(|e| BindError::Recipe(EngineError::Internal(e.to_string())))
}
