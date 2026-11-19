//! レシピの表（M14 段 3。`edit_recipes`。docs/data-model.md §4「一括加工（M14）」、docs/editing.md §4.3、ADR-190・192）。
//!
//! 文書は内容アドレスの部品（`store::put_doc`）、結び付けは `recipe::bindings_json` の正規の JSON で持つ。名前は重なってよく
//! （ADR-192 (2)）、並びは `pos`（プレイリストと同じ）。消すときはすぐ消す（ADR-192 (3)）。消す・中身を差し替えると古い文書の
//! 部品が孤立しうるので、GC の部品の印を置く（部品の片付けは `edit_recipes.doc_id` を根に数える）。レシピは小さい表なので、
//! 一覧は表を走査する（プレイリストと同じ）。名前・説明は前後の空白を除いて NFC にし、エンジンの決まり（`recipe::check_text`）で
//! 確かめる。

use kura_engine::doc::{Graph, NodeId};
use kura_engine::recipe::{
    Binding, MAX_NAME_BYTES, Recipe, RecipeKind, RecipeNote, bindings_from_json, bindings_json,
    check_text,
};
use rusqlite::{Connection, OptionalExtension, params};

use super::{EngineRuntime, from_engine, gc, store};
use crate::db::Db;
use crate::text::nfc;
use crate::time::now_ms;
use crate::{CoreError, Result};

/// 一覧の 1 行（サイドバーの節・レシピの画面の見出し）。
#[derive(Debug, Clone, PartialEq)]
pub struct RecipeSummary {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub kind: RecipeKind,
    /// 操作（ノード）の数。
    pub nodes: i64,
    /// 結び付けの数。
    pub bindings: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 1 件のレシピ（文書と結び付けを読んだもの）。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecipe {
    pub id: i64,
    pub recipe: Recipe,
    /// 今の版でない操作のノードの印（旧版・移す版・退役した版。docs/editing.md §8.4）。
    pub notes: Vec<RecipeNote>,
    /// 今の Kura では当てられない理由（知らない操作・新しすぎる版など。あれば当てる画面は断る。書き出しと消すことはできる）。
    pub problem: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn kind_str(kind: RecipeKind) -> &'static str {
    match kind {
        RecipeKind::Each => "each",
        RecipeKind::Merge => "merge",
    }
}

fn kind_of(s: &str) -> Result<RecipeKind> {
    match s {
        "each" => Ok(RecipeKind::Each),
        "merge" => Ok(RecipeKind::Merge),
        other => Err(CoreError::Invalid(format!(
            "レシピの種類 {other} を知りません"
        ))),
    }
}

/// 名前・説明を整えて確かめる（前後の空白を除き NFC にする。名前 1〜256 バイト・説明 4,096 バイトまで、制御文字なし）。
pub fn normalize(name: &str, description: &str) -> Result<(String, String)> {
    let (name, description) = (nfc(name.trim()), nfc(description.trim()));
    check_text(&name, &description)
        .map_err(|e| from_engine("レシピの名前・説明が正しくありません", e))?;
    Ok((name, description))
}

/// 複製の名前「{名前} のコピー」（256 バイトに収まるよう、元の名前を文字の切れ目で詰める）。
pub fn copy_name(name: &str) -> String {
    const SUFFIX: &str = " のコピー";
    let room = MAX_NAME_BYTES.saturating_sub(SUFFIX.len());
    let mut end = name.len().min(room);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{SUFFIX}", name[..end].trim_end())
}

/// レシピの一覧（並びの順）。操作の数は文書の根の子の数、結び付けの数は JSON の配列の長さ（どちらも行ごとに索引で引く）。
pub fn list(conn: &Connection) -> Result<Vec<RecipeSummary>> {
    let mut stmt = conn.prepare_cached(
        "SELECT r.id, r.name, r.description, r.kind, json_array_length(r.bindings), r.created_at, r.updated_at,
                (SELECT count(*) FROM edit_edges e WHERE e.parent_id = r.doc_id)
           FROM edit_recipes r ORDER BY r.pos, r.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, description, kind, bindings, created_at, updated_at, nodes) = row?;
        out.push(RecipeSummary {
            id,
            name,
            description,
            kind: kind_of(&kind)?,
            nodes,
            bindings,
            created_at,
            updated_at,
        });
    }
    Ok(out)
}

