//! 文書を DB に内容アドレスの部品で置くための分け方とつなぎ方（docs/editing.md §4.4。M12 の段 2）。
//!
//! 筆の点の多い文書を版ごとに丸ごと持つと、容量が保存の回数に比例して増える。そこで文書を 3 種類の部品に分け、同じ中身の
//! 部品は 1 つだけ持つ（追記した版どうしで、前の切れ目とほかのノードを共有する）。形は凍結（`parts_format_is_frozen`。E34）：
//!
//! | 部品 | 本文 | ドメイン | 子 |
//! |---|---|---|---|
//! | chunk | 配列 1 つ（1〜[`PART_CHUNK`] 要素。元の配列の `[64k, 64k+64)`） | `kura-doc-chunk-v1` | なし |
//! | node | ノードのオブジェクト。切った引数の値は `[]`、最上位に `"$chunks": {<引数名>: {"count": N, "parts": ["<hex>", …]}}` | `kura-doc-node-v1` | 切れ目（引数名の順 → 切れ目の順） |
//! | doc | 文書のオブジェクト。`"nodes"` をノードの部品の 16 進の配列にしたもの | `kura-doc-root-v1` | ノード（文書の順） |
//!
//! - 本文は正規の JSON（`digest::canonical_json`）、ダイジェストは `Hasher::new(<ドメイン>).bytes_with_len(本文)`。
//! - 切る規則：ノードの `params` の最上位の値のうち、要素が [`PART_CHUNK`] を超える配列を、位置 0 から 64 個ずつ切る。登録表に
//!   依らない（`Engine` なしで呼べる）。64 個以下の配列は切らない。切れ目の境界は追記だけの配列（`session::log::CHUNK`）と同じ。
//! - つなぐとき（[`join`]）は、各部品の本文からダイジェストを求め直して照らし、本文が正規の JSON であること・`count` と切れ目の
//!   数と長さ・殻が `[]` であること・切っていない長い配列がないことを確かめる（同じ文書は必ず同じ根になり、根が同じなら文書も
//!   同じ）。壊れていれば `Invalid`（`BadGraph`）。出来たバイト列は文書の読み込み（`Engine::load`）に通して使う。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::digest::{Digest, Hasher, canonical_json, canonical_len_within};
use crate::doc::{Graph, MAX_DOC_BYTES};
use crate::error::{EngineError, Issue, IssueCode, Issues};

/// 切れ目 1 つの要素の数（凍結。追記だけの配列 `session::log::CHUNK` と同じ値。テストで照らす）。
pub const PART_CHUNK: usize = 64;

/// ノードの部品で、切った引数の切れ目を書く最上位のキー（ノードの型は知らない項目を受け付けないので、項目とぶつからない）。
const CHUNKS_KEY: &str = "$chunks";

/// つなぐときに読む本文の合計の上限（バイト）。同じ切れ目を何度も参照して膨らませる部品の組を、組み立てる前に断る。
/// 出来上がる文書は参照した本文の合計より小さいので、上限の文書（`MAX_DOC_BYTES`）に参照の 16 進の分を足しても収まる 2 倍。
const JOIN_BUDGET: usize = 2 * MAX_DOC_BYTES;

/// 部品の種類（DB の `edit_objects.kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PartKind {
    /// 文書（根）。
    Doc,
    /// ノード。
    Node,
    /// 長い配列の切れ目。
    Chunk,
}

impl PartKind {
    /// DB に書く名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Doc => "doc",
            Self::Node => "node",
            Self::Chunk => "chunk",
        }
    }

    /// DB の名前から戻す（知らない名前は None）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "doc" => Some(Self::Doc),
            "node" => Some(Self::Node),
            "chunk" => Some(Self::Chunk),
            _ => None,
        }
    }

    /// ダイジェストのドメイン（凍結）。
    fn domain(self) -> &'static str {
        match self {
            Self::Doc => "kura-doc-root-v1",
            Self::Node => "kura-doc-node-v1",
            Self::Chunk => "kura-doc-chunk-v1",
        }
    }
}

