//! 仮想フォルダ（docs/data-model.md「仮想フォルダ」）：木の読み取り・件数・作成・改名・色・移動・削除・アイテムの所属。

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::text::nfc;
use crate::{CoreError, Result};

/// フォルダとタグに付けられる色（トークン名。`src/styles/tokens.css` の `--folder-*`）。
pub const COLORS: [&str; 4] = ["blue", "violet", "amber", "teal"];

/// 並び順の間隔がこれより狭くなったら振り直す。
const MIN_GAP: f64 = 1e-9;

/// 色の名前を確かめる（なしは None）。
pub fn check_color(color: Option<&str>) -> Result<Option<String>> {
    match color {
        None => Ok(None),
        Some(c) if COLORS.contains(&c) => Ok(Some(c.to_string())),
        Some(c) => Err(CoreError::Invalid(format!("使えない色です: {c}"))),
    }
}

/// 仮想フォルダ 1 つ。
#[derive(Debug, Clone, PartialEq)]
pub struct FolderNode {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub name: String,
    pub color: Option<String>,
    pub pos: f64,
    /// このフォルダを参照する folder ユニット（いちばん古いもの）。
    pub unit_id: Option<i64>,
}

/// すべての仮想フォルダ（親 → 並び順）。木はフロントで組み立てる。
pub fn tree(conn: &Connection) -> Result<Vec<FolderNode>> {
    let mut units: HashMap<i64, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT folder_id, min(unit_id) FROM unit_folders GROUP BY folder_id",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        for row in rows {
            let (folder, unit) = row?;
            units.insert(folder, unit);
        }
    }
    let mut stmt = conn.prepare_cached(
        "SELECT id, parent_id, name, color, pos FROM folders ORDER BY ifnull(parent_id, 0), pos, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FolderNode {
            id: row.get(0)?,
            parent_id: row.get(1)?,
            name: row.get(2)?,
            color: row.get(3)?,
            pos: row.get(4)?,
            unit_id: None,
        })
    })?;
    let mut nodes = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    for node in &mut nodes {
        node.unit_id = units.get(&node.id).copied();
    }
    Ok(nodes)
}

/// いちばん上からのパス（名前の列）。見つからなければ空。
pub fn path_of(nodes: &[FolderNode], id: i64) -> Vec<String> {
    let by_id: HashMap<i64, &FolderNode> = nodes.iter().map(|n| (n.id, n)).collect();
    let mut path = Vec::new();
    let mut current = by_id.get(&id).copied();
    while let Some(node) = current {
        path.push(node.name.clone());
        if path.len() > nodes.len() {
            break;
        }
        current = node.parent_id.and_then(|p| by_id.get(&p).copied());
    }
    path.reverse();
    path
}

/// フォルダごとの件数（そのフォルダと子孫フォルダにある、ゴミ箱以外のアイテムの数。重複は 1 回）。
pub fn item_counts(conn: &Connection) -> Result<HashMap<i64, i64>> {
    let parents = parent_map(conn)?;
    let mut counts: HashMap<i64, i64> = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT m.item_id, m.folder_id FROM item_folders m JOIN items i ON i.id = m.item_id
         WHERE i.trashed_at IS NULL ORDER BY m.item_id",
    )?;
    let mut rows = stmt.query([])?;
    let mut current: Option<i64> = None;
    let mut folders: Vec<i64> = Vec::new();
    let flush = |folders: &mut Vec<i64>, counts: &mut HashMap<i64, i64>| {
        folders.sort_unstable();
        folders.dedup();
        for folder in folders.drain(..) {
            *counts.entry(folder).or_insert(0) += 1;
        }
    };
    while let Some(row) = rows.next()? {
        let item: i64 = row.get(0)?;
        let folder: i64 = row.get(1)?;
        if current != Some(item) {
            flush(&mut folders, &mut counts);
            current = Some(item);
        }
        // そのフォルダと祖先すべてに数える（壊れた循環に備えて段数を制限する）
        let mut f = Some(folder);
        let mut depth = 0;
        while let Some(id) = f {
            folders.push(id);
            depth += 1;
            if depth > parents.len() {
                break;
            }
            f = parents.get(&id).copied().flatten();
        }
    }
    flush(&mut folders, &mut counts);
    Ok(counts)
}

