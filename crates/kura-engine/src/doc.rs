//! 文書（docs/editing.md §4）。元に当てる操作のノードの並び。
//!
//! - ノードは位相順に並べ、入力は前のノードか文書の入力だけを指せる（循環しない）。
//! - 文書は常に「元」に対する累積で持つ（§4.2）。
//! - 保存は JSON。種（u64）は JS で精度が落ちないよう 10 進の文字列にする。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::digest::{Digest, hash_canonical};
use crate::error::{EngineError, Issue, IssueCode, Issues};

/// 今の文書のスキーマの版。文書の型（項目）を変えたときに上げ、`SCHEMA_STEPS` に移行を 1 つ足す（§8.4）。
/// 文書の型は知らない項目を受け付けないので、新しい版の文書を古い版の Kura が黙って別の意味で読むことはない。
/// - 2：`rules`（全体の規則の版）を足した。1 の文書は `rules: 1` として読む。
pub const SCHEMA: u32 = 2;

/// 今の全体の規則の版（メタデータの写し方・丸め・量子化など、どの操作にも効く約束。§8.4）。
/// 全体の規則を変えたら上げ、古い規則の実装も残す（`render` が文書の `rules` で選ぶ）。
/// スキーマの移行は文書の形だけを移し、この値は変えない（古い文書は古い規則で作り直す）。
pub const RULES: u32 = 1;

/// ノードの ID。文書の中で一意。消しても使い回さない。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

/// 文書の入力の ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputId(pub u16);

/// 文書の出力の ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutputId(pub u16);

/// 入力の役割。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", deny_unknown_fields)]
pub enum InputRole {
    /// 編集・一括の対象。アイテムの元に結び付く。
    Primary,
    /// 中身を固定した素材（透かし・パレットの元など）。
    Fixed { hash: Digest },
    /// 一括の選択全体（N → 1 の結合）。
    Selection,
}

/// 読み方の設定（出力に効くので文書に入れる）。形式ごとの項目は段 2 以降で足す。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct DecodeOptions {}

/// 文書の入力。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InputSlot {
    pub id: InputId,
    pub role: InputRole,
    #[serde(default)]
    pub decode: DecodeOptions,
}

/// ノードの入力の参照。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "from", deny_unknown_fields)]
pub enum PortRef {
    /// 文書の入力。
    Input { id: InputId },
    /// 前のノードの出力（出力の口は 1 つ。複数の結果は値の中の数で表す）。
    Node { id: NodeId },
}

fn yes() -> bool {
    true
}

/// 操作 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Node {
    pub id: NodeId,
    /// 操作の名前（例 `image.crop`）。
    pub op: String,
    /// 作ったときの操作の版。勝手に上げない。
    pub v: u16,
    /// 引数（その版の型で読む）。
    #[serde(default)]
    pub params: serde_json::Value,
    pub inputs: Vec<PortRef>,
    /// 乱数を使う操作の種。作るときに決め、移行しても変えない。
    #[serde(default, with = "u64_text")]
    pub seed: u64,
    /// false なら一時的に外す（入力 0 をそのまま通す）。
    #[serde(default = "yes")]
    pub enabled: bool,
}

/// 出力の形式。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", deny_unknown_fields)]
pub enum FormatChoice {
    /// 元と同じ形式。
    #[default]
    Keep,
    /// 形式を指定する（`options` はエンコーダの設定）。
    To {
        format: String,
        #[serde(default)]
        options: serde_json::Value,
    },
}

/// 出力をいくつのファイルにするか。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type", deny_unknown_fields)]
pub enum Expand {
    /// 1 つのファイル。
    #[default]
    One,
    /// フレーム・クリップごとに 1 つ（例 `"{stem}_{index:03}"`）。
    PerElement { name: String },
}

/// 文書の出力。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputSpec {
    pub id: OutputId,
    pub from: PortRef,
    #[serde(default)]
    pub format: FormatChoice,
    #[serde(default)]
    pub expand: Expand,
}