/// 部品 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub digest: Digest,
    pub kind: PartKind,
    /// 正規の JSON（UTF-8）。
    pub body: Vec<u8>,
    /// 子の部品（本文に書いた順。同じ子が 2 度出てもよい）。
    pub children: Vec<Digest>,
}

/// 文書を分けた部品の組。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocParts {
    /// 根（doc の部品）のダイジェスト。
    pub root: Digest,
    /// 子が先に並ぶ（親を書くときには子が書いてある）。同じダイジェストの部品は 1 つだけ。
    pub parts: Vec<Part>,
}

/// 部品のダイジェスト（ドメイン付きの BLAKE3 に長さ付きの本文）。
pub fn part_digest(kind: PartKind, body: &[u8]) -> Digest {
    Hasher::new(kind.domain()).bytes_with_len(body).finish()
}

/// 文書を部品に分ける。保存の形（正規の JSON）にできない文書（大きさ・深さ・有限でない数）は、文書の保存と同じく断る。
pub fn split(g: &Graph) -> Result<DocParts, EngineError> {
    canonical_len_within(g, MAX_DOC_BYTES).map_err(|e| match e {
        EngineError::Limit(m) => EngineError::Limit(format!("文書を部品に分けられません：{m}")),
        other => other,
    })?;
    let mut doc = match serde_json::to_value(g) {
        Ok(Value::Object(map)) => map,
        Ok(_) => return Err(internal("文書がオブジェクトになりません")),
        Err(e) => return Err(internal(&format!("文書を JSON にできません: {e}"))),
    };
    let nodes = match doc.remove("nodes") {
        Some(Value::Array(nodes)) => nodes,
        _ => return Err(internal("文書に nodes の配列がありません")),
    };
    let mut out = Parts::default();
    let mut node_refs = Vec::with_capacity(nodes.len());
    let mut node_digests = Vec::with_capacity(nodes.len());
    for node in nodes {
        let Value::Object(mut node) = node else {
            return Err(internal("ノードがオブジェクトになりません"));
        };
        let mut chunks = Map::new();
        let mut children = Vec::new();
        if let Some(Value::Object(params)) = node.get_mut("params") {
            let mut long: Vec<String> = params
                .iter()
                .filter(|(_, v)| matches!(v, Value::Array(a) if a.len() > PART_CHUNK))
                .map(|(k, _)| k.clone())
                .collect();
            // 引数名の順（UTF-8 のバイト順。正規の JSON のキーの並びと同じ）
            long.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            for name in long {
                let Some(Value::Array(items)) =
                    params.insert(name.clone(), Value::Array(Vec::new()))
                else {
                    return Err(internal("切る引数が配列ではありません"));
                };
                let count = items.len();
                let mut refs = Vec::with_capacity(count.div_ceil(PART_CHUNK));
                let mut rest = items.into_iter();
                loop {
                    let piece: Vec<Value> = rest.by_ref().take(PART_CHUNK).collect();
                    if piece.is_empty() {
                        break;
                    }
                    let body = canonical_json(&Value::Array(piece))?;
                    let digest = out.push(PartKind::Chunk, body, Vec::new());
                    refs.push(Value::String(digest.to_hex()));
                    children.push(digest);
                }
                let mut spec = Map::new();
                spec.insert("count".into(), Value::from(count));
                spec.insert("parts".into(), Value::Array(refs));
                chunks.insert(name, Value::Object(spec));
            }
        }
        if !chunks.is_empty() {
            node.insert(CHUNKS_KEY.into(), Value::Object(chunks));
        }
        let body = canonical_json(&Value::Object(node))?;
        let digest = out.push(PartKind::Node, body, children);
        node_refs.push(Value::String(digest.to_hex()));
        node_digests.push(digest);
    }
    doc.insert("nodes".into(), Value::Array(node_refs));
    let body = canonical_json(&Value::Object(doc))?;
    let root = out.push(PartKind::Doc, body, node_digests);
    Ok(DocParts {
        root,
        parts: out.parts,
    })
}