fn parent_map(conn: &Connection) -> Result<HashMap<i64, Option<i64>>> {
    let mut stmt = conn.prepare_cached("SELECT id, parent_id FROM folders")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// そのフォルダと子孫フォルダの ID（そのフォルダが先頭）。
pub fn descendants(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE sub(id) AS (
           SELECT id FROM folders WHERE id = ?1
           UNION
           SELECT f.id FROM folders f JOIN sub ON f.parent_id = sub.id
         )
         SELECT id FROM sub",
    )?;
    let rows = stmt.query_map([id], |row| row.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

fn clean_name(name: &str) -> Result<String> {
    let name = nfc(name.trim());
    if name.is_empty() {
        return Err(CoreError::Invalid(
            "フォルダの名前を入力してください".into(),
        ));
    }
    if name.contains('/') {
        return Err(CoreError::Invalid(
            "フォルダの名前に「/」は使えません".into(),
        ));
    }
    Ok(name)
}

fn ensure_exists(conn: &Connection, id: i64) -> Result<()> {
    let mut stmt = conn.prepare_cached("SELECT 1 FROM folders WHERE id = ?1")?;
    if stmt.exists([id])? {
        Ok(())
    } else {
        Err(CoreError::NotFound(format!("フォルダ {id}")))
    }
}

/// 同じ親の下に同じ名前のフォルダがないことを確かめる（`except` は自分自身）。
fn ensure_unique(
    conn: &Connection,
    parent: Option<i64>,
    name: &str,
    except: Option<i64>,
) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("SELECT id FROM folders WHERE ifnull(parent_id, 0) = ?1 AND name = ?2")?;
    let existing: Option<i64> = stmt
        .query_row(params![parent.unwrap_or(0), name], |row| row.get(0))
        .optional()?;
    match existing {
        Some(id) if Some(id) != except => Err(CoreError::Conflict(format!(
            "同じ場所に「{name}」というフォルダがあります"
        ))),
        _ => Ok(()),
    }
}

fn next_pos(conn: &Connection, parent: Option<i64>) -> Result<f64> {
    let mut stmt = conn.prepare_cached(
        "SELECT coalesce(max(pos), 0) + 1 FROM folders WHERE ifnull(parent_id, 0) = ?1",
    )?;
    Ok(stmt.query_row([parent.unwrap_or(0)], |row| row.get(0))?)
}

/// フォルダを作る。作ったフォルダの ID を返す。
pub fn create(conn: &Connection, parent: Option<i64>, name: &str, now: i64) -> Result<i64> {
    let name = clean_name(name)?;
    if let Some(p) = parent {
        ensure_exists(conn, p)?;
    }
    ensure_unique(conn, parent, &name, None)?;
    let pos = next_pos(conn, parent)?;
    let mut stmt = conn.prepare_cached(
        "INSERT INTO folders (parent_id, name, pos, created_at) VALUES (?1, ?2, ?3, ?4)",
    )?;
    stmt.execute(params![parent, name, pos, now])?;
    Ok(conn.last_insert_rowid())
}

/// 変更する項目（None は変えない。`color` の `Some(None)` は色を外す）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderPatch {
    pub name: Option<String>,
    pub color: Option<Option<String>>,
}