/// 文書。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Graph {
    pub schema: u32,
    /// この文書を作ったときの全体の規則の版。
    pub rules: u32,
    pub inputs: Vec<InputSlot>,
    pub nodes: Vec<Node>,
    pub outputs: Vec<OutputSpec>,
    /// 次に払い出すノード ID。
    pub next_id: u32,
}

impl Graph {
    /// 入力 1 つ（Primary）だけの空の文書。
    pub fn new_single() -> Self {
        Self {
            schema: SCHEMA,
            rules: RULES,
            inputs: vec![InputSlot {
                id: InputId(0),
                role: InputRole::Primary,
                decode: DecodeOptions::default(),
            }],
            nodes: Vec::new(),
            outputs: vec![OutputSpec {
                id: OutputId(0),
                from: PortRef::Input { id: InputId(0) },
                format: FormatChoice::Keep,
                expand: Expand::One,
            }],
            next_id: 1,
        }
    }

    /// 最後の出力の前に操作を 1 つつなぐ（線形の列を作るときの補助）。足したノードの ID を返す。ID を使い切ったらエラー。
    pub fn push_linear(
        &mut self,
        op: &str,
        v: u16,
        params: serde_json::Value,
        seed: u64,
    ) -> Result<NodeId, EngineError> {
        let id = NodeId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or_else(|| {
            EngineError::Invalid(Issues::one(Issue::error(
                IssueCode::TooLarge,
                "ノードの ID を使い切りました",
            )))
        })?;
        let from = self
            .outputs
            .first()
            .map(|o| o.from)
            .unwrap_or(PortRef::Input { id: InputId(0) });
        self.nodes.push(Node {
            id,
            op: op.to_string(),
            v,
            params,
            inputs: vec![from],
            seed,
            enabled: true,
        });
        if let Some(out) = self.outputs.first_mut() {
            out.from = PortRef::Node { id };
        }
        Ok(id)
    }

    /// 形の正しさ（ID の重複、前にないものの参照、出力の参照、next_id）を確かめる。
    pub fn check_structure(&self, max_nodes: u64) -> Issues {
        let mut issues = Issues::none();
        let bad = |msg: String| Issue::error(IssueCode::BadGraph, msg);
        if self.rules == 0 || self.rules > RULES {
            issues.push(Issue::error(
                IssueCode::SchemaTooNew,
                format!("この版の Kura が知らない規則の版です（{}）", self.rules),
            ));
            return issues;
        }
        if self.nodes.len() as u64 > max_nodes {
            issues.push(Issue::error(
                IssueCode::TooLarge,
                format!("ノードが多すぎます（{}）", self.nodes.len()),
            ));
            return issues;
        }
        let mut input_ids: Vec<InputId> = self.inputs.iter().map(|i| i.id).collect();
        input_ids.sort();
        if input_ids.windows(2).any(|w| w[0] == w[1]) {
            issues.push(bad("入力の ID が重複しています".into()));
        }
        let mut seen: Vec<NodeId> = Vec::with_capacity(self.nodes.len());
        let known = |r: &PortRef, seen: &[NodeId]| match r {
            PortRef::Input { id } => input_ids.contains(id),
            PortRef::Node { id } => seen.contains(id),
        };
        for node in &self.nodes {
            if seen.contains(&node.id) {
                issues.push(bad(format!("ノードの ID {} が重複しています", node.id.0)).at(node.id));
            }
            if node.id.0 >= self.next_id {
                issues.push(
                    bad(format!("ノードの ID {} が next_id 以上です", node.id.0)).at(node.id),
                );
            }
            for r in &node.inputs {
                if !known(r, &seen) {
                    issues
                        .push(bad("前にないノードか、ない入力を参照しています".into()).at(node.id));
                }
            }
            seen.push(node.id);
        }
        let mut out_ids: Vec<OutputId> = self.outputs.iter().map(|o| o.id).collect();
        out_ids.sort();
        if out_ids.windows(2).any(|w| w[0] == w[1]) {
            issues.push(bad("出力の ID が重複しています".into()));
        }
        if self.outputs.is_empty() {
            issues.push(bad("出力がありません".into()));
        }
        for out in &self.outputs {
            if !known(&out.from, &seen) {
                issues.push(bad("出力が、ないノードか入力を参照しています".into()));
            }
        }
        issues
    }