/// 部品をつないで、文書の正規の JSON のバイト列に戻す（`root` から子をたどる。`parts` にほかの部品があってもよい）。
/// 部品が足りない・種類が違う・本文がダイジェストと合わない・正規の JSON でない・切れ目の数や長さが合わない・殻が `[]` で
/// ない・切っていない長い配列があれば `Invalid`（`BadGraph`）、大きすぎれば `Limit`。
pub fn join(
    root: &Digest,
    parts: &BTreeMap<Digest, (PartKind, Vec<u8>)>,
) -> Result<Vec<u8>, EngineError> {
    let mut budget = JOIN_BUDGET;
    let mut doc = read_object(root, PartKind::Doc, parts, &mut budget)?;
    let refs = match doc.get_mut("nodes") {
        Some(Value::Array(refs)) => std::mem::take(refs),
        _ => {
            return Err(bad(format!(
                "文書の部品 {root} に nodes の配列がありません"
            )));
        }
    };
    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(refs.len())
        .map_err(|_| EngineError::Limit("文書のノードが多すぎます".into()))?;
    for r in &refs {
        let digest = reference(r)?;
        let mut node = read_object(&digest, PartKind::Node, parts, &mut budget)?;
        let chunks = match node.remove(CHUNKS_KEY) {
            None => None,
            Some(Value::Object(chunks)) if !chunks.is_empty() => Some(chunks),
            Some(_) => {
                return Err(bad(format!(
                    "ノードの部品 {digest} の {CHUNKS_KEY} が空でないオブジェクトではありません"
                )));
            }
        };
        if let Some(Value::Object(params)) = node.get(CHUNKS_KEY_PARAMS) {
            // 切っていない長い配列は、分けた形として正しくない（同じ文書が別の根になる）
            if let Some((name, _)) = params
                .iter()
                .find(|(_, v)| matches!(v, Value::Array(a) if a.len() > PART_CHUNK))
            {
                return Err(bad(format!(
                    "ノードの部品 {digest} の引数 {name} が切られていません"
                )));
            }
        }
        if let Some(chunks) = chunks {
            let Some(Value::Object(params)) = node.get_mut(CHUNKS_KEY_PARAMS) else {
                return Err(bad(format!(
                    "ノードの部品 {digest} に切った引数があるのに params のオブジェクトがありません"
                )));
            };
            for (name, spec) in chunks {
                let items = read_chunks(&digest, &name, &spec, parts, &mut budget)?;
                match params.get_mut(&name) {
                    Some(Value::Array(shell)) if shell.is_empty() => *shell = items,
                    _ => {
                        return Err(bad(format!(
                            "ノードの部品 {digest} の引数 {name} の殻が [] ではありません"
                        )));
                    }
                }
            }
        }
        nodes.push(Value::Object(node));
    }
    doc.insert("nodes".into(), Value::Array(nodes));
    let doc = Value::Object(doc);
    canonical_len_within(&doc, MAX_DOC_BYTES).map_err(|e| match e {
        EngineError::Limit(m) => {
            EngineError::Limit(format!("部品をつないだ文書が大きすぎます：{m}"))
        }
        other => other,
    })?;
    canonical_json(&doc)
}

/// ノードの引数のキー。
const CHUNKS_KEY_PARAMS: &str = "params";

