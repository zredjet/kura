//! レシピ（docs/editing.md §4.3。M14 段 1）：名前を付けて保存した文書と、結び付けの組。
//!
//! - 文書（`graph`）は今のスキーマの文書そのもの。ノードの引数は見本の値で、結び付けがなくても描ける。
//! - 結び付け（`bindings`）は、ノードの引数（区間なら片方）をアイテムの値（素材の設定・秒）で上書きする（bind.rs）。
//! - 共有のファイル `.kurarecipe`（file.rs）は封筒 `{ kura, schema, name, description, kind, requires, graph, bindings }` の
//!   正規の JSON。読むときは壊れたものより先に「新しい Kura が要る」ものを断り、旧版の操作には印を付ける。
//! - どの口も純粋（OS・DB を知らない）。レシピを当てる前には毎回 `Engine::check_recipe` を通す（DB から読んだものも）。

mod bind;
mod file;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub use bind::{BindError, BoundValue, Facts, Instance, PivotFact, bound_json};
pub use file::{LoadedRecipe, RecipeNote, bindings_from_json, bindings_json};

use crate::doc::{
    self, DecodeOptions, Expand, FormatChoice, Graph, InputRole, Node, NodeId, PortRef,
};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::limits::Limits;
use crate::render::Engine;

/// レシピのファイルの型（封筒）の版。項目・列挙の値・結び付けの出どころを足したら上げ、`file::RECIPE_STEPS` に移行を足す。
pub const RECIPE_SCHEMA: u32 = 1;
/// `.kurarecipe` の大きさの上限（バイト）。継ぎ足した文書が `doc::MAX_DOC_BYTES`（16 MiB）に収まる余地を残す。
pub const MAX_RECIPE_BYTES: usize = 4 << 20;
/// 名前の長さの上限（UTF-8 のバイト）。1 以上。
pub const MAX_NAME_BYTES: usize = 256;
/// 説明の長さの上限（UTF-8 のバイト）。0 でもよい。
pub const MAX_DESCRIPTION_BYTES: usize = 4096;
/// 結び付けの数の上限。
pub const MAX_BINDINGS: usize = 64;
/// 結び付け・出どころの記録の正規の JSON の上限（バイト。`bindings_json`・`bound_json`）。
pub const MAX_BINDINGS_BYTES: usize = 64 << 10;
/// 秒の結び付けの ms の上限（2^32。直したフレームは引数の範囲（2^40）で確かめる）。
pub const MAX_BIND_MS: u64 = 1 << 32;

/// レシピの種類。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecipeKind {
    /// 1 件ずつ当てる（入力は Primary 1 つ。頭に継ぎ足す）。
    Each,
    /// N → 1（入力は Selection 1 つ。M14 段 6。段 1 は形の確かめだけで、結び付けは持てない）。
    Merge,
}

/// レシピ（名前を付けて保存した文書と結び付け）。作るときは [`Recipe::new`]・[`Recipe::from_doc`]、当てる前は
/// [`Engine::check_recipe`] を通す。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    pub name: String,
    pub description: String,
    pub kind: RecipeKind,
    /// 今のスキーマの文書。ノードの引数は見本の値（そのままで描ける文書）。
    pub graph: Graph,
    /// `(node, param, part)` の順に並べて持つ（正規の形。`part` なしが先）。
    pub bindings: Vec<Binding>,
}

/// レシピが使う操作 1 つ（文書に書いたままの版）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub op: String,
    pub v: u16,
}

/// レシピの結び付け 1 つ：ノード `node` の引数 `param`（区間なら片方 `part`）を、アイテムの値 `from` で上書きする。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Binding {
    /// レシピの文書のノード。
    pub node: NodeId,
    /// 引数の名前（`ParamSpec.key`）。
    pub param: String,
    /// 区間（`FrameRange`）の片方だけを結ぶとき。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<RangePart>,
    pub from: Source,
}