    /// 正規の JSON（キーを並べ、空白なし）。
    /// 大きさの上限（`MAX_DOC_BYTES`）を超える文書は、読み直せなくなるので書かずに `Limit` にする
    /// （エディタが追記した文書を保存して、開けなくなるのを防ぐ）。
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, EngineError> {
        crate::digest::canonical_within(self, MAX_DOC_BYTES).map_err(too_large)
    }

    /// 正規の JSON の大きさ（バイト）。エディタが上限に近づいたことを知らせるのに使う（書き出さずに数える）。
    pub fn encoded_len(&self) -> Result<usize, EngineError> {
        crate::digest::canonical_len_within(self, usize::MAX)
    }

    /// 文書のハッシュ。
    pub fn digest(&self) -> Result<Digest, EngineError> {
        hash_canonical("kura-doc-v1", self)
    }
}

impl Node {
    /// ノードのハッシュ（DB で内容アドレスとして持つとき。M12）。
    pub fn digest(&self) -> Result<Digest, EngineError> {
        hash_canonical("kura-node-v1", self)
    }
}

/// 文書の大きさの上限（バイト）。筆 1,000 本（各 50 点）の文書が約 0.3 MB なので、その 50 倍ほどの余裕を見る（ADR-147）。
pub const MAX_DOC_BYTES: usize = 16 << 20;

/// 保存の形（正規の JSON）で測った上限の誤りに、文書のことだと分かる文言を付ける。
fn too_large(e: EngineError) -> EngineError {
    match e {
        EngineError::Limit(m) => {
            EngineError::Limit(format!("文書を保存の形（正規の JSON）にできません：{m}"))
        }
        other => other,
    }
}