/// 引数 1 つの切れ目を読んで、元の配列に戻す（`count` と切れ目の数・長さを照らす）。
fn read_chunks(
    node: &Digest,
    name: &str,
    spec: &Value,
    parts: &BTreeMap<Digest, (PartKind, Vec<u8>)>,
    budget: &mut usize,
) -> Result<Vec<Value>, EngineError> {
    let fail = |what: &str| {
        bad(format!(
            "ノードの部品 {node} の引数 {name} の切れ目：{what}"
        ))
    };
    let Value::Object(spec) = spec else {
        return Err(fail("オブジェクトではありません"));
    };
    if spec.len() != 2 {
        return Err(fail("count と parts のほかの項目があります"));
    }
    let count = spec
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|c| usize::try_from(c).ok())
        .ok_or_else(|| fail("count が数ではありません"))?;
    let Some(Value::Array(refs)) = spec.get("parts") else {
        return Err(fail("parts が配列ではありません"));
    };
    if count <= PART_CHUNK {
        return Err(fail(&format!("{count} 個は切らない長さです")));
    }
    let want = count.div_ceil(PART_CHUNK);
    if refs.len() != want {
        return Err(fail(&format!(
            "{count} 個なら切れ目は {want} 個（{} 個あります）",
            refs.len()
        )));
    }
    let last = want
        .checked_sub(1)
        .and_then(|n| n.checked_mul(PART_CHUNK))
        .and_then(|before| count.checked_sub(before))
        .ok_or_else(|| fail("長さを数えられません"))?;
    let mut items: Vec<Value> = Vec::new();
    for (i, r) in refs.iter().enumerate() {
        let digest = reference(r)?;
        let piece = match read_part(&digest, PartKind::Chunk, parts, budget)? {
            Value::Array(piece) => piece,
            _ => return Err(fail(&format!("{digest} が配列ではありません"))),
        };
        let expect = if i + 1 == want { last } else { PART_CHUNK };
        if piece.len() != expect {
            return Err(fail(&format!(
                "{} 番目（{digest}）は {expect} 個のはず（{} 個あります）",
                i + 1,
                piece.len()
            )));
        }
        items
            .try_reserve(piece.len())
            .map_err(|_| EngineError::Limit("切れ目をつなげません（メモリ）".into()))?;
        items.extend(piece);
    }
    Ok(items)
}

/// 子の参照（64 桁の小文字の 16 進の文字列）を読む。
fn reference(v: &Value) -> Result<Digest, EngineError> {
    v.as_str()
        .and_then(Digest::from_hex)
        .ok_or_else(|| bad("子の部品の参照が 64 桁の小文字の 16 進ではありません".into()))
}

/// オブジェクトの部品を読む。
fn read_object(
    digest: &Digest,
    kind: PartKind,
    parts: &BTreeMap<Digest, (PartKind, Vec<u8>)>,
    budget: &mut usize,
) -> Result<Map<String, Value>, EngineError> {
    match read_part(digest, kind, parts, budget)? {
        Value::Object(map) => Ok(map),
        _ => Err(bad(format!(
            "{} の部品 {digest} がオブジェクトではありません",
            kind.as_str()
        ))),
    }
}

/// 部品を 1 つ読む：あること・種類・本文からのダイジェスト・本文が正規の JSON であることを確かめる。
fn read_part(
    digest: &Digest,
    kind: PartKind,
    parts: &BTreeMap<Digest, (PartKind, Vec<u8>)>,
    budget: &mut usize,
) -> Result<Value, EngineError> {
    let Some((found, body)) = parts.get(digest) else {
        return Err(bad(format!(
            "{} の部品 {digest} がありません",
            kind.as_str()
        )));
    };
    if *found != kind {
        return Err(bad(format!(
            "部品 {digest} の種類が {}（{} のはず）",
            found.as_str(),
            kind.as_str()
        )));
    }
    if part_digest(kind, body) != *digest {
        return Err(bad(format!(
            "{} の部品 {digest} の本文がダイジェストと合いません",
            kind.as_str()
        )));
    }
    *budget = budget.checked_sub(body.len()).ok_or_else(|| {
        EngineError::Limit(format!(
            "部品の本文の合計が大きすぎます（上限 {JOIN_BUDGET} バイト）"
        ))
    })?;
    let value: Value = serde_json::from_slice(body).map_err(|e| {
        bad(format!(
            "{} の部品 {digest} の本文が JSON ではありません: {e}",
            kind.as_str()
        ))
    })?;
    if canonical_json(&value)? != *body {
        return Err(bad(format!(
            "{} の部品 {digest} の本文が正規の JSON ではありません",
            kind.as_str()
        )));
    }
    Ok(value)
}