/// 行（名前・説明・種類・文書・結び付け・日時）。
struct Row {
    name: String,
    description: String,
    kind: RecipeKind,
    doc_id: i64,
    bindings: String,
    created_at: i64,
    updated_at: i64,
}

fn row(conn: &Connection, id: i64) -> Result<Row> {
    let found = conn
        .prepare_cached(
            "SELECT name, description, kind, doc_id, bindings, created_at, updated_at FROM edit_recipes WHERE id = ?1",
        )?
        .query_row([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })
        .optional()?;
    let (name, description, kind, doc_id, bindings, created_at, updated_at) =
        found.ok_or_else(|| CoreError::NotFound(format!("レシピ {id}")))?;
    Ok(Row {
        name,
        description,
        kind: kind_of(&kind)?,
        doc_id,
        bindings,
        created_at,
        updated_at,
    })
}

/// レシピの文書の根のダイジェスト（一括のジョブの入力に写す）。
pub fn doc_root(conn: &Connection, id: i64) -> Result<kura_engine::Digest> {
    let digest: Vec<u8> = conn
        .prepare_cached(
            "SELECT o.digest FROM edit_recipes r JOIN edit_objects o ON o.id = r.doc_id WHERE r.id = ?1",
        )?
        .query_row([id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| CoreError::NotFound(format!("レシピ {id}")))?;
    let digest: [u8; 32] = digest.try_into().map_err(|_| {
        CoreError::Invalid(format!("レシピ {id} の文書のダイジェストが壊れています"))
    })?;
    Ok(kura_engine::Digest(digest))
}

/// 1 件を読む（文書は今のスキーマへ移して読み、当てる前の確かめ `check_recipe` の印か理由を付ける）。文書は登録表を見ずに
/// 読むので、この Kura の知らない操作・新しすぎる版を含むレシピ（新しい Kura で作って古い Kura で開いた）も読めて、理由
/// （`problem`）が付く（書き出しと消すことはできる）。
pub fn get(db: &Db, runtime: &EngineRuntime, id: i64) -> Result<StoredRecipe> {
    let (row, bytes) = db.read(|c| {
        let row = row(c, id)?;
        let bytes = store::load_doc_bytes(c, row.doc_id)?;
        Ok((row, bytes))
    })?;
    let graph =
        kura_engine::doc::load(&bytes).map_err(|e| from_engine("レシピの文書を読めません", e))?;
    let bindings = bindings_from_json(row.bindings.as_bytes())
        .map_err(|e| from_engine("レシピの結び付けを読めません", e))?;
    let recipe = Recipe {
        name: row.name,
        description: row.description,
        kind: row.kind,
        graph,
        bindings,
    };
    let (notes, problem) = match runtime.preview(|e| e.check_recipe(&recipe)) {
        Ok(notes) => (notes, None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    Ok(StoredRecipe {
        id,
        recipe,
        notes,
        problem,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// 結び付けの正規の JSON（DB の列）。
fn bindings_text(bindings: &[Binding]) -> Result<String> {
    let bytes =
        bindings_json(bindings).map_err(|e| from_engine("レシピの結び付けを書けません", e))?;
    String::from_utf8(bytes)
        .map_err(|e| CoreError::Invalid(format!("結び付けが UTF-8 ではありません: {e}")))
}

/// 文書を部品で置き、根の行の ID を返す。
fn put_graph(tx: &Connection, graph: &Graph, now: i64) -> Result<i64> {
    let parts = kura_engine::doc_parts::split(graph)
        .map_err(|e| from_engine("レシピの文書を部品に分けられません", e))?;
    store::put_doc(tx, &parts, now)
}

/// 確かめたレシピを末尾に足し、ID を返す（呼び手のトランザクションの中で）。名前・説明は確かめ済みのもの。
pub fn insert(tx: &Connection, recipe: &Recipe, now: i64) -> Result<i64> {
    let doc_id = put_graph(tx, &recipe.graph, now)?;
    let bindings = bindings_text(&recipe.bindings)?;
    tx.prepare_cached(
        "INSERT INTO edit_recipes (name, description, kind, doc_id, bindings, pos, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, (SELECT coalesce(max(pos), 0) + 1 FROM edit_recipes), ?6, ?6)",
    )?
    .execute(params![
        recipe.name,
        recipe.description,
        kind_str(recipe.kind),
        doc_id,
        bindings,
        now
    ])?;
    Ok(tx.last_insert_rowid())
}

/// エディタの文書から選んだもの（レシピとして保存…・見本で編集の保存）。
#[derive(Debug, Clone)]
pub struct EditorPick<'a> {
    /// エディタの頭の文書。
    pub graph: &'a Graph,
    /// 含めるノード（`graph` のノードの ID）。外したノードは、その入力 0 に付け替える。
    pub keep: &'a [NodeId],
    /// 出力の形式を含めるか（偽なら「元と同じ」）。
    pub with_format: bool,
    /// 結び付け（`graph` のノードで指す。レシピのノードの ID に付け直す）。
    pub bindings: Vec<Binding>,
}

/// エディタの文書から作るレシピ（`Recipe::from_doc`）。当てる前と同じ確かめ（`check_recipe`）を通す。
fn from_editor(
    runtime: &EngineRuntime,
    name: String,
    description: String,
    pick: EditorPick<'_>,
) -> Result<Recipe> {
    let keep = pick.keep;
    let recipe = Recipe::from_doc(
        name,
        description,
        pick.graph,
        &|n| keep.contains(&n.id),
        pick.with_format,
        pick.bindings,
    )
    .map_err(|e| from_engine("レシピにできません", e))?;
    runtime
        .preview(|e| e.check_recipe(&recipe))
        .map_err(|e| from_engine("レシピにできません", e))?;
    Ok(recipe)
}

/// エディタの文書からレシピを作り、ID を返す（段 7 の「レシピとして保存…」）。
pub fn create_from_editor(
    db: &Db,
    runtime: &EngineRuntime,
    name: &str,
    description: &str,
    pick: EditorPick<'_>,
) -> Result<i64> {
    let (name, description) = normalize(name, description)?;
    let recipe = from_editor(runtime, name, description, pick)?;
    db.write(move |tx| insert(tx, &recipe, now_ms()))
}

/// レシピ `id` の中身（文書と結び付け）を、エディタの文書から作り直して差し替える（ID・名前・説明・並びはそのまま。段 7 の
/// 見本で編集）。動いている一括はジョブの入力の写しを使うので変わらない。古い文書の部品が孤立しうるので、GC の部品の印を置く。
pub fn replace_from_editor(
    db: &Db,
    runtime: &EngineRuntime,
    id: i64,
    pick: EditorPick<'_>,
) -> Result<()> {
    let row = db.read(|c| row(c, id))?;
    let recipe = from_editor(runtime, row.name, row.description, pick)?;
    db.write(move |tx| {
        let now = now_ms();
        let doc_id = put_graph(tx, &recipe.graph, now)?;
        let bindings = bindings_text(&recipe.bindings)?;
        let n = tx
            .prepare_cached(
                "UPDATE edit_recipes SET kind = ?2, doc_id = ?3, bindings = ?4, updated_at = ?5 WHERE id = ?1",
            )?
            .execute(params![id, kind_str(recipe.kind), doc_id, bindings, now])?;
        if n != 1 {
            return Err(CoreError::NotFound(format!("レシピ {id}")));
        }
        gc::mark_parts(tx)
    })
}

/// 名前を変える。
pub fn rename(db: &Db, id: i64, name: &str) -> Result<()> {
    let name = name.to_string();
    db.write(move |tx| {
        let row = row(tx, id)?;
        let (name, _) = normalize(&name, &row.description)?;
        tx.prepare_cached("UPDATE edit_recipes SET name = ?2, updated_at = ?3 WHERE id = ?1")?
            .execute(params![id, name, now_ms()])?;
        Ok(())
    })
}

/// 説明を変える（空でよい）。
pub fn describe(db: &Db, id: i64, description: &str) -> Result<()> {
    let description = description.to_string();
    db.write(move |tx| {
        let row = row(tx, id)?;
        let (_, description) = normalize(&row.name, &description)?;
        tx.prepare_cached(
            "UPDATE edit_recipes SET description = ?2, updated_at = ?3 WHERE id = ?1",
        )?
        .execute(params![id, description, now_ms()])?;
        Ok(())
    })
}

/// 並びを変える（`ids` の順にする。含まれないものは後ろに今の順で。プレイリストと同じ）。
pub fn reorder(tx: &Connection, ids: &[i64]) -> Result<()> {
    let mut order: Vec<i64> = Vec::with_capacity(ids.len());
    for id in ids {
        if !order.contains(id) {
            order.push(*id);
        }
    }
    for recipe in list(tx)? {
        if !order.contains(&recipe.id) {
            order.push(recipe.id);
        }
    }
    let mut stmt = tx.prepare_cached("UPDATE edit_recipes SET pos = ?2 WHERE id = ?1")?;
    for (i, id) in order.iter().enumerate() {
        stmt.execute(params![id, (i + 1) as f64])?;
    }
    Ok(())
}

/// 元 `id` のすぐ後ろの並びの値（元と次の中間。間が詰まっていれば、先に 1, 2, … に振り直す）。
fn pos_after(tx: &Connection, id: i64) -> Result<f64> {
    let around = |tx: &Connection| -> Result<(f64, Option<f64>)> {
        let pos: f64 = tx
            .prepare_cached("SELECT pos FROM edit_recipes WHERE id = ?1")?
            .query_row([id], |r| r.get(0))?;
        let next: Option<f64> = tx
            .prepare_cached("SELECT min(pos) FROM edit_recipes WHERE pos > ?1")?
            .query_row([pos], |r| r.get(0))?;
        Ok((pos, next))
    };
    let mid = |(pos, next): (f64, Option<f64>)| next.map_or(pos + 1.0, |n| (pos + n) / 2.0);
    let fits =
        |(pos, next): (f64, Option<f64>), new: f64| new > pos && next.is_none_or(|n| new < n);
    let found = around(tx)?;
    if fits(found, mid(found)) {
        return Ok(mid(found));
    }
    // 同じ間に何度も入れて中間が取れなくなった：今の順のまま振り直す
    reorder(tx, &[])?;
    let found = around(tx)?;
    Ok(mid(found))
}

/// 複製し、新しい ID を返す（名前は「{名前} のコピー」、元のすぐ後ろに並べる。文書の部品は共有する）。
pub fn duplicate(tx: &Connection, id: i64, now: i64) -> Result<i64> {
    let row = row(tx, id)?;
    let new_pos = pos_after(tx, id)?;
    tx.prepare_cached(
        "INSERT INTO edit_recipes (name, description, kind, doc_id, bindings, pos, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
    )?
    .execute(params![
        copy_name(&row.name),
        row.description,
        kind_str(row.kind),
        row.doc_id,
        row.bindings,
        new_pos,
        now
    ])?;
    Ok(tx.last_insert_rowid())
}

/// すぐ消す（ADR-192 (3)）。文書の部品が孤立しうるので、GC の部品の印を置く。
pub fn delete(tx: &Connection, id: i64) -> Result<()> {
    let n = tx
        .prepare_cached("DELETE FROM edit_recipes WHERE id = ?1")?
        .execute([id])?;
    if n != 1 {
        return Err(CoreError::NotFound(format!("レシピ {id}")));
    }
    gc::mark_parts(tx)
}

/// `.kurarecipe` のバイト列（正規の JSON）。
pub fn export(db: &Db, runtime: &EngineRuntime, id: i64) -> Result<Vec<u8>> {
    let stored = get(db, runtime, id)?;
    stored
        .recipe
        .to_file()
        .map_err(|e| from_engine("レシピを書き出せません", e))
}

/// `.kurarecipe` のバイト列 `raw` を読んで足し、(ID, 旧版の印) を返す。知らない操作・新しすぎる版・壊れたファイルは断る
/// （`Engine::load_recipe`。読めたものだけを末尾に足す）。名前・説明も足す前に整える。
pub fn import(db: &Db, runtime: &EngineRuntime, raw: &[u8]) -> Result<(i64, Vec<RecipeNote>)> {
    let loaded = runtime
        .preview(|e| e.load_recipe(raw))
        .map_err(|e| from_engine("レシピを読み込めません", e))?;
    let (name, description) = normalize(&loaded.recipe.name, &loaded.recipe.description)?;
    let recipe = Recipe {
        name,
        description,
        ..loaded.recipe
    };
    let id = db.write(move |tx| insert(tx, &recipe, now_ms()))?;
    Ok((id, loaded.notes))
}

#[cfg(test)]
mod tests {
    use kura_engine::recipe::{SettingKey, Source};
    use serde_json::json;

    use super::*;
    use crate::edit::gc::{Scope, parts_marked, sweep_parts};
    use crate::edit::save_tests::{Env, doc, flip};

    fn runtime(env: &Env) -> std::sync::Arc<EngineRuntime> {
        env.engine.runtime().unwrap().clone()
    }

    /// 反転 2 つと格子の設定の文書（エディタの頭の形）。
    fn editor_doc() -> Graph {
        let mut g = doc(&[flip("vertical"), flip("horizontal")], None);
        g.push_linear("meta.set_grid", 1, json!({ "grid": null }), 0)
            .unwrap();
        g
    }

    fn create(env: &Env, name: &str, keep: &[u32]) -> i64 {
        let g = editor_doc();
        let keep: Vec<NodeId> = keep.iter().map(|n| NodeId(*n)).collect();
        create_from_editor(
            env.lib.db(),
            &runtime(env),
            name,
            "",
            EditorPick {
                graph: &g,
                keep: &keep,
                with_format: false,
                bindings: Vec::new(),
            },
        )
        .unwrap()
    }

    fn names(env: &Env) -> Vec<String> {
        env.lib
            .db()
            .read(list)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect()
    }

    /// エディタの文書から作ると、含めたノードだけのレシピになり（ID は 1, 2, … に付け直し、結び付けも付け直す）、一覧と 1 件で
    /// 同じものを読める。名前・説明は整えて持つ。
    #[test]
    fn a_recipe_made_from_the_editor_round_trips() {
        let env = Env::new();
        let g = editor_doc();
        let binding = Binding {
            node: NodeId(3),
            param: "grid".into(),
            part: None,
            from: Source::Setting {
                key: SettingKey::Grid,
            },
        };
        let keep = [NodeId(1), NodeId(3)];
        let id = create_from_editor(
            env.lib.db(),
            &runtime(&env),
            "  反転して格子  ",
            "説明\n2 行目 ",
            EditorPick {
                graph: &g,
                keep: &keep,
                with_format: false,
                bindings: vec![binding.clone()],
            },
        )
        .unwrap();
        let rows = env.lib.db().read(list).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            (
                r.id,
                r.name.as_str(),
                r.description.as_str(),
                r.kind,
                r.nodes,
                r.bindings
            ),
            (id, "反転して格子", "説明\n2 行目", RecipeKind::Each, 2, 1)
        );
        let stored = get(env.lib.db(), &runtime(&env), id).unwrap();
        assert_eq!(stored.problem, None);
        assert!(stored.notes.is_empty());
        let ops: Vec<(&str, u32)> = stored
            .recipe
            .graph
            .nodes
            .iter()
            .map(|n| (n.op.as_str(), n.id.0))
            .collect();
        assert_eq!(ops, [("image.flip", 1), ("meta.set_grid", 2)]);
        assert_eq!(
            stored.recipe.bindings,
            vec![Binding {
                node: NodeId(2),
                ..binding
            }]
        );
        // 一覧の問い合わせは、部品の辺を主キーで引く（レシピの表は小さいので走査してよい）
        let plan = env
            .lib
            .db()
            .read(|c| {
                Ok(crate::db::query_plan(
                    c,
                    "SELECT r.id, (SELECT count(*) FROM edit_edges e WHERE e.parent_id = r.doc_id)
                       FROM edit_recipes r ORDER BY r.pos, r.id",
                ))
            })
            .unwrap();
        assert!(!plan.contains("SCAN e"), "{plan}");
    }

    /// 名前と説明は整えて確かめる：前後の空白を除き NFC にする。空の名前・制御文字・長すぎるものは Invalid。名前は重なってよい。
    #[test]
    fn names_are_normalized_and_checked() {
        let env = Env::new();
        let id = create(&env, "レシピ", &[1]);
        rename(env.lib.db(), id, " \u{304b}\u{3099}く ").unwrap();
        describe(env.lib.db(), id, "  メモ  ").unwrap();
        let r = &env.lib.db().read(list).unwrap()[0];
        assert_eq!((r.name.as_str(), r.description.as_str()), ("がく", "メモ"));
        for bad in ["", "   ", "a\u{7}b", &"あ".repeat(86)] {
            let e = rename(env.lib.db(), id, bad).unwrap_err();
            assert!(matches!(e, CoreError::Invalid(_)), "{bad:?}: {e:?}");
        }
        let e = describe(env.lib.db(), id, &"x".repeat(4097)).unwrap_err();
        assert!(matches!(e, CoreError::Invalid(_)), "{e:?}");
        create(&env, "がく", &[1]);
        assert_eq!(names(&env), ["がく", "がく"]);
        assert!(matches!(
            rename(env.lib.db(), 999, "x").unwrap_err(),
            CoreError::NotFound(_)
        ));
    }

    /// 複製は「{名前} のコピー」を元のすぐ後ろに並べ（長い名前は文字の切れ目で詰める）、並べ替えは渡した順に、残りは今の順で後ろ。
    #[test]
    fn duplicates_and_reorders() {
        let env = Env::new();
        let a = create(&env, "A", &[1]);
        let b = create(&env, "B", &[2]);
        let c = create(&env, "C", &[1, 2]);
        let a2 = env.lib.db().write(move |tx| duplicate(tx, a, 5)).unwrap();
        assert_eq!(names(&env), ["A", "A のコピー", "B", "C"]);
        let c2 = env.lib.db().write(move |tx| duplicate(tx, c, 5)).unwrap();
        assert_eq!(names(&env), ["A", "A のコピー", "B", "C", "C のコピー"]);
        env.lib
            .db()
            .write(move |tx| reorder(tx, &[c2, b, c2]))
            .unwrap();
        assert_eq!(names(&env), ["C のコピー", "B", "A", "A のコピー", "C"]);
        let _ = a2;
        let long = "あ".repeat(85);
        let copied = copy_name(&long);
        assert!(copied.len() <= MAX_NAME_BYTES, "{}", copied.len());
        assert!(copied.ends_with(" のコピー"));
        assert!(copied.starts_with("あ"));
    }

    /// 消すと行が消え、GC の部品の印が付き、部品の片付けがレシピだけの部品を消す（複製と共有する部品は、複製を消すまで残る）。
    #[test]
    fn deleting_marks_parts_and_sweeps_them() {
        let env = Env::new();
        let a = create(&env, "A", &[1, 3]);
        let copy = env.lib.db().write(move |tx| duplicate(tx, a, 5)).unwrap();
        let objects = |env: &Env| env.count("SELECT count(*) FROM edit_objects");
        let before = objects(&env);
        assert!(before > 0);
        env.lib.db().write(move |tx| delete(tx, a)).unwrap();
        assert!(env.lib.db().read(parts_marked).unwrap());
        let removed = env
            .lib
            .db()
            .write(|tx| sweep_parts(tx, Scope::All, &[]))
            .unwrap();
        assert_eq!(removed, 0, "複製が同じ文書を指している");
        env.lib.db().write(move |tx| delete(tx, copy)).unwrap();
        env.lib
            .db()
            .write(|tx| sweep_parts(tx, Scope::All, &[]))
            .unwrap();
        assert_eq!(objects(&env), 0);
        assert!(matches!(
            env.lib.db().write(move |tx| delete(tx, a)).unwrap_err(),
            CoreError::NotFound(_)
        ));
    }

    /// 書き出したものを読み込むと同じレシピになる（名前は重なってよい）。壊れたファイル・知らない操作は断り、何も足さない。
    #[test]
    fn export_and_import_round_trip() {
        let env = Env::new();
        let rt = runtime(&env);
        let a = create(&env, "書き出す", &[1, 2, 3]);
        let bytes = export(env.lib.db(), &rt, a).unwrap();
        let (b, notes) = import(env.lib.db(), &rt, &bytes).unwrap();
        assert!(notes.is_empty());
        let (ra, rb) = (
            get(env.lib.db(), &rt, a).unwrap(),
            get(env.lib.db(), &rt, b).unwrap(),
        );
        assert_eq!(ra.recipe, rb.recipe);
        assert_eq!(names(&env), ["書き出す", "書き出す"]);
        for broken in [
            b"not json".to_vec(),
            String::from_utf8(bytes.clone())
                .unwrap()
                .replace("image.flip", "image.nothing")
                .into_bytes(),
        ] {
            assert!(import(env.lib.db(), &rt, &broken).is_err());
        }
        assert_eq!(env.lib.db().read(list).unwrap().len(), 2);
    }

    /// 中身を差し替えると、ID・名前・説明・並びはそのままで文書と結び付けが変わり、古い文書の部品に GC の印が付く。
    #[test]
    fn replacing_keeps_the_identity() {
        let env = Env::new();
        let rt = runtime(&env);
        let a = create(&env, "A", &[1]);
        create(&env, "B", &[1]);
        describe(env.lib.db(), a, "説明").unwrap();
        env.lib
            .db()
            .write(|tx| crate::settings::set(tx, crate::edit::gc::PARTS_KEY, &false))
            .unwrap();
        let g = editor_doc();
        let keep = [NodeId(2), NodeId(3)];
        let grid = Binding {
            node: NodeId(3),
            param: "grid".into(),
            part: None,
            from: Source::Setting {
                key: SettingKey::Grid,
            },
        };
        replace_from_editor(
            env.lib.db(),
            &rt,
            a,
            EditorPick {
                graph: &g,
                keep: &keep,
                with_format: false,
                bindings: vec![grid.clone()],
            },
        )
        .unwrap();
        let stored = get(env.lib.db(), &rt, a).unwrap();
        assert_eq!(
            (
                stored.recipe.name.as_str(),
                stored.recipe.description.as_str()
            ),
            ("A", "説明")
        );
        let ops: Vec<&str> = stored
            .recipe
            .graph
            .nodes
            .iter()
            .map(|n| n.op.as_str())
            .collect();
        assert_eq!(ops, ["image.flip", "meta.set_grid"]);
        // 結び付けもレシピのノード（付け直した 2）で差し替わる
        assert_eq!(
            stored.recipe.bindings,
            vec![Binding {
                node: NodeId(2),
                ..grid
            }]
        );
        assert_eq!(names(&env), ["A", "B"]);
        assert!(env.lib.db().read(parts_marked).unwrap());
        assert!(matches!(
            replace_from_editor(
                env.lib.db(),
                &rt,
                999,
                EditorPick {
                    graph: &g,
                    keep: &keep,
                    with_format: false,
                    bindings: Vec::new(),
                },
            )
            .unwrap_err(),
            CoreError::NotFound(_)
        ));
    }

    /// この Kura の知らない操作を含むレシピ（新しい Kura で作って古い Kura で開いた形）も、1 件は読めて理由が付き、書き出せる。
    #[test]
    fn a_recipe_with_an_unknown_op_reads_with_a_problem() {
        let env = Env::new();
        let rt = runtime(&env);
        let mut g = Graph::new_single();
        g.push_linear("image.future", 1, json!({}), 0).unwrap();
        let recipe = Recipe {
            name: "新しい".into(),
            description: String::new(),
            kind: RecipeKind::Each,
            graph: g,
            bindings: Vec::new(),
        };
        let id = env
            .lib
            .db()
            .write(move |tx| insert(tx, &recipe, 1))
            .unwrap();
        let stored = get(env.lib.db(), &rt, id).unwrap();
        assert!(stored.problem.is_some(), "{stored:?}");
        assert_eq!(stored.recipe.graph.nodes[0].op, "image.future");
        let bytes = export(env.lib.db(), &rt, id).unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("image.future"));
        // この Kura では読み込めない（知らない操作）
        assert!(import(env.lib.db(), &rt, &export(env.lib.db(), &rt, id).unwrap()).is_err());
    }

    /// 同じ元を何度複製しても、新しいコピーはいつも元のすぐ後ろに並ぶ（中間が取れなくなったら振り直す）。
    #[test]
    fn many_copies_stay_right_after_the_original() {
        let env = Env::new();
        let a = create(&env, "A", &[1]);
        create(&env, "B", &[1]);
        for k in 0..60 {
            let copy = env.lib.db().write(move |tx| duplicate(tx, a, 5)).unwrap();
            let rows = env.lib.db().read(list).unwrap();
            assert_eq!(rows[0].id, a, "{k}");
            assert_eq!(rows[1].id, copy, "{k}");
            assert_eq!(rows.last().unwrap().name, "B", "{k}");
        }
    }
}
