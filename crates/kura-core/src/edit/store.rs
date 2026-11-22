//! 文書を内容アドレスの部品で DB に置く・読む（`edit_objects`・`edit_edges`。docs/data-model.md §4「保存と版（M12）」、
//! 部品の形は docs/editing.md §4.4 と `kura_engine::doc_parts`）。
//!
//! 同じ中身の部品は 1 行だけ持つ（筆を足した版どうしで前の切れ目とほかのノードを共有する）。読むときは根から 2 段の子を
//! 1 つの再帰の問い合わせで集め、エンジンの `doc_parts::join` で本文からダイジェストを求め直して照らしてつなぐ。部品の行は
//! 消さない（どの版・下書きからも参照されない部品の片付けは段 3 の GC）。

use std::collections::{BTreeMap, HashMap};

use kura_engine::doc_parts::{self, DocParts, PartKind};
use kura_engine::{Digest, Engine, Graph};
use rusqlite::{Connection, OptionalExtension, params};

use super::from_engine;
use crate::{CoreError, Result};

/// 文書の部品を DB に置き、根（doc の部品）の行の ID を返す（呼び手のトランザクションの中で）。子から順に入れ、すでにある部品は
/// そのまま使う（`ON CONFLICT(digest) DO NOTHING`）。新しく入れた親の分だけ辺を足す（同じダイジェストの部品は同じ本文・
/// 同じ子なので、すでにある部品の辺はもうある）。
pub fn put_doc(tx: &Connection, parts: &DocParts, now: i64) -> Result<i64> {
    let mut insert = tx.prepare_cached(
        "INSERT INTO edit_objects (digest, kind, body, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(digest) DO NOTHING",
    )?;
    let mut find = tx.prepare_cached("SELECT id FROM edit_objects WHERE digest = ?1")?;
    let mut edge =
        tx.prepare_cached("INSERT INTO edit_edges (parent_id, ord, child_id) VALUES (?1, ?2, ?3)")?;
    let mut ids: HashMap<Digest, i64> = HashMap::with_capacity(parts.parts.len());
    for part in &parts.parts {
        let added = insert.execute(params![
            &part.digest.0[..],
            part.kind.as_str(),
            &part.body,
            now
        ])?;
        let id: i64 = find.query_row([&part.digest.0[..]], |row| row.get(0))?;
        ids.insert(part.digest, id);
        if added == 0 {
            continue;
        }
        for (ord, child) in part.children.iter().enumerate() {
            let child_id = ids.get(child).copied().ok_or_else(|| {
                CoreError::Invalid(format!(
                    "文書の部品 {} の子 {child} が先に並んでいません",
                    part.digest
                ))
            })?;
            let ord = i64::try_from(ord)
                .map_err(|_| CoreError::Invalid("文書の部品の子が多すぎます".into()))?;
            edge.execute(params![id, ord, child_id])?;
        }
    }
    ids.get(&parts.root)
        .copied()
        .ok_or_else(|| CoreError::Invalid(format!("文書の根 {} が部品にありません", parts.root)))
}

/// 根の行 `doc_id` から部品を集めてつなぎ、文書の正規の JSON のバイト列を返す。部品がない・壊れていれば Invalid
/// （根の行がなければ NotFound）、大きすぎれば TooLarge。
pub fn load_doc_bytes(conn: &Connection, doc_id: i64) -> Result<Vec<u8>> {
    // 根（深さ 0）・ノード（1）・切れ目（2）。UNION で同じ部品は 1 度だけ
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE t(id, depth) AS (
           SELECT ?1, 0
           UNION
           SELECT e.child_id, t.depth + 1 FROM edit_edges e JOIN t ON e.parent_id = t.id WHERE t.depth < 2)
         SELECT o.id, o.digest, o.kind, o.body FROM edit_objects o WHERE o.id IN (SELECT id FROM t)",
    )?;
    let rows = stmt.query_map([doc_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })?;
    let mut parts: BTreeMap<Digest, (PartKind, Vec<u8>)> = BTreeMap::new();
    let mut root = None;
    for row in rows {
        let (id, digest, kind, body) = row?;
        let digest = <[u8; 32]>::try_from(digest.as_slice())
            .map(Digest)
            .map_err(|_| {
                CoreError::Invalid(format!(
                    "文書の部品 {id} のダイジェストが 32 バイトではありません"
                ))
            })?;
        let kind = PartKind::parse(&kind).ok_or_else(|| {
            CoreError::Invalid(format!("文書の部品 {id} の種類 {kind} を知りません"))
        })?;
        if id == doc_id {
            root = Some(digest);
        }
        parts.insert(digest, (kind, body));
    }
    let root =
        root.ok_or_else(|| CoreError::NotFound(format!("文書（部品 {doc_id}）がありません")))?;
    doc_parts::join(&root, &parts).map_err(|e| from_engine("文書の部品をつなげません", e))
}