/// 文書を読む。大きすぎる・スキーマが新しすぎればエラー、古ければ今のスキーマへ移す。
/// 大きさは、読んだバイト列と、保存の形（正規の JSON。移行と既定値を入れたあと）の両方で確かめる。保存の形の大きさと
/// 入れ子の深さ（`digest::MAX_DEPTH`）は、保存（`to_canonical_json`）と同じ数え方なので、読めた文書は必ず保存でき、
/// 保存できた文書は必ず読める。
pub fn load(raw: &[u8]) -> Result<Graph, EngineError> {
    if raw.len() > MAX_DOC_BYTES {
        return Err(EngineError::Limit(format!(
            "文書が大きすぎます（{} バイト。上限 {MAX_DOC_BYTES}）",
            raw.len()
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(raw).map_err(|e| {
        EngineError::Invalid(Issues::one(Issue::error(
            IssueCode::BadGraph,
            format!("JSON が読めません: {e}"),
        )))
    })?;
    let schema = value
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            EngineError::Invalid(Issues::one(Issue::error(
                IssueCode::BadGraph,
                "schema がありません",
            )))
        })?;
    if schema > u64::from(SCHEMA) {
        return Err(EngineError::Invalid(Issues::one(Issue::error(
            IssueCode::SchemaTooNew,
            format!("この版の Kura より新しい文書です（スキーマ {schema}）"),
        ))));
    }
    let value = migrate_schema(value, schema as u32)?;
    let graph: Graph = serde_json::from_value(value).map_err(|e| {
        EngineError::Invalid(Issues::one(Issue::error(
            IssueCode::BadGraph,
            format!("文書の形が正しくありません: {e}"),
        )))
    })?;
    crate::digest::canonical_len_within(&graph, MAX_DOC_BYTES).map_err(too_large)?;
    Ok(graph)
}

/// スキーマを 1 段移す関数。
pub type SchemaStep = fn(serde_json::Value) -> Result<serde_json::Value, EngineError>;

/// スキーマの移行の表：`SCHEMA_STEPS[i]` はスキーマ `i + 1` の文書を `i + 2` へ移す。スキーマを上げたら 1 つ足す（消さない）。
const SCHEMA_STEPS: &[SchemaStep] = &[schema_1_to_2];

/// 1 → 2：`rules` を足す（1 の文書はすべて規則 1 で作った）。
fn schema_1_to_2(mut value: serde_json::Value) -> Result<serde_json::Value, EngineError> {
    if let Some(map) = value.as_object_mut()
        && !map.contains_key("rules")
    {
        map.insert("rules".into(), serde_json::Value::from(1));
    }
    Ok(value)
}

/// 古いスキーマの文書を今のスキーマへ移す。結果（出力）を変える移行は作らない（§8.4）。
fn migrate_schema(value: serde_json::Value, from: u32) -> Result<serde_json::Value, EngineError> {
    migrate_with(value, from, SCHEMA, SCHEMA_STEPS)
}

/// `from` から `to` まで、表の関数で 1 段ずつ移す（段ごとに `schema` も書き換える）。
pub fn migrate_with(
    mut value: serde_json::Value,
    from: u32,
    to: u32,
    steps: &[SchemaStep],
) -> Result<serde_json::Value, EngineError> {
    let bad =
        |msg: String| EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadGraph, msg)));
    if from == 0 || from > to {
        return Err(bad(format!("スキーマ {from} の文書は読めません")));
    }
    for v in from..to {
        let step = steps
            .get((v - 1) as usize)
            .ok_or_else(|| bad(format!("スキーマ {v} から移す方法がありません")))?;
        value = step(value)?;
        match value.as_object_mut() {
            Some(map) => {
                map.insert("schema".into(), serde_json::Value::from(v + 1));
            }
            None => return Err(bad("文書がオブジェクトではありません".into())),
        }
    }
    Ok(value)
}

