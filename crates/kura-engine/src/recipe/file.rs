//! `.kurarecipe` の読み書き（docs/editing.md §4.3）。
//!
//! - 書く：封筒 `{ kura: "recipe", schema, name, description, kind, requires, graph, bindings }` を正規の JSON にする
//!   （キーを並べ、空白なし、BOM なし）。`requires` は文書から求め直したもの、`bindings` は並べたもの。
//! - 読む：正規でない JSON（手で整えたもの）も受け、先頭の UTF-8 の BOM は外す。確かめは
//!   0 生の長さ → 1 JSON → 2 `kura` → 3 `schema`（新しすぎる・移行）→ 4 封筒の型と名前・説明 → 5 文書（`doc::load` と同じ）と
//!   正規の形の長さ・深さ → 6・7 登録表と形（`Engine::check_recipe`。指摘を集める）→ 8 `requires` の照らし、の順で、最初に
//!   当たったところで返す。新しい Kura で作ったレシピに「壊れている」ではなく「新しい Kura が要る」と出すため、3 を 4 より前、
//!   6 を 8 より前に置く。どの誤りも panic せず、`Internal` にしない。
//! - 入れ子の深さと大きさは読み書きで同じ数え方（封筒の全体の正規の JSON で `digest::MAX_DEPTH` 段・`MAX_RECIPE_BYTES`）なので、
//!   書けたレシピは必ず読め、読めたレシピは必ず書ける。

use serde::{Deserialize, Serialize};

use super::{
    Binding, MAX_BINDINGS, MAX_BINDINGS_BYTES, MAX_RECIPE_BYTES, RECIPE_SCHEMA, Recipe, RecipeKind,
    Requirement, check_text, sort_bindings,
};
use crate::digest::{Digest, canonical_len_within, canonical_within, hash_canonical};
use crate::doc::{self, Graph, NodeId, SchemaStep};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::limits::Limits;
use crate::op::OpStatus;
use crate::render::Engine;

/// 封筒のスキーマの移行の表：`RECIPE_STEPS[i]` はスキーマ `i + 1` を `i + 2` へ移す。上げたら 1 つ足す（消さない）。
const RECIPE_STEPS: &[SchemaStep] = &[];

/// 読み込んだレシピと、今の版でない操作の印。
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedRecipe {
    pub recipe: Recipe,
    /// 今の版でないノード（旧版・移す版・退役した版）。取り込みの画面は Legacy・Retired に「古い版の操作を含みます」を出す。
    pub notes: Vec<RecipeNote>,
}

/// 今の版でない操作のノードの印（docs/editing.md §8.4）。
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeNote {
    pub node: NodeId,
    pub op: String,
    pub v: u16,
    pub status: OpStatus,
}

/// 書く形（封筒）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FileOut<'a> {
    kura: &'static str,
    schema: u32,
    name: &'a str,
    description: &'a str,
    kind: RecipeKind,
    requires: Vec<Requirement>,
    graph: &'a Graph,
    bindings: Vec<Binding>,
}

/// 読む形（封筒。文書は `doc::load_value` で読むので JSON のまま受ける）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileIn {
    #[allow(dead_code)]
    kura: String,
    #[allow(dead_code)]
    schema: u32,
    name: String,
    #[serde(default)]
    description: String,
    kind: RecipeKind,
    requires: Vec<Requirement>,
    graph: serde_json::Value,
    #[serde(default)]
    bindings: Vec<Binding>,
}

fn bad_graph(message: impl Into<String>) -> EngineError {
    EngineError::Invalid(Issues::one(Issue::error(IssueCode::BadGraph, message)))
}

/// 正規の形で測った上限の誤りに、レシピのことだと分かる文言を付ける。
fn too_large(e: EngineError) -> EngineError {
    match e {
        EngineError::Limit(m) => {
            EngineError::Limit(format!("レシピを保存の形（正規の JSON）にできません：{m}"))
        }
        other => other,
    }
}

impl Recipe {
    /// 書く形（`requires` は求め直し、`bindings` は並べる）。
    fn file_form(&self) -> FileOut<'_> {
        let mut bindings = self.bindings.clone();
        sort_bindings(&mut bindings);
        FileOut {
            kura: "recipe",
            schema: RECIPE_SCHEMA,
            name: &self.name,
            description: &self.description,
            kind: self.kind,
            requires: self.requires(),
            graph: &self.graph,
            bindings,
        }
    }

    /// 書く形の正規の JSON の長さ（`MAX_RECIPE_BYTES` と `digest::MAX_DEPTH` 段を超えれば `Limit`）。
    pub(crate) fn file_len(&self) -> Result<usize, EngineError> {
        canonical_len_within(&self.file_form(), MAX_RECIPE_BYTES).map_err(too_large)
    }

    /// `.kurarecipe` の正規の JSON（キーを並べ、空白なし、BOM なし）。名前・説明・形（[`Recipe::new`] と同じ確かめ）を
    /// 通らなければ `Invalid`、`MAX_RECIPE_BYTES` か `digest::MAX_DEPTH` 段を超えれば `Limit`。登録表での確かめは
    /// しない（書く前に [`Engine::check_recipe`] を通す）。
    pub fn to_file(&self) -> Result<Vec<u8>, EngineError> {
        check_text(&self.name, &self.description)?;
        let issues = self.shape_issues(Limits::default().max_nodes);
        if issues.has_errors() {
            return Err(EngineError::Invalid(issues));
        }
        canonical_within(&self.file_form(), MAX_RECIPE_BYTES).map_err(too_large)
    }

    /// レシピのダイジェスト（書く形の `kura-recipe-v1`）。kura-core が取り込みの重なりを見分けるのに使える。
    pub fn digest(&self) -> Result<Digest, EngineError> {
        hash_canonical("kura-recipe-v1", &self.file_form())
    }
}