/// 分けた部品を集める（同じダイジェストは 1 つ。子を先に入れる）。
#[derive(Default)]
struct Parts {
    parts: Vec<Part>,
    seen: BTreeSet<Digest>,
}

impl Parts {
    fn push(&mut self, kind: PartKind, body: Vec<u8>, children: Vec<Digest>) -> Digest {
        let digest = part_digest(kind, &body);
        if self.seen.insert(digest) {
            self.parts.push(Part {
                digest,
                kind,
                body,
                children,
            });
        }
        digest
    }
}

fn bad(message: String) -> EngineError {
    EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadGraph, message)))
}

fn internal(message: &str) -> EngineError {
    EngineError::Internal(format!("文書の部品：{message}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{
        DecodeOptions, Expand, FormatChoice, InputId, InputRole, InputSlot, Node, NodeId, OutputId,
        OutputSpec, PortRef,
    };
    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::Rng;
    use serde_json::json;

    /// 部品を DB から読んだ形（ダイジェスト → 種類と本文）にする。
    fn as_map(p: &DocParts) -> BTreeMap<Digest, (PartKind, Vec<u8>)> {
        p.parts
            .iter()
            .map(|part| (part.digest, (part.kind, part.body.clone())))
            .collect()
    }

    /// 筆を `strokes` 本持つ描画のノードと、短い引数のノードの文書（スキーマ 3・規則 1 の形を手で組む）。
    fn doc_with(strokes: usize) -> Graph {
        let actions: Vec<Value> = (0..strokes)
            .map(|i| json!({ "t": "dot", "x": i, "y": i % 7, "c": [255, 0, 0, 255] }))
            .collect();
        let node = |id: u32, op: &str, params: Value, from: PortRef| Node {
            id: NodeId(id),
            op: op.into(),
            v: 1,
            params,
            inputs: vec![from],
            seed: 7,
            enabled: true,
        };
        Graph {
            schema: 3,
            rules: 1,
            inputs: vec![InputSlot {
                id: InputId(0),
                role: InputRole::Primary,
                decode: DecodeOptions::default(),
            }],
            nodes: vec![
                node(
                    1,
                    "image.crop",
                    json!({ "x": 1, "y": 2, "w": 30, "h": 20 }),
                    PortRef::Input { id: InputId(0) },
                ),
                node(
                    2,
                    "paint.actions",
                    json!({ "actions": actions, "short": [1, 2, 3] }),
                    PortRef::Node { id: NodeId(1) },
                ),
            ],
            outputs: vec![OutputSpec {
                id: OutputId(0),
                from: PortRef::Node { id: NodeId(2) },
                format: FormatChoice::Keep,
                quantize: None,
                expand: Expand::One,
            }],
            next_id: 3,
        }
    }

    #[test]
    fn part_chunk_matches_the_append_log() {
        assert_eq!(PART_CHUNK, crate::session::log::CHUNK);
    }

    #[test]
    fn kinds_round_trip_and_unknown_kinds_are_refused() {
        for kind in [PartKind::Doc, PartKind::Node, PartKind::Chunk] {
            assert_eq!(PartKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(PartKind::parse("blob"), None);
        assert_eq!(PartKind::parse("Doc"), None);
    }

    #[test]
    fn split_and_join_round_trip() {
        for strokes in [0, 1, 63, 64, 65, 128, 129, 200] {
            let g = doc_with(strokes);
            let parts = split(&g).unwrap();
            let joined = join(&parts.root, &as_map(&parts)).unwrap();
            assert_eq!(joined, g.to_canonical_json().unwrap(), "{strokes} 本");
            assert_eq!(crate::doc::load(&joined).unwrap(), g);
            // 子が先・同じダイジェストは 1 つ・根は最後
            let mut seen = BTreeSet::new();
            for p in &parts.parts {
                for c in &p.children {
                    assert!(seen.contains(c), "{strokes} 本：子が親より後");
                }
                assert!(seen.insert(p.digest));
                assert_eq!(p.digest, part_digest(p.kind, &p.body));
            }
            assert_eq!(parts.parts.last().unwrap().digest, parts.root);
            let chunks = parts
                .parts
                .iter()
                .filter(|p| p.kind == PartKind::Chunk)
                .count();
            let want = if strokes > PART_CHUNK {
                strokes.div_ceil(PART_CHUNK)
            } else {
                0
            };
            assert_eq!(chunks, want, "{strokes} 本");
            // 分け直しても同じ（決定的）
            assert_eq!(split(&g).unwrap(), parts);
        }
    }

    #[test]
    fn short_params_stay_in_the_node_and_long_ones_become_empty_shells() {
        let parts = split(&doc_with(130)).unwrap();
        let node = parts
            .parts
            .iter()
            .find(|p| p.kind == PartKind::Node && p.children.len() == 3)
            .unwrap();
        let v: Value = serde_json::from_slice(&node.body).unwrap();
        assert_eq!(v["params"]["actions"], json!([]));
        assert_eq!(v["params"]["short"], json!([1, 2, 3]));
        assert_eq!(v[CHUNKS_KEY]["actions"]["count"], json!(130));
        let refs: Vec<String> = node.children.iter().map(Digest::to_hex).collect();
        assert_eq!(v[CHUNKS_KEY]["actions"]["parts"], json!(refs));
        // 切らないノードの本文は、ノードの正規の JSON そのまま
        let plain = parts
            .parts
            .iter()
            .find(|p| p.kind == PartKind::Node && p.children.is_empty())
            .unwrap();
        assert_eq!(
            plain.body,
            crate::digest::canonical(&doc_with(0).nodes[0]).unwrap()
        );
    }

    /// 追記した版どうしで、前の切れ目を共有する：200 本 → 210 本で新しい部品は最後の切れ目・描画のノード・根だけ。
    #[test]
    fn appending_shares_all_but_the_last_chunk() {
        let before = split(&doc_with(200)).unwrap();
        let after = split(&doc_with(210)).unwrap();
        let old: BTreeSet<Digest> = before.parts.iter().map(|p| p.digest).collect();
        let new: Vec<PartKind> = after
            .parts
            .iter()
            .filter(|p| !old.contains(&p.digest))
            .map(|p| p.kind)
            .collect();
        assert_eq!(new, vec![PartKind::Chunk, PartKind::Node, PartKind::Doc]);
    }

    /// 部品の形は凍結（E34）。ドメイン・切れ目の長さ・キーの名前・子の並びを変えると、DB に置いた文書と根が合わなくなる。
    #[test]
    fn parts_format_is_frozen() {
        let parts = split(&doc_with(130)).unwrap();
        assert_eq!(
            parts.root.to_hex(),
            "9bf3800533bed9c497e211e01dc16634816b58f38c48762213e02d907867c34e",
            "部品の形を変えたら、新しいドメイン（-v2）で別の形にする"
        );
        assert_eq!(
            part_digest(PartKind::Chunk, b"[]").to_hex(),
            "a0309542e759cf481c6605b28f0cae9493e84e02ad9bcd8382856452c83c9e47"
        );
    }

    #[test]
    fn broken_parts_are_refused() {
        let g = doc_with(130);
        let parts = split(&g).unwrap();
        let map = as_map(&parts);
        let node_digest = parts
            .parts
            .iter()
            .find(|p| p.kind == PartKind::Node && p.children.len() == 3)
            .unwrap()
            .digest;
        let chunk_digest = parts
            .parts
            .iter()
            .find(|p| p.kind == PartKind::Chunk)
            .unwrap()
            .digest;
        let invalid =
            |m: &BTreeMap<Digest, (PartKind, Vec<u8>)>, root: &Digest, what: &str| match join(
                root, m,
            ) {
                Err(EngineError::Invalid(issues)) => {
                    assert_eq!(issues.0[0].code, IssueCode::BadGraph, "{what}");
                }
                other => panic!("{what}: {other:?}"),
            };
        // 足りない部品（根・ノード・切れ目）
        for gone in [parts.root, node_digest, chunk_digest] {
            let mut m = map.clone();
            m.remove(&gone);
            invalid(&m, &parts.root, "足りない部品");
        }
        // 違うダイジェスト（本文を変えた）
        let mut m = map.clone();
        m.get_mut(&chunk_digest).unwrap().1 = b"[1,2]".to_vec();
        invalid(&m, &parts.root, "違うダイジェスト");
        // 種類の違い（同じ本文を別の種類で持つ）
        let mut m = map.clone();
        m.get_mut(&chunk_digest).unwrap().0 = PartKind::Node;
        invalid(&m, &parts.root, "種類の違い");
        // 本文を書き換えて、ダイジェストも合わせ直した部品（count の違い・殻が [] でない・正規でない・切っていない長い配列・
        // 切れ目の長さの違い）
        let node_body: Value = serde_json::from_slice(&map[&node_digest].1).unwrap();
        let rewrite =
            |edit: &dyn Fn(&mut Value)| -> (Digest, BTreeMap<Digest, (PartKind, Vec<u8>)>) {
                let mut v = node_body.clone();
                edit(&mut v);
                let body = canonical_json(&v).unwrap();
                node_swap(&parts, &map, node_digest, body)
            };
        type Edit = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Edit)> = vec![
            (
                "count の違い",
                Box::new(|v: &mut Value| v[CHUNKS_KEY]["actions"]["count"] = json!(129)),
            ),
            (
                "count が切らない長さ",
                Box::new(|v: &mut Value| v[CHUNKS_KEY]["actions"]["count"] = json!(64)),
            ),
            (
                "殻が [] でない",
                Box::new(|v: &mut Value| v["params"]["actions"] = json!([1])),
            ),
            (
                "殻がない",
                Box::new(|v: &mut Value| {
                    v["params"].as_object_mut().unwrap().remove("actions");
                }),
            ),
            (
                "余分な項目",
                Box::new(|v: &mut Value| v[CHUNKS_KEY]["actions"]["x"] = json!(1)),
            ),
            (
                "空の $chunks",
                Box::new(|v: &mut Value| v[CHUNKS_KEY] = json!({})),
            ),
            (
                "切れ目の数の違い",
                Box::new(|v: &mut Value| {
                    v[CHUNKS_KEY]["actions"]["parts"]
                        .as_array_mut()
                        .unwrap()
                        .pop();
                }),
            ),
            (
                "切れ目の並びの違い",
                Box::new(|v: &mut Value| {
                    v[CHUNKS_KEY]["actions"]["parts"]
                        .as_array_mut()
                        .unwrap()
                        .swap(1, 2);
                }),
            ),
            (
                "参照が 16 進でない",
                Box::new(|v: &mut Value| v[CHUNKS_KEY]["actions"]["parts"][0] = json!("x")),
            ),
            (
                "切っていない長い配列",
                Box::new(|v: &mut Value| v["params"]["short"] = json!(vec![0; 65])),
            ),
        ];
        for (what, edit) in &cases {
            let (root, m) = rewrite(edit.as_ref());
            invalid(&m, &root, what);
        }
        // 正規でない本文（空白を足して、ダイジェストは合わせる）
        let mut body = map[&node_digest].1.clone();
        body.push(b' ');
        let (root, m) = node_swap(&parts, &map, node_digest, body);
        invalid(&m, &root, "正規でない本文");
        // 未変更ならつながる
        assert_eq!(
            join(&parts.root, &map).unwrap(),
            g.to_canonical_json().unwrap()
        );
    }

    /// ノードの部品を `body` に差し替え、根もそれを指すように作り直した組。
    fn node_swap(
        parts: &DocParts,
        map: &BTreeMap<Digest, (PartKind, Vec<u8>)>,
        node: Digest,
        body: Vec<u8>,
    ) -> (Digest, BTreeMap<Digest, (PartKind, Vec<u8>)>) {
        let mut m = map.clone();
        let new = part_digest(PartKind::Node, &body);
        m.insert(new, (PartKind::Node, body));
        let root_body = String::from_utf8(map[&parts.root].1.clone())
            .unwrap()
            .replace(&node.to_hex(), &new.to_hex())
            .into_bytes();
        let root = part_digest(PartKind::Doc, &root_body);
        m.insert(root, (PartKind::Doc, root_body));
        (root, m)
    }

    /// ChaCha8 で作った文書を分けてつなぐと戻り、部品を壊すと（足りない・本文を 1 バイト変える・種類を変える）落ちずに断る。
    #[test]
    fn random_documents_round_trip_and_random_damage_is_refused() {
        let mut r: ChaCha8Rng = crate::determinism::rng(0xD0C5);
        for case in 0..60 {
            let strokes = (r.next_u32() % 400) as usize;
            let mut g = doc_with(strokes);
            // 引数の形を散らす：2 つ目の長い配列・入れ子・文字列
            if r.next_u32().is_multiple_of(2) {
                let n = (r.next_u32() % 300) as usize;
                g.nodes[0].params["pts"] = json!(
                    (0..n)
                        .map(|i| json!([i, -(i as i64), "あ"]))
                        .collect::<Vec<_>>()
                );
            }
            let parts = split(&g).unwrap();
            let map = as_map(&parts);
            assert_eq!(
                join(&parts.root, &map).unwrap(),
                g.to_canonical_json().unwrap(),
                "case {case}"
            );
            for _ in 0..8 {
                let mut m = map.clone();
                let keys: Vec<Digest> = m.keys().copied().collect();
                let at = keys[(r.next_u32() as usize) % keys.len()];
                match r.next_u32() % 3 {
                    0 => {
                        m.remove(&at);
                    }
                    1 => {
                        let body = &mut m.get_mut(&at).unwrap().1;
                        let i = (r.next_u32() as usize) % body.len();
                        body[i] ^= 1 << (r.next_u32() % 8);
                    }
                    _ => {
                        let entry = m.get_mut(&at).unwrap();
                        entry.0 = match entry.0 {
                            PartKind::Doc => PartKind::Node,
                            PartKind::Node => PartKind::Chunk,
                            PartKind::Chunk => PartKind::Doc,
                        };
                    }
                }
                assert!(
                    matches!(join(&parts.root, &m), Err(EngineError::Invalid(_))),
                    "case {case}"
                );
            }
        }
    }

    /// 同じ切れ目を何度も参照して膨らませる組は、組み立てる前に上限で断る。
    #[test]
    fn amplification_is_limited() {
        let big = Value::Array(
            (0..PART_CHUNK)
                .map(|_| json!("x".repeat(64 * 1024)))
                .collect(),
        );
        let chunk = canonical_json(&big).unwrap();
        let c = part_digest(PartKind::Chunk, &chunk);
        let n = 20_000usize;
        let refs: Vec<Value> = (0..n).map(|_| json!(c.to_hex())).collect();
        let node = json!({
            "id": 1, "op": "paint.actions", "v": 1, "params": { "actions": [] }, "inputs": [],
            "seed": "0", "enabled": true,
            "$chunks": { "actions": { "count": n * PART_CHUNK, "parts": refs } }
        });
        let node_body = canonical_json(&node).unwrap();
        let nd = part_digest(PartKind::Node, &node_body);
        let doc = json!({ "schema": 3, "rules": 1, "inputs": [], "nodes": [nd.to_hex()], "outputs": [], "nextId": 2 });
        let doc_body = canonical_json(&doc).unwrap();
        let root = part_digest(PartKind::Doc, &doc_body);
        let m = BTreeMap::from([
            (c, (PartKind::Chunk, chunk)),
            (nd, (PartKind::Node, node_body)),
            (root, (PartKind::Doc, doc_body)),
        ]);
        assert!(matches!(join(&root, &m), Err(EngineError::Limit(_))));
    }
}