/// 名前・色を変える。
pub fn update(conn: &Connection, id: i64, patch: &FolderPatch) -> Result<()> {
    let parent: Option<Option<i64>> = {
        let mut stmt = conn.prepare_cached("SELECT parent_id FROM folders WHERE id = ?1")?;
        stmt.query_row([id], |row| row.get(0)).optional()?
    };
    let Some(parent) = parent else {
        return Err(CoreError::NotFound(format!("フォルダ {id}")));
    };
    if let Some(name) = &patch.name {
        let name = clean_name(name)?;
        ensure_unique(conn, parent, &name, Some(id))?;
        let mut stmt = conn.prepare_cached("UPDATE folders SET name = ?2 WHERE id = ?1")?;
        stmt.execute(params![id, name])?;
    }
    if let Some(color) = &patch.color {
        let color = check_color(color.as_deref())?;
        let mut stmt = conn.prepare_cached("UPDATE folders SET color = ?2 WHERE id = ?1")?;
        stmt.execute(params![id, color])?;
    }
    Ok(())
}

/// フォルダを `parent` の下（None はいちばん上）の、`before` の前（None は最後）へ移す。
pub fn move_to(conn: &Connection, id: i64, parent: Option<i64>, before: Option<i64>) -> Result<()> {
    let name: Option<String> = {
        let mut stmt = conn.prepare_cached("SELECT name FROM folders WHERE id = ?1")?;
        stmt.query_row([id], |row| row.get(0)).optional()?
    };
    let Some(name) = name else {
        return Err(CoreError::NotFound(format!("フォルダ {id}")));
    };
    if let Some(p) = parent {
        ensure_exists(conn, p)?;
        if descendants(conn, id)?.contains(&p) {
            return Err(CoreError::Invalid(
                "フォルダを自分自身や中のフォルダへは移動できません".into(),
            ));
        }
    }
    ensure_unique(conn, parent, &name, Some(id))?;

    let siblings: Vec<(i64, f64)> = {
        let mut stmt = conn.prepare_cached(
            "SELECT id, pos FROM folders WHERE ifnull(parent_id, 0) = ?1 AND id <> ?2 ORDER BY pos, id",
        )?;
        let rows = stmt.query_map(params![parent.unwrap_or(0), id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let pos = match before {
        None => siblings.last().map_or(1.0, |(_, p)| p + 1.0),
        Some(b) => {
            let Some(k) = siblings.iter().position(|(s, _)| *s == b) else {
                return Err(CoreError::Invalid(format!(
                    "フォルダ {b} は移動先の中にありません"
                )));
            };
            let next = siblings[k].1;
            let prev = if k == 0 {
                next - 1.0
            } else {
                siblings[k - 1].1
            };
            if next - prev < MIN_GAP {
                // 間隔が詰まったら 1, 2, 3… に振り直し、空けた場所に入れる
                let mut stmt = conn.prepare_cached("UPDATE folders SET pos = ?2 WHERE id = ?1")?;
                for (i, (sid, _)) in siblings.iter().enumerate() {
                    let n = i as f64 + if i >= k { 2.0 } else { 1.0 };
                    stmt.execute(params![sid, n])?;
                }
                k as f64 + 1.0
            } else {
                (prev + next) / 2.0
            }
        }
    };
    let mut stmt =
        conn.prepare_cached("UPDATE folders SET parent_id = ?2, pos = ?3 WHERE id = ?1")?;
    stmt.execute(params![id, parent, pos])?;
    Ok(())
}

/// フォルダと中のフォルダを消す（アイテムは残る）。参照するフォルダがなくなった folder ユニットも消す。
/// 消したフォルダの数を返す。
pub fn delete(conn: &Connection, id: i64) -> Result<usize> {
    let ids = descendants(conn, id)?;
    if ids.is_empty() {
        return Err(CoreError::NotFound(format!("フォルダ {id}")));
    }
    let mut stmt = conn.prepare_cached("DELETE FROM folders WHERE id = ?1")?;
    stmt.execute([id])?;
    let mut stmt = conn.prepare_cached(
        "DELETE FROM units WHERE kind = 'folder'
         AND NOT EXISTS (SELECT 1 FROM unit_folders f WHERE f.unit_id = units.id)",
    )?;
    stmt.execute([])?;
    Ok(ids.len())
}

/// アイテムを `add` のフォルダに入れ、`remove` のフォルダから外す。
pub fn set_items(conn: &Connection, item_ids: &[i64], add: &[i64], remove: &[i64]) -> Result<()> {
    for folder in add {
        ensure_exists(conn, *folder)?;
    }
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO item_folders (folder_id, item_id)
         SELECT ?1, id FROM items WHERE id = ?2",
    )?;
    let mut delete =
        conn.prepare_cached("DELETE FROM item_folders WHERE folder_id = ?1 AND item_id = ?2")?;
    for item in item_ids {
        for folder in add {
            insert.execute([folder, item])?;
        }
        for folder in remove {
            delete.execute([folder, item])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn names(conn: &Connection) -> Vec<(String, Option<i64>)> {
        tree(conn)
            .unwrap()
            .into_iter()
            .map(|n| (n.name, n.parent_id))
            .collect()
    }

    fn item(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (?1, ?2, 'jpg', 'image', 1, 't', 't', 0, 0)",
            params![id, vec![id as u8; 32]],
        )
        .unwrap();
    }

    #[test]
    fn creates_renames_and_colors() {
        let conn = test_conn();
        let photos = create(&conn, None, " 写真 ", 0).unwrap();
        let trip = create(&conn, Some(photos), "2026 旅行", 0).unwrap();
        assert_eq!(
            names(&conn),
            vec![("写真".into(), None), ("2026 旅行".into(), Some(photos))]
        );

        // 同じ親に同じ名前は作れない。別の親ならよい
        assert!(matches!(
            create(&conn, None, "写真", 0),
            Err(CoreError::Conflict(_))
        ));
        create(&conn, Some(photos), "写真", 0).unwrap();
        assert!(matches!(
            create(&conn, None, "a/b", 0),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            create(&conn, None, "  ", 0),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            create(&conn, Some(999), "x", 0),
            Err(CoreError::NotFound(_))
        ));

        update(
            &conn,
            trip,
            &FolderPatch {
                name: Some("旅行".into()),
                color: Some(Some("teal".into())),
            },
        )
        .unwrap();
        let node = tree(&conn)
            .unwrap()
            .into_iter()
            .find(|n| n.id == trip)
            .unwrap();
        assert_eq!(
            (node.name.as_str(), node.color.as_deref()),
            ("旅行", Some("teal"))
        );
        assert!(
            update(
                &conn,
                trip,
                &FolderPatch {
                    color: Some(Some("red".into())),
                    ..FolderPatch::default()
                }
            )
            .is_err()
        );
        // 自分と同じ名前への改名はそのまま通る
        update(
            &conn,
            trip,
            &FolderPatch {
                name: Some("旅行".into()),
                ..FolderPatch::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn moves_and_reorders_without_cycles() {
        let conn = test_conn();
        let a = create(&conn, None, "A", 0).unwrap();
        let b = create(&conn, None, "B", 0).unwrap();
        let c = create(&conn, None, "C", 0).unwrap();
        let a1 = create(&conn, Some(a), "A1", 0).unwrap();

        // C を A の前へ
        move_to(&conn, c, None, Some(a)).unwrap();
        let top: Vec<String> = tree(&conn)
            .unwrap()
            .into_iter()
            .filter(|n| n.parent_id.is_none())
            .map(|n| n.name)
            .collect();
        assert_eq!(top, vec!["C", "A", "B"]);

        // B を A の中へ。A を A1 の中へは移せない
        move_to(&conn, b, Some(a), None).unwrap();
        assert!(matches!(
            move_to(&conn, a, Some(a1), None),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            move_to(&conn, a, Some(a), None),
            Err(CoreError::Invalid(_))
        ));
        let paths: Vec<Vec<String>> = [a1, b]
            .iter()
            .map(|id| path_of(&tree(&conn).unwrap(), *id))
            .collect();
        assert_eq!(paths, vec![vec!["A", "A1"], vec!["A", "B"]]);

        // 移動先に同じ名前があれば競合
        let other = create(&conn, None, "A1", 0).unwrap();
        assert!(matches!(
            move_to(&conn, other, Some(a), None),
            Err(CoreError::Conflict(_))
        ));
    }

    #[test]
    fn reorders_even_when_positions_are_crowded() {
        let conn = test_conn();
        let ids: Vec<i64> = ["a", "b", "c"]
            .iter()
            .map(|n| create(&conn, None, n, 0).unwrap())
            .collect();
        conn.execute("UPDATE folders SET pos = 1.0", []).unwrap();
        conn.execute(
            "UPDATE folders SET pos = 1.0 + 1e-12 WHERE id = ?1",
            [ids[1]],
        )
        .unwrap();
        conn.execute(
            "UPDATE folders SET pos = 1.0 + 2e-12 WHERE id = ?1",
            [ids[2]],
        )
        .unwrap();
        move_to(&conn, ids[2], None, Some(ids[1])).unwrap();
        let order: Vec<String> = tree(&conn).unwrap().into_iter().map(|n| n.name).collect();
        assert_eq!(order, vec!["a", "c", "b"]);
    }

    #[test]
    fn delete_removes_subfolders_and_orphan_units_but_keeps_items() {
        let conn = test_conn();
        let a = create(&conn, None, "A", 0).unwrap();
        let a1 = create(&conn, Some(a), "A1", 0).unwrap();
        let b = create(&conn, None, "B", 0).unwrap();
        item(&conn, 1);
        set_items(&conn, &[1], &[a1], &[]).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (1, 'folder', 'u1', 'name', 0, 0), (2, 'folder', 'u2', 'name', 0, 0);
             INSERT INTO unit_folders (unit_id, folder_id, pos) VALUES (1, {a1}, 1), (2, {a1}, 1), (2, {b}, 2);"
        ))
        .unwrap();
        assert_eq!(
            tree(&conn)
                .unwrap()
                .iter()
                .find(|n| n.id == a1)
                .unwrap()
                .unit_id,
            Some(1)
        );

        assert_eq!(delete(&conn, a).unwrap(), 2);
        assert_eq!(names(&conn), vec![("B".into(), None)]);
        let items: i64 = conn
            .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(items, 1);
        // u1 は参照がなくなったので消え、u2 は B が残るので残る
        let units: Vec<i64> = conn
            .prepare("SELECT id FROM units")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(units, vec![2]);
        assert!(matches!(delete(&conn, a), Err(CoreError::NotFound(_))));
    }

    #[test]
    fn counts_include_subfolders_once_and_skip_trash() {
        let conn = test_conn();
        let a = create(&conn, None, "A", 0).unwrap();
        let a1 = create(&conn, Some(a), "A1", 0).unwrap();
        let b = create(&conn, None, "B", 0).unwrap();
        for id in 1..=4 {
            item(&conn, id);
        }
        set_items(&conn, &[1, 2], &[a1], &[]).unwrap();
        set_items(&conn, &[1, 3], &[a], &[]).unwrap();
        set_items(&conn, &[4], &[b], &[]).unwrap();
        conn.execute("UPDATE items SET trashed_at = 1 WHERE id = 3", [])
            .unwrap();
        let counts = item_counts(&conn).unwrap();
        assert_eq!(counts.get(&a), Some(&2)); // 1（A と A1 の両方）と 2
        assert_eq!(counts.get(&a1), Some(&2));
        assert_eq!(counts.get(&b), Some(&1));

        set_items(&conn, &[1], &[], &[a1, a]).unwrap();
        let counts = item_counts(&conn).unwrap();
        assert_eq!(counts.get(&a), Some(&1));
        // ないアイテムは無視し、ないフォルダはエラー
        set_items(&conn, &[99], &[b], &[]).unwrap();
        assert!(set_items(&conn, &[1], &[999], &[]).is_err());
        assert_eq!(descendants(&conn, a).unwrap(), vec![a, a1]);
    }
}