/// 結び付けの出どころ。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", deny_unknown_fields)]
pub enum Source {
    /// 素材の設定（アイテムごとの利用者の設定。頭の座標・フレーム）。`{"type":"setting","key":"grid"}`。
    Setting { key: SettingKey },
    /// 秒（ms）。そのノードの入力のレートでフレームに直す。`anchor` が end なら終わりから。
    /// `{"type":"time","ms":2000,"anchor":"start"}`。
    Time { ms: u64, anchor: TimeAnchor },
}

/// 素材の設定の欄。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingKey {
    Grid,
    Pivot,
    Loop,
    FrameMs,
}

/// 秒を数える基準。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimeAnchor {
    /// 頭から。
    Start,
    /// 終わりから（入力の長さが決まっていること）。
    End,
}

/// 区間の片方。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RangePart {
    Start,
    End,
}

impl RangePart {
    /// 区間の JSON の項目の名前。
    pub fn key(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }
}

impl Source {
    /// 引数の記述の `binds` に要る種類。
    pub fn bind_kind(&self) -> crate::op::BindKind {
        use crate::op::BindKind;
        match self {
            Self::Setting {
                key: SettingKey::Grid,
            } => BindKind::Grid,
            Self::Setting {
                key: SettingKey::Pivot,
            } => BindKind::Pivot,
            Self::Setting {
                key: SettingKey::Loop,
            } => BindKind::Loop,
            Self::Setting {
                key: SettingKey::FrameMs,
            } => BindKind::FrameMs,
            Self::Time { .. } => BindKind::Time,
        }
    }
}

/// 結び付けの並びの鍵（`(node, param, part)`。`part` なしが先）。
fn binding_key(b: &Binding) -> (NodeId, &str, Option<RangePart>) {
    (b.node, b.param.as_str(), b.part)
}

/// 結び付けを正規の順に並べる（安定な並べ替え）。
pub(crate) fn sort_bindings(bindings: &mut [Binding]) {
    bindings.sort_by(|a, b| binding_key(a).cmp(&binding_key(b)));
}

/// 名前と説明の確かめ（長さはバイトで数え、制御文字もバイトで見る。多バイト文字のバイトは 0x80 以上なので混ざらない）。
/// 前後の空白の除き方と NFC への正規化は kura-core が行う（エンジンは Unicode の表を持たない）。kura-core がレシピの名前・
/// 説明だけを変えるときにも使う（M14 段 3）。
pub fn check_text(name: &str, description: &str) -> Result<(), EngineError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES {
        return Err(EngineError::Invalid(Issues::one(
            Issue::error(
                IssueCode::BadGraph,
                format!(
                    "レシピの名前は 1〜{MAX_NAME_BYTES} バイトです（{} バイト）",
                    bytes.len()
                ),
            )
            .param("name"),
        )));
    }
    if bytes.iter().any(|b| *b < 0x20 || *b == 0x7f) {
        return Err(EngineError::Invalid(Issues::one(
            Issue::error(IssueCode::BadGraph, "レシピの名前に制御文字があります").param("name"),
        )));
    }
    let bytes = description.as_bytes();
    if bytes.len() > MAX_DESCRIPTION_BYTES {
        return Err(EngineError::Invalid(Issues::one(
            Issue::error(
                IssueCode::BadGraph,
                format!(
                    "レシピの説明は {MAX_DESCRIPTION_BYTES} バイトまでです（{} バイト）",
                    bytes.len()
                ),
            )
            .param("description"),
        )));
    }
    if bytes
        .iter()
        .any(|b| (*b < 0x20 && *b != b'\n' && *b != b'\t') || *b == 0x7f)
    {
        return Err(EngineError::Invalid(Issues::one(
            Issue::error(
                IssueCode::BadGraph,
                "レシピの説明に改行とタブのほかの制御文字があります",
            )
            .param("description"),
        )));
    }
    Ok(())
}

