//! 文書（docs/editing.md §4）。元に当てる操作のノードの並び。
//!
//! - ノードは位相順に並べ、入力は前のノードか文書の入力だけを指せる（循環しない）。
//! - 文書は常に「元」に対する累積で持つ（§4.2）。
//! - 保存は JSON。種（u64）は JS で精度が落ちないよう 10 進の文字列にする。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::digest::{Digest, hash_canonical};
use crate::error::{EngineError, Issue, IssueCode, Issues};

/// 今の文書のスキーマの版。全体の規則（メタデータの写し方・丸めなど）を変えたとき、文書の型に項目を足したときに上げる（§8.4）。
/// 文書の型は知らない項目を受け付けないので、新しい版の文書を古い版の Kura が黙って別の意味で読むことはない。
pub const SCHEMA: u32 = 1;

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
            PortRef::Input { id } => input_ids.binary_search(id).is_ok(),
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
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, EngineError> {
        crate::digest::canonical(self)
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

/// 文書を読む。スキーマが新しすぎればエラー、古ければ今のスキーマへ移す。
pub fn load(raw: &[u8]) -> Result<Graph, EngineError> {
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
    serde_json::from_value(value).map_err(|e| {
        EngineError::Invalid(Issues::one(Issue::error(
            IssueCode::BadGraph,
            format!("文書の形が正しくありません: {e}"),
        )))
    })
}

/// 古いスキーマの文書を今のスキーマへ移す。結果（出力）を変える移行は作らない（§8.4）。
fn migrate_schema(value: serde_json::Value, from: u32) -> Result<serde_json::Value, EngineError> {
    match from {
        SCHEMA => Ok(value),
        _ => Err(EngineError::Invalid(Issues::one(Issue::error(
            IssueCode::BadGraph,
            format!("スキーマ {from} の文書は読めません"),
        )))),
    }
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
    use serde_json::json;

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

    #[test]
    fn node_ids_do_not_overflow() {
        let mut g = Graph::new_single();
        g.next_id = u32::MAX;
        assert!(g.push_linear("image.flip", 1, json!({}), 0).is_err());
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