/// 根の行 `doc_id` の文書を読む（つないだバイト列を `Engine::load` に通す。スキーマの移行と、知らない操作・版の確かめを含む）。
pub fn load_doc(conn: &Connection, engine: &Engine, doc_id: i64) -> Result<Graph> {
    let bytes = load_doc_bytes(conn, doc_id)?;
    engine
        .load(&bytes)
        .map_err(|e| from_engine("文書を読めません", e))
}

/// 根のダイジェストから、DB に置いた文書（doc の部品）の行の ID を引く（なければ None）。
pub fn doc_id_by_digest(conn: &Connection, digest: &Digest) -> Result<Option<i64>> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM edit_objects WHERE digest = ?1 AND kind = 'doc'")?;
    Ok(stmt
        .query_row([&digest.0[..]], |row| row.get(0))
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use kura_engine::doc_parts::split;
    use serde_json::json;

    /// 塗りつぶしの動作を `n` 個持つ描画のノードと、反転のノードの文書（登録表で読める引数）。
    fn doc_with(n: usize) -> Graph {
        let actions: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                json!({ "type": "fill", "at": { "x": i, "y": 3 }, "color": { "rgba": [0, 0, 0, 0] },
                        "tolerance": 30, "connectivity": "eight", "contiguous": true, "frame": 0 })
            })
            .collect();
        let mut g = Graph::new_single();
        g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
            .unwrap();
        g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
            .unwrap();
        g
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn put_and_load_round_trip() {
        let conn = test_conn();
        let engine = Engine::new().unwrap();
        for n in [0, 3, 64, 65, 200] {
            let g = doc_with(n);
            let parts = split(&g).unwrap();
            let id = put_doc(&conn, &parts, 1).unwrap();
            assert_eq!(doc_id_by_digest(&conn, &parts.root).unwrap(), Some(id));
            assert_eq!(
                load_doc_bytes(&conn, id).unwrap(),
                g.to_canonical_json().unwrap()
            );
            assert_eq!(load_doc(&conn, &engine, id).unwrap(), g);
            // 同じ文書を置き直しても行は増えず、同じ根
            let objects = count(&conn, "edit_objects");
            assert_eq!(put_doc(&conn, &parts, 2).unwrap(), id);
            assert_eq!(count(&conn, "edit_objects"), objects);
        }
        assert_eq!(doc_id_by_digest(&conn, &Digest([9; 32])).unwrap(), None);
        assert!(matches!(
            load_doc_bytes(&conn, 999),
            Err(CoreError::NotFound(_))
        ));
        assert!(violations_empty(&conn));
    }

    /// 筆を足した版どうしで部品を共有する：200 本 → 210 本で新しく書くのは、最後の切れ目・描画のノード・根の 3 行と、
    /// 新しいノード（切れ目 4 つ）と根（ノード 2 つ）の辺だけ。
    #[test]
    fn appending_ten_strokes_writes_only_the_last_chunk_the_node_and_the_root() {
        let conn = test_conn();
        put_doc(&conn, &split(&doc_with(200)).unwrap(), 1).unwrap();
        let (objects, edges) = (count(&conn, "edit_objects"), count(&conn, "edit_edges"));
        assert_eq!((objects, edges), (1 + 4 + 1 + 1, 4 + 2));
        let parts = split(&doc_with(210)).unwrap();
        let id = put_doc(&conn, &parts, 2).unwrap();
        assert_eq!(count(&conn, "edit_objects") - objects, 3);
        assert_eq!(count(&conn, "edit_edges") - edges, 4 + 2);
        let kinds: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT kind FROM edit_objects WHERE created_at = 2 ORDER BY id")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(kinds, ["chunk", "node", "doc"]);
        assert_eq!(
            load_doc_bytes(&conn, id).unwrap(),
            doc_with(210).to_canonical_json().unwrap()
        );
    }

    /// 同じ切れ目を 2 回参照するノード（同じ動作を 130 個：切れ目 0 と 1 が同じ中身）も、置いて読み直せる。辺は ord ごとに
    /// 1 行（ord 0・1 が同じ子）、部品は 1 行だけで、置き直しても行は増えない。
    #[test]
    fn a_node_that_refers_to_the_same_chunk_twice_round_trips() {
        let conn = test_conn();
        let engine = Engine::new().unwrap();
        let action = json!({ "type": "fill", "at": { "x": 1, "y": 3 }, "color": { "rgba": [0, 0, 0, 0] },
                             "tolerance": 30, "connectivity": "eight", "contiguous": true, "frame": 0 });
        let mut g = Graph::new_single();
        g.push_linear(
            "paint.actions",
            1,
            json!({ "actions": vec![action; 130] }),
            0,
        )
        .unwrap();
        let parts = split(&g).unwrap();
        let id = put_doc(&conn, &parts, 1).unwrap();
        // (a) 描画のノードの辺は 3 行（切れ目 64・64・2 個）で、ord 0・1 は同じ切れ目の行を指す
        let node: i64 = conn
            .query_row(
                "SELECT child_id FROM edit_edges WHERE parent_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        let edges: Vec<(i64, i64)> = {
            let mut stmt = conn
                .prepare("SELECT ord, child_id FROM edit_edges WHERE parent_id = ?1 ORDER BY ord")
                .unwrap();
            stmt.query_map([node], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(edges.len(), 3, "{edges:?}");
        assert_eq!(
            edges.iter().map(|(ord, _)| *ord).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(edges[0].1, edges[1].1, "切れ目 0 と 1 は同じ部品");
        assert_ne!(edges[1].1, edges[2].1);
        // 部品は根・ノード・切れ目 2 つ（同じ中身の切れ目は 1 行）
        assert_eq!(count(&conn, "edit_objects"), 4);
        // (b) 読み直すと同じ文書
        assert_eq!(
            load_doc_bytes(&conn, id).unwrap(),
            g.to_canonical_json().unwrap()
        );
        assert_eq!(load_doc(&conn, &engine, id).unwrap(), g);
        // (c) 置き直しても行は増えず、同じ根
        let (objects, all_edges) = (count(&conn, "edit_objects"), count(&conn, "edit_edges"));
        assert_eq!(put_doc(&conn, &parts, 2).unwrap(), id);
        assert_eq!(
            (count(&conn, "edit_objects"), count(&conn, "edit_edges")),
            (objects, all_edges)
        );
        // (d) 外部キーの違反がない
        assert!(violations_empty(&conn));
    }

    /// DB の部品が壊れていれば（本文の書き換え・知らない種類・足りない辺）、つながずに Invalid。
    #[test]
    fn damaged_rows_are_refused() {
        let conn = test_conn();
        let id = put_doc(&conn, &split(&doc_with(130)).unwrap(), 1).unwrap();
        conn.execute(
            "UPDATE edit_objects SET body = CAST('[1]' AS BLOB) WHERE id = (SELECT min(id) FROM edit_objects)",
            [],
        )
        .unwrap();
        assert!(matches!(
            load_doc_bytes(&conn, id),
            Err(CoreError::Invalid(_))
        ));

        let conn = test_conn();
        let id = put_doc(&conn, &split(&doc_with(130)).unwrap(), 1).unwrap();
        // 知らない種類は CHECK が断る（DB の外で書き換えられたものは読むときに断る）
        assert!(
            conn.execute("UPDATE edit_objects SET kind = 'blob' WHERE id = ?1", [id])
                .is_err()
        );
        conn.execute(
            "DELETE FROM edit_edges WHERE parent_id = ?1 AND ord = 1",
            [id],
        )
        .unwrap();
        assert!(matches!(
            load_doc_bytes(&conn, id),
            Err(CoreError::Invalid(_))
        ));
    }

    #[test]
    fn queries_use_their_indexes() {
        let conn = test_conn();
        let plan = query_plan(
            &conn,
            "SELECT id FROM edit_objects WHERE digest = x'00' AND kind = 'doc'",
        );
        assert!(plan.contains("sqlite_autoindex_edit_objects_1"), "{plan}");
        let plan = query_plan(
            &conn,
            "WITH RECURSIVE t(id, depth) AS (
               SELECT 1, 0
               UNION
               SELECT e.child_id, t.depth + 1 FROM edit_edges e JOIN t ON e.parent_id = t.id WHERE t.depth < 2)
             SELECT o.id, o.digest, o.kind, o.body FROM edit_objects o WHERE o.id IN (SELECT id FROM t)",
        );
        // 辺は主キー（parent_id, ord）で、部品は行の ID で引く。部品・辺の表は走査しない
        assert!(
            plan.contains("SEARCH e USING PRIMARY KEY (parent_id=?)"),
            "{plan}"
        );
        assert!(
            plan.contains("SEARCH o USING INTEGER PRIMARY KEY"),
            "{plan}"
        );
        assert!(
            !plan.contains("SCAN e") && !plan.contains("SCAN o"),
            "{plan}"
        );
    }

    fn violations_empty(conn: &Connection) -> bool {
        crate::db::violations(conn).unwrap().is_empty()
    }
}