impl Recipe {
    /// レシピを作る：登録表の要らない確かめ（名前・説明・文書の形・入出力・結び付けの形・大きさ）をして、結び付けを
    /// 正規の順に並べる。操作の版と引数・結び付ける引数の確かめは [`Engine::check_recipe`]。
    pub fn new(
        name: String,
        description: String,
        kind: RecipeKind,
        graph: Graph,
        mut bindings: Vec<Binding>,
    ) -> Result<Self, EngineError> {
        check_text(&name, &description)?;
        sort_bindings(&mut bindings);
        let recipe = Self {
            name,
            description,
            kind,
            graph,
            bindings,
        };
        let issues = recipe.shape_issues(Limits::default().max_nodes);
        if issues.has_errors() {
            return Err(EngineError::Invalid(issues));
        }
        recipe.file_len()?;
        // 文書は保存の形に往復させて持つ（メモリ上の 16.0 と、ファイルから読んだ 16 を同じレシピにする）
        let graph = doc::load(&recipe.graph.to_canonical_json()?)?;
        Ok(Self { graph, ..recipe })
    }

    /// エディタの文書からレシピを作る（段 7 の「レシピとして保存…」）。種類は `Each`。
    /// 1. `keep` が偽のノードを外す。外したノードを指していた入力・出力は、そのノードの入力 0 に付け替える（続けて外した
    ///    ノードは順にたどる）。入力 0 のないノードは外せない（`BadGraph`）。型の合わない外し方はここでは断らず、見本での
    ///    `instantiate` と compile が指摘する。
    /// 2. ノードの ID を文書の順に 1, 2, … と付け直し、`next_id` を n + 1 にする（結び付けも付け直す。種はそのまま）。
    /// 3. `with_format` が偽なら出力を `Keep`・量子化なしにする（真なら文書の出力の形式・量子化をそのまま）。
    /// 4. [`Recipe::new`] を通す。外すノードへの結び付けは断る（`BadParams`）。
    pub fn from_doc(
        name: String,
        description: String,
        doc: &Graph,
        keep: &dyn Fn(&Node) -> bool,
        with_format: bool,
        bindings: Vec<Binding>,
    ) -> Result<Self, EngineError> {
        // 外したノードの ID → 付け替える先（外していないノードの、付け直す前の参照）
        let mut replaced: BTreeMap<NodeId, PortRef> = BTreeMap::new();
        let follow = |r: PortRef, replaced: &BTreeMap<NodeId, PortRef>| match r {
            PortRef::Node { id } => replaced.get(&id).copied().unwrap_or(r),
            other => other,
        };
        let mut kept: Vec<Node> = Vec::new();
        for node in &doc.nodes {
            if keep(node) {
                let mut n = node.clone();
                n.inputs = n.inputs.iter().map(|r| follow(*r, &replaced)).collect();
                kept.push(n);
            } else {
                let Some(first) = node.inputs.first() else {
                    return Err(EngineError::Invalid(Issues::one(
                        Issue::error(IssueCode::BadGraph, "入力のないノードは外せません")
                            .at(node.id),
                    )));
                };
                let to = follow(*first, &replaced);
                replaced.insert(node.id, to);
            }
        }
        let mut outputs = doc.outputs.clone();
        for out in &mut outputs {
            out.from = follow(out.from, &replaced);
            if !with_format {
                out.format = FormatChoice::Keep;
                out.quantize = None;
            }
        }
        // 文書の順に 1, 2, … と付け直す
        let renumber: BTreeMap<NodeId, NodeId> = kept
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, NodeId(i as u32 + 1)))
            .collect();
        let new_id = |id: NodeId| renumber.get(&id).copied();
        let fix = |r: PortRef| match r {
            PortRef::Node { id } => new_id(id).map_or(r, |id| PortRef::Node { id }),
            other => other,
        };
        for n in &mut kept {
            n.id = new_id(n.id).unwrap_or(n.id);
            n.inputs = n.inputs.iter().map(|r| fix(*r)).collect();
        }
        for out in &mut outputs {
            out.from = fix(out.from);
        }
        let mut moved = Vec::with_capacity(bindings.len());
        for mut b in bindings {
            let Some(id) = new_id(b.node) else {
                return Err(EngineError::Invalid(Issues::one(
                    Issue::error(
                        IssueCode::BadParams,
                        "レシピに入れないノードには結び付けを置けません",
                    )
                    .at(b.node)
                    .param(b.param),
                )));
            };
            b.node = id;
            moved.push(b);
        }
        let graph = Graph {
            schema: doc.schema,
            rules: doc.rules,
            inputs: doc.inputs.clone(),
            next_id: kept.len() as u32 + 1,
            nodes: kept,
            outputs,
        };
        Self::new(name, description, RecipeKind::Each, graph, moved)
    }

    /// 文書が使う操作（すべてのノード。外したノードも）：（操作, 文書に書いたままの版）を `op` のバイト列の順・`v` の順に
    /// 並べ、重なりを除いたもの。
    pub fn requires(&self) -> Vec<Requirement> {
        let mut out: Vec<Requirement> = self
            .graph
            .nodes
            .iter()
            .map(|n| Requirement {
                op: n.op.clone(),
                v: n.v,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// 文書のスキーマと形（ID・参照・規則の版・ノードの数）の確かめ。
    fn structure_issues(&self, max_nodes: u64) -> Issues {
        if self.graph.schema != doc::SCHEMA {
            return Issues::one(Issue::error(
                IssueCode::BadGraph,
                "今のスキーマの文書ではありません",
            ));
        }
        self.graph.check_structure(max_nodes)
    }

    /// 登録表の要らない形の確かめ（文書のスキーマと形・種類と入力・出力・結び付けの形）。指摘を集めて返す。
    pub(crate) fn shape_issues(&self, max_nodes: u64) -> Issues {
        let g = &self.graph;
        let mut issues = Issues::none();
        let err = |code: IssueCode, msg: &str| Issue::error(code, msg);
        let structure = self.structure_issues(max_nodes);
        if structure.has_errors() {
            return structure;
        }
        // 種類と入力
        let want = match self.kind {
            RecipeKind::Each => InputRole::Primary,
            RecipeKind::Merge => InputRole::Selection,
        };
        match g.inputs.as_slice() {
            [slot] => {
                if matches!(slot.role, InputRole::Fixed { .. }) {
                    issues.push(err(
                        IssueCode::BadInput,
                        "固定の素材を使うレシピは、この版では扱えません",
                    ));
                } else if slot.role != want {
                    issues.push(err(
                        IssueCode::BadInput,
                        match self.kind {
                            RecipeKind::Each => "1 件ずつのレシピの入力は対象（Primary）です",
                            RecipeKind::Merge => "N → 1 のレシピの入力は選択（Selection）です",
                        },
                    ));
                }
                if slot.decode != DecodeOptions::default() {
                    issues.push(err(
                        IssueCode::BadGraph,
                        "レシピの入力の読み方は既定のものだけです",
                    ));
                }
            }
            _ => {
                if g.inputs
                    .iter()
                    .any(|i| matches!(i.role, InputRole::Fixed { .. }))
                {
                    issues.push(err(
                        IssueCode::BadInput,
                        "固定の素材を使うレシピは、この版では扱えません",
                    ));
                }
                issues.push(err(IssueCode::BadInput, "レシピの入力は 1 つだけです"));
            }
        }
        // 出力
        match g.outputs.as_slice() {
            [out] => {
                if out.expand != Expand::One {
                    issues.push(err(
                        IssueCode::BadGraph,
                        "レシピの出力は 1 つのファイルだけです",
                    ));
                }
                if out.format == FormatChoice::Keep && out.quantize.is_some() {
                    issues.push(err(
                        IssueCode::BadGraph,
                        "形式を決めない出力に量子化は付けられません",
                    ));
                }
            }
            _ => issues.push(err(IssueCode::BadGraph, "レシピの出力は 1 つだけです")),
        }
        // 結び付けの形
        if self.bindings.len() > MAX_BINDINGS {
            issues.push(err(
                IssueCode::BadGraph,
                &format!(
                    "結び付けが多すぎます（{} 個。上限 {MAX_BINDINGS}）",
                    self.bindings.len()
                ),
            ));
            return issues;
        }
        if self.kind == RecipeKind::Merge && !self.bindings.is_empty() {
            issues.push(err(
                IssueCode::BadGraph,
                "N → 1 のレシピは結び付けを持てません",
            ));
        }
        let mut keys: Vec<(NodeId, &str, Option<RangePart>)> =
            self.bindings.iter().map(binding_key).collect();
        keys.sort();
        for w in keys.windows(2) {
            let same_param = w[0].0 == w[1].0 && w[0].1 == w[1].1;
            if same_param && w[0].2 == w[1].2 {
                issues.push(
                    err(IssueCode::BadGraph, "同じ引数への結び付けが 2 つあります")
                        .at(w[0].0)
                        .param(w[0].1),
                );
            } else if same_param && w[0].2.is_none() {
                issues.push(
                    err(
                        IssueCode::BadGraph,
                        "同じ引数に、全体と片方の結び付けを両方は置けません",
                    )
                    .at(w[0].0)
                    .param(w[0].1),
                );
            }
        }
        for b in &self.bindings {
            let at = |code: IssueCode, msg: &str| {
                Issue::error(code, msg).at(b.node).param(b.param.clone())
            };
            match g.nodes.iter().find(|n| n.id == b.node) {
                None => issues.push(at(IssueCode::BadGraph, "ないノードへの結び付けです")),
                Some(n) if !n.enabled => issues.push(at(
                    IssueCode::BadParams,
                    "外したノードには結び付けを置けません",
                )),
                Some(_) => {}
            }
            match b.from {
                Source::Time { ms, .. } if ms > MAX_BIND_MS => issues.push(at(
                    IssueCode::OutOfRange,
                    "秒の結び付けが長すぎます（2^32 ms まで）",
                )),
                Source::Setting { .. } if b.part.is_some() => issues.push(at(
                    IssueCode::BadParams,
                    "素材の設定は区間の片方には結び付けられません",
                )),
                _ => {}
            }
        }
        issues
    }
}

impl Engine {
    /// レシピを登録表で確かめる（当てる前に毎回。DB から読んだものも）：名前・説明・形（[`Recipe::new`] と同じ）、
    /// ノードごとの操作の版と引数（知らない操作・新しすぎる版・移せない版・型と範囲）、結び付ける引数（その版の実装の
    /// 記述にあり、出どころの種類と引数の種類が合う）。指摘は集めて一度に返す。返すのは今の版でないノードの印。
    pub fn check_recipe(&self, recipe: &Recipe) -> Result<Vec<RecipeNote>, EngineError> {
        check_text(&recipe.name, &recipe.description)?;
        // 文書の形が壊れている（スキーマ・規則の版・ノードの数・参照）なら、操作の確かめはしない
        let structure = recipe.structure_issues(self.limits().max_nodes);
        if structure.has_errors() {
            return Err(EngineError::Invalid(structure));
        }
        let mut issues = recipe.shape_issues(self.limits().max_nodes);
        let registry = self.registry();
        let mut notes = Vec::new();
        // 版を解決できなかったノード（知らない操作・新しすぎる版・移せない版）。結び付けの確かめはしない（別の指摘を混ぜない）
        let mut unresolved: Vec<NodeId> = Vec::new();
        for node in &recipe.graph.nodes {
            match registry.resolve(&node.op, node.v, &node.params) {
                Ok(
                    crate::op::Resolved::Direct(imp, raw) | crate::op::Resolved::Migrated(imp, raw),
                ) => {
                    if let Err(found) = imp.parse(&raw) {
                        issues.extend(found.at(node.id));
                    }
                }
                Err(issue) => {
                    unresolved.push(node.id);
                    issues.push(issue.at(node.id));
                }
            }
            if let Ok(status) = registry.status(&node.op, node.v)
                && status != crate::op::OpStatus::Current
            {
                notes.push(RecipeNote {
                    node: node.id,
                    op: node.op.clone(),
                    v: node.v,
                    status,
                });
            }
        }
        for b in recipe
            .bindings
            .iter()
            .filter(|b| !unresolved.contains(&b.node))
        {
            bind::check_binding(registry, &recipe.graph, b, &mut issues);
        }
        if issues.has_errors() {
            return Err(EngineError::Invalid(issues));
        }
        recipe.file_len()?;
        Ok(notes)
    }
}