impl Engine {
    /// `.kurarecipe` を読んで全部確かめる（モジュールの冒頭の順）。旧版・退役した操作は読めて、印（`notes`）を返す。
    pub fn load_recipe(&self, raw: &[u8]) -> Result<LoadedRecipe, EngineError> {
        // 0：生の長さ
        if raw.len() > MAX_RECIPE_BYTES {
            return Err(EngineError::Limit(format!(
                "レシピが大きすぎます（{} バイト。上限 {MAX_RECIPE_BYTES}）",
                raw.len()
            )));
        }
        // 1：JSON（先頭の BOM は外す）
        let raw = raw.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(raw);
        let value: serde_json::Value = serde_json::from_slice(raw)
            .map_err(|e| bad_graph(format!("JSON が読めません: {e}")))?;
        // 2：Kura のレシピか
        if value.get("kura").and_then(serde_json::Value::as_str) != Some("recipe") {
            return Err(bad_graph("Kura のレシピではありません"));
        }
        // 3：封筒の版（新しすぎれば、壊れているより先に断る）
        let schema = value
            .get("schema")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| bad_graph("レシピの schema がありません"))?;
        if schema > u64::from(RECIPE_SCHEMA) {
            return Err(EngineError::Invalid(Issues::one(Issue::error(
                IssueCode::SchemaTooNew,
                format!("この版の Kura より新しいレシピです（schema {schema}）"),
            ))));
        }
        let value = doc::migrate_with(value, schema as u32, RECIPE_SCHEMA, RECIPE_STEPS)?;
        // 4：封筒の型と名前・説明
        let file: FileIn = serde_json::from_value(value)
            .map_err(|e| bad_graph(format!("レシピの形が正しくありません: {e}")))?;
        check_text(&file.name, &file.description)?;
        if file.bindings.len() > MAX_BINDINGS {
            return Err(bad_graph(format!(
                "結び付けが多すぎます（{} 個。上限 {MAX_BINDINGS}）",
                file.bindings.len()
            )));
        }
        // 5：文書（スキーマの確かめと移行・型・大きさ）と、封筒の全体の正規の形の長さ・深さ（膨らむ数・深い入れ子）。文書は
        // `Recipe::new` と同じく保存の形に往復させて持つ（引数の 16.0・-0 を 16・0 にし、書いて読み直したレシピと等しくする）
        let graph = doc::load(&doc::load_value(file.graph)?.to_canonical_json()?)?;
        let mut bindings = file.bindings;
        sort_bindings(&mut bindings);
        let recipe = Recipe {
            name: file.name,
            description: file.description,
            kind: file.kind,
            graph,
            bindings,
        };
        recipe.file_len()?;
        // 6・7：登録表と形
        let notes = self.check_recipe(&recipe)?;
        // 8：requires は文書から求めたものと集合として等しい（少なく書いて確かめをすり抜けるファイルを断る）
        let mut given = file.requires;
        given.sort();
        given.dedup();
        if given != recipe.requires() {
            return Err(bad_graph("requires が文書と合いません"));
        }
        Ok(LoadedRecipe { recipe, notes })
    }
}

/// 結び付けの正規の JSON（kura-core が DB の列に置く）。`MAX_BINDINGS` 個・`MAX_BINDINGS_BYTES` まで。並べて書く。
pub fn bindings_json(list: &[Binding]) -> Result<Vec<u8>, EngineError> {
    if list.len() > MAX_BINDINGS {
        return Err(EngineError::Limit(format!(
            "結び付けが多すぎます（{} 個。上限 {MAX_BINDINGS}）",
            list.len()
        )));
    }
    let mut sorted = list.to_vec();
    sort_bindings(&mut sorted);
    canonical_within(&sorted, MAX_BINDINGS_BYTES)
}

/// DB の列から結び付けを読む（[`bindings_json`] の逆。`MAX_BINDINGS` 個・`MAX_BINDINGS_BYTES` まで）。
pub fn bindings_from_json(raw: &[u8]) -> Result<Vec<Binding>, EngineError> {
    if raw.len() > MAX_BINDINGS_BYTES {
        return Err(EngineError::Limit(format!(
            "結び付けが大きすぎます（{} バイト。上限 {MAX_BINDINGS_BYTES}）",
            raw.len()
        )));
    }
    let mut list: Vec<Binding> =
        serde_json::from_slice(raw).map_err(|e| bad_graph(format!("結び付けが読めません: {e}")))?;
    if list.len() > MAX_BINDINGS {
        return Err(EngineError::Limit(format!(
            "結び付けが多すぎます（{} 個。上限 {MAX_BINDINGS}）",
            list.len()
        )));
    }
    sort_bindings(&mut list);
    Ok(list)
}