/// 編集済みの文書（`base`）の出力 0 の後ろに、レシピ（`recipe`）の操作をつなぐ（docs/editing.md §4.3。一括加工の継ぎ足し）。
/// - レシピは入力 1 つ（Primary）の文書。その入力を `base` の出力 0 に結び付ける。
/// - レシピのノード ID は `base.next_id` だけずらす。種はそのまま（元のハッシュと合わせて乱数が決まるので、結果は継ぎ足しても変わらない）。
/// - 出力はレシピの出力（形式・展開もレシピのもの）。`base` のほかの出力は捨てる。
pub fn compose(base: &Graph, recipe: &Graph) -> Result<Graph, EngineError> {
    let bad = |msg: &str| EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadGraph, msg)));
    if base.schema != SCHEMA || recipe.schema != SCHEMA {
        return Err(bad("今のスキーマに移してからつなぎます"));
    }
    // 1 つの文書は 1 つの規則で描くので、規則の版の違う文書はつなげない
    if base.rules != recipe.rules {
        return Err(bad("規則の版の違う文書はつなげません"));
    }
    let [slot] = recipe.inputs.as_slice() else {
        return Err(bad("レシピの入力は 1 つだけです"));
    };
    if slot.role != InputRole::Primary {
        return Err(bad("レシピの入力は対象（Primary）です"));
    }
    let attach = base
        .outputs
        .first()
        .map(|o| o.from)
        .ok_or_else(|| bad("つなぐ先の出力がありません"))?;
    let offset = base.next_id;
    let shift = |id: NodeId| {
        id.0.checked_add(offset)
            .map(NodeId)
            .ok_or_else(|| bad("ノードの ID を使い切りました"))
    };
    let remap = |r: PortRef| -> Result<PortRef, EngineError> {
        match r {
            PortRef::Input { id } if id == slot.id => Ok(attach),
            PortRef::Input { .. } => Err(bad("レシピがない入力を参照しています")),
            PortRef::Node { id } => Ok(PortRef::Node { id: shift(id)? }),
        }
    };
    let mut nodes = base.nodes.clone();
    for n in &recipe.nodes {
        let inputs = n
            .inputs
            .iter()
            .map(|r| remap(*r))
            .collect::<Result<Vec<_>, _>>()?;
        nodes.push(Node {
            id: shift(n.id)?,
            inputs,
            ..n.clone()
        });
    }
    let outputs = recipe
        .outputs
        .iter()
        .map(|o| {
            Ok(OutputSpec {
                from: remap(o.from)?,
                ..o.clone()
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;
    let next_id = offset
        .checked_add(recipe.next_id)
        .ok_or_else(|| bad("ノードの ID を使い切りました"))?;
    Ok(Graph {
        schema: SCHEMA,
        rules: base.rules,
        inputs: base.inputs.clone(),
        nodes,
        outputs,
        next_id,
    })
}

/// u64 を 10 進の文字列で書く（JS の数で精度が落ちないように）。読むときは数でも受け付ける。
mod u64_text {
    use super::*;

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Text {
            Str(String),
            Num(u64),
        }
        match Text::deserialize(d)? {
            Text::Str(s) => s.parse().map_err(serde::de::Error::custom),
            Text::Num(n) => Ok(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上限を超える文書は保存の形にもしない（作れても読み直せなくなるため）。
    #[test]
    fn huge_documents_are_not_written() {
        let mut g = Graph::new_single();
        let big = "a".repeat(MAX_DOC_BYTES);
        g.push_linear("image.flip", 1, json!({ "axis": big }), 0)
            .unwrap();
        assert!(matches!(g.to_canonical_json(), Err(EngineError::Limit(_))));
        assert!(g.encoded_len().unwrap() > MAX_DOC_BYTES);
    }

    #[test]
    fn huge_documents_are_refused_before_parsing() {
        let mut raw = br#"{"schema":2,"rules":1,"pad":""#.to_vec();
        raw.resize(MAX_DOC_BYTES + 1, b'a');
        assert!(matches!(load(&raw), Err(EngineError::Limit(_))));
    }

    /// 保存の形で何十倍にも膨らむ文書（1e300 は正規の形で 301 桁）も、読むときに断る。読めた文書は必ず保存できる。
    #[test]
    fn documents_that_grow_when_written_are_refused() {
        let mut v: serde_json::Value =
            serde_json::from_slice(&sample().to_canonical_json().unwrap()).unwrap();
        let pad = format!("[{}]", vec!["1e300"; 60_000].join(","));
        v["nodes"][0]["params"]["pad"] = serde_json::from_str(&pad).unwrap();
        let raw = serde_json::to_vec(&v).unwrap();
        assert!(raw.len() < MAX_DOC_BYTES);
        assert!(matches!(load(&raw), Err(EngineError::Limit(_))));
        // 少なければ読めて、保存もできる
        let pad = format!("[{}]", vec!["1e300"; 100].join(","));
        v["nodes"][0]["params"]["pad"] = serde_json::from_str(&pad).unwrap();
        let g = load(&serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(g.to_canonical_json().is_ok());
    }
    use serde_json::json;

    /// 入れ子の深さも、保存と読み込みで同じ数え方：保存できない深さの文書は読まない（serde_json の 128 段より浅くても）。
    #[test]
    fn deep_documents_are_refused_on_both_sides() {
        let mut deep = json!(1);
        for _ in 0..crate::digest::MAX_DEPTH {
            deep = json!([deep]);
        }
        let mut g = sample();
        g.nodes[0].params["pad"] = deep;
        assert!(matches!(g.to_canonical_json(), Err(EngineError::Limit(_))));
        let raw = serde_json::to_vec(&serde_json::to_value(&g).unwrap()).unwrap();
        assert!(matches!(load(&raw), Err(EngineError::Limit(_))));
        // 数段浅ければ、保存も読み込みもできる
        let mut shallow = json!(1);
        for _ in 0..crate::digest::MAX_DEPTH - 10 {
            shallow = json!([shallow]);
        }
        g.nodes[0].params["pad"] = shallow;
        let bytes = g.to_canonical_json().unwrap();
        assert_eq!(load(&bytes).unwrap().to_canonical_json().unwrap(), bytes);
    }

    fn sample() -> Graph {
        let mut g = Graph::new_single();
        g.push_linear(
            "image.crop",
            1,
            json!({ "rect": { "x": 1, "y": 1, "w": 2, "h": 2 } }),
            0,
        )
        .unwrap();
        g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), u64::MAX)
            .unwrap();
        g
    }

    #[test]
    fn json_round_trip_keeps_seed_exact() {
        let g = sample();
        let bytes = g.to_canonical_json().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\"seed\":\"18446744073709551615\""), "{text}");
        let back = load(&bytes).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.digest().unwrap(), g.digest().unwrap());
    }

    #[test]
    fn structure_is_checked() {
        let mut g = sample();
        assert!(g.check_structure(100).is_empty());
        g.nodes[0].inputs = vec![PortRef::Node { id: NodeId(2) }];
        assert!(g.check_structure(100).has_errors());
        let mut g = sample();
        g.next_id = 1;
        assert!(g.check_structure(100).has_errors());
        assert!(sample().check_structure(1).has_errors());
    }

    #[test]
    fn newer_or_broken_documents_are_rejected_without_panic() {
        let too_new =
            json!({ "schema": SCHEMA + 1, "inputs": [], "nodes": [], "outputs": [], "nextId": 1 });
        let err = load(too_new.to_string().as_bytes()).unwrap_err();
        assert!(
            matches!(&err, EngineError::Invalid(i) if i.0[0].code == IssueCode::SchemaTooNew),
            "{err}"
        );
        assert!(load(b"{").is_err());
        assert!(load(b"{}").is_err());
        assert!(load(br#"{"schema":1,"inputs":"x"}"#).is_err());
    }

    /// 知らない項目を含む文書は読み込みでエラーにする（新しい版の項目を黙って捨てない）。
    #[test]
    fn unknown_fields_are_rejected() {
        let base = sample().to_canonical_json().unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&base).unwrap();
        assert!(load(&serde_json::to_vec(&v).unwrap()).is_ok());
        v["outputs"][0]["quantize"] = json!({ "bits": 8 });
        assert!(load(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut v: serde_json::Value = serde_json::from_slice(&base).unwrap();
        v["inputs"][0]["decode"] = json!({ "scale": 2 });
        assert!(load(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut v: serde_json::Value = serde_json::from_slice(&base).unwrap();
        v["future"] = json!(1);
        assert!(load(&serde_json::to_vec(&v).unwrap()).is_err());
        let mut v: serde_json::Value = serde_json::from_slice(&base).unwrap();
        v["nodes"][0]["inputs"][0]["port"] = json!(1);
        assert!(load(&serde_json::to_vec(&v).unwrap()).is_err());
    }

    /// スキーマ 1 の文書は規則 1 として読む。知らない規則の版は形の確かめで断る。
    #[test]
    fn schema_1_documents_get_rules_1_and_unknown_rules_are_rejected() {
        let mut v: serde_json::Value =
            serde_json::from_slice(&sample().to_canonical_json().unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("rules");
        // rules のないスキーマ 2 の文書は形が正しくない
        assert!(load(&serde_json::to_vec(&v).unwrap()).is_err());
        v["schema"] = json!(1);
        let g = load(&serde_json::to_vec(&v).unwrap()).unwrap();
        assert_eq!((g.schema, g.rules), (SCHEMA, 1));
        assert_eq!(g, sample());
        let mut g2 = g.clone();
        g2.rules = RULES + 1;
        assert!(g2.check_structure(100).has_errors());
        g2.rules = 0;
        assert!(g2.check_structure(100).has_errors());
    }

    #[test]
    fn node_ids_do_not_overflow() {
        let mut g = Graph::new_single();
        g.next_id = u32::MAX;
        assert!(g.push_linear("image.flip", 1, json!({}), 0).is_err());
    }

    /// スキーマの移行は 1 段ずつ、表の関数で行う（E7 の文書の側）。
    #[test]
    fn schema_migrations_chain_step_by_step() {
        fn v1_to_v2(mut v: serde_json::Value) -> Result<serde_json::Value, EngineError> {
            if let Some(m) = v.as_object_mut()
                && let Some(x) = m.remove("old")
            {
                m.insert("mid".into(), x);
            }
            Ok(v)
        }
        fn v2_to_v3(mut v: serde_json::Value) -> Result<serde_json::Value, EngineError> {
            if let Some(m) = v.as_object_mut()
                && let Some(x) = m.remove("mid")
            {
                m.insert("new".into(), x);
            }
            Ok(v)
        }
        let steps: &[SchemaStep] = &[v1_to_v2, v2_to_v3];
        let moved = migrate_with(json!({ "schema": 1, "old": 7 }), 1, 3, steps).unwrap();
        assert_eq!(moved, json!({ "schema": 3, "new": 7 }));
        assert_eq!(
            migrate_with(json!({ "schema": 2, "mid": 7 }), 2, 3, steps).unwrap(),
            json!({ "schema": 3, "new": 7 })
        );
        assert!(migrate_with(json!({}), 1, 4, steps).is_err());
        assert!(migrate_with(json!({}), 0, 1, steps).is_err());
        assert_eq!(
            migrate_with(json!({ "schema": 1 }), 1, 1, steps).unwrap(),
            json!({ "schema": 1 })
        );
    }

    /// 継ぎ足し：レシピのノードは ID をずらして base の出力 0 につながり、出力はレシピのもの。
    #[test]
    fn compose_appends_recipe_after_base() {
        let base = sample();
        let mut recipe = Graph::new_single();
        recipe
            .push_linear("image.flip", 1, json!({ "axis": "vertical" }), 9)
            .unwrap();
        recipe.outputs[0].format = FormatChoice::To {
            format: "png".into(),
            options: json!({}),
        };
        let g = compose(&base, &recipe).unwrap();
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.nodes[2].id, NodeId(base.next_id + 1));
        assert_eq!(g.nodes[2].inputs, vec![PortRef::Node { id: NodeId(2) }]);
        assert_eq!(g.nodes[2].seed, 9);
        assert_eq!(
            g.outputs[0].from,
            PortRef::Node {
                id: NodeId(base.next_id + 1)
            }
        );
        assert!(matches!(g.outputs[0].format, FormatChoice::To { .. }));
        assert!(g.check_structure(100).is_empty());
        // 空の文書にレシピをつなぐと、レシピと同じ形
        let empty = Graph::new_single();
        let g2 = compose(&empty, &recipe).unwrap();
        assert_eq!(g2.nodes[0].inputs, vec![PortRef::Input { id: InputId(0) }]);
        let mut two_inputs = recipe.clone();
        two_inputs.inputs.push(InputSlot {
            id: InputId(1),
            role: InputRole::Selection,
            decode: DecodeOptions::default(),
        });
        assert!(compose(&base, &two_inputs).is_err());
    }

    #[test]
    fn seed_accepts_numbers_too() {
        let raw = json!({
            "schema": 1, "inputs": [{ "id": 0, "role": { "type": "primary" } }],
            "nodes": [{ "id": 1, "op": "image.flip", "v": 1, "params": {}, "inputs": [{ "from": "input", "id": 0 }], "seed": 5 }],
            "outputs": [{ "id": 0, "from": { "from": "node", "id": 1 } }], "nextId": 2
        });
        let g = load(raw.to_string().as_bytes()).unwrap();
        assert_eq!(g.nodes[0].seed, 5);
        assert!(g.nodes[0].enabled);
    }
}
