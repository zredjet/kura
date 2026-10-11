//! 選択（アイテムとユニットのタイル）への評価・メモ・タグの付け外し。
//!
//! ユニットへの操作は、ユニット自体とメンバー全員の両方に付ける（docs/playlists.md §2.1、ADR-129）。

use std::collections::HashSet;

use rusqlite::Connection;

use crate::{Result, items, tags, unit};

/// まとめて変える項目（None は変えない。`note` の `Some(None)` はメモを消す）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryPatch {
    pub rating: Option<i64>,
    pub note: Option<Option<String>>,
    /// 付けるタグの名前。
    pub add_tags: Vec<String>,
    /// 外すタグの ID。
    pub remove_tags: Vec<i64>,
}

/// 変えた結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryUpdate {
    /// 変えたアイテム（選んだアイテムとユニットのメンバー。重複なし、選んだ順）。
    pub item_ids: Vec<i64>,
    /// そのうちユニットのメンバーの数（知らせに出す）。
    pub members: usize,
}

/// アイテムとユニット（とそのメンバー全員）に、評価・メモ・タグをまとめて付け外しする。
pub fn update(
    conn: &Connection,
    item_ids: &[i64],
    unit_ids: &[i64],
    patch: &EntryPatch,
    now: i64,
) -> Result<EntryUpdate> {
    let mut seen = HashSet::new();
    let mut targets: Vec<i64> = item_ids
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .collect();
    let mut members = HashSet::new();
    for &id in unit_ids {
        for member in unit::members(conn, id)? {
            members.insert(member);
            if seen.insert(member) {
                targets.push(member);
            }
        }
    }

    if patch.rating.is_some() || patch.note.is_some() {
        let item_patch = items::ItemPatch {
            title: None,
            rating: patch.rating,
            note: patch.note.clone(),
        };
        items::update(conn, &targets, &item_patch, now)?;
        let unit_patch = unit::UnitPatch {
            rating: patch.rating,
            note: patch.note.clone(),
            ..unit::UnitPatch::default()
        };
        for &id in unit_ids {
            unit::update(conn, id, &unit_patch, now)?;
        }
    }
    if !patch.add_tags.is_empty() || !patch.remove_tags.is_empty() {
        tags::apply(conn, &targets, &patch.add_tags, &patch.remove_tags, now)?;
        tags::apply_to_units(conn, unit_ids, &patch.add_tags, &patch.remove_tags, now)?;
    }
    Ok(EntryUpdate {
        item_ids: targets,
        members: members.len(),
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::CoreError;
    use crate::db::test_conn;

    fn item(conn: &Connection, id: i64) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (?1, ?2, 'jpg', 'image', 1, 't', 't', 0, 0)",
            params![id, vec![id as u8; 32]],
        )
        .unwrap();
    }

    fn rating_of(conn: &Connection, table: &str, id: i64) -> i64 {
        conn.query_row(
            &format!("SELECT rating FROM {table} WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn note_of(conn: &Connection, table: &str, id: i64) -> Option<String> {
        conn.query_row(
            &format!("SELECT note FROM {table} WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn item_tags(conn: &Connection, id: i64) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT t.name FROM item_tags m JOIN tags t ON t.id = m.tag_id
                 WHERE m.item_id = ?1 ORDER BY t.name",
            )
            .unwrap();
        stmt.query_map([id], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    fn unit_tags(conn: &Connection, id: i64) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT t.name FROM unit_tags m JOIN tags t ON t.id = m.tag_id
                 WHERE m.unit_id = ?1 ORDER BY t.name",
            )
            .unwrap();
        stmt.query_map([id], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// アイテム 1〜5、ユニット A = [1, 2, 3]、ユニット B = [3, 4]。
    fn library(conn: &Connection) -> (i64, i64) {
        for id in 1..=5 {
            item(conn, id);
        }
        let a = unit::create_set(conn, "A", &[1, 2, 3], 0).unwrap();
        let b = unit::create_set(conn, "B", &[3, 4], 0).unwrap();
        (a, b)
    }

    #[test]
    fn rating_a_unit_also_rates_every_member() {
        let conn = test_conn();
        let (a, _) = library(&conn);
        let patch = EntryPatch {
            rating: Some(4),
            ..EntryPatch::default()
        };
        let done = update(&conn, &[], &[a], &patch, 10).unwrap();
        assert_eq!(done.item_ids, vec![1, 2, 3]);
        assert_eq!(done.members, 3);
        assert_eq!(rating_of(&conn, "units", a), 4);
        for id in [1, 2, 3] {
            assert_eq!(rating_of(&conn, "items", id), 4);
        }
        assert_eq!(rating_of(&conn, "items", 4), 0);
    }

    #[test]
    fn mixes_items_and_units_without_counting_members_twice() {
        let conn = test_conn();
        let (a, b) = library(&conn);
        let patch = EntryPatch {
            note: Some(Some(" 夕方 ".into())),
            ..EntryPatch::default()
        };
        let done = update(&conn, &[5, 3], &[a, b], &patch, 10).unwrap();
        assert_eq!(done.item_ids, vec![5, 3, 1, 2, 4]);
        // 3 は直接選んでいるが、メンバーでもある
        assert_eq!(done.members, 4);
        for id in 1..=5 {
            assert_eq!(note_of(&conn, "items", id).as_deref(), Some("夕方"));
        }
        assert_eq!(note_of(&conn, "units", a).as_deref(), Some("夕方"));
        assert_eq!(note_of(&conn, "units", b).as_deref(), Some("夕方"));

        let clear = EntryPatch {
            note: Some(None),
            ..EntryPatch::default()
        };
        update(&conn, &[], &[a], &clear, 11).unwrap();
        assert_eq!(note_of(&conn, "units", a), None);
        assert_eq!(note_of(&conn, "items", 1), None);
        assert_eq!(note_of(&conn, "items", 4).as_deref(), Some("夕方"));
    }

    #[test]
    fn tags_go_on_and_come_off_the_unit_and_its_members() {
        let conn = test_conn();
        let (a, _) = library(&conn);
        let add = EntryPatch {
            add_tags: vec!["夕景".into(), "鎌倉".into()],
            ..EntryPatch::default()
        };
        update(&conn, &[], &[a], &add, 10).unwrap();
        assert_eq!(unit_tags(&conn, a), vec!["夕景", "鎌倉"]);
        for id in [1, 2, 3] {
            assert_eq!(item_tags(&conn, id), vec!["夕景", "鎌倉"]);
        }
        assert!(item_tags(&conn, 4).is_empty());

        let tag: i64 = conn
            .query_row("SELECT id FROM tags WHERE name = '夕景'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // メンバーにだけ付いているタグ（自動で付いたものなど）もユニットから外せる
        tags::apply(&conn, &[2], &["pixiv".into()], &[], 10).unwrap();
        let pixiv: i64 = conn
            .query_row("SELECT id FROM tags WHERE name = 'pixiv'", [], |r| r.get(0))
            .unwrap();
        let remove = EntryPatch {
            remove_tags: vec![tag, pixiv],
            ..EntryPatch::default()
        };
        update(&conn, &[], &[a], &remove, 11).unwrap();
        assert_eq!(unit_tags(&conn, a), vec!["鎌倉"]);
        for id in [1, 2, 3] {
            assert_eq!(item_tags(&conn, id), vec!["鎌倉"]);
        }
    }

    #[test]
    fn leaves_trashed_members_alone() {
        let conn = test_conn();
        let (a, _) = library(&conn);
        conn.execute("UPDATE items SET trashed_at = 1 WHERE id = 2", [])
            .unwrap();
        let patch = EntryPatch {
            rating: Some(5),
            ..EntryPatch::default()
        };
        let done = update(&conn, &[], &[a], &patch, 10).unwrap();
        assert_eq!(done.item_ids, vec![1, 3]);
        assert_eq!(rating_of(&conn, "items", 2), 0);
    }

    #[test]
    fn rejects_a_bad_rating_before_changing_anything() {
        let conn = test_conn();
        let (a, _) = library(&conn);
        let patch = EntryPatch {
            rating: Some(6),
            ..EntryPatch::default()
        };
        assert!(matches!(
            update(&conn, &[4], &[a], &patch, 10),
            Err(CoreError::Invalid(_))
        ));
        assert_eq!(rating_of(&conn, "units", a), 0);
        assert_eq!(rating_of(&conn, "items", 1), 0);
    }

    #[test]
    fn unknown_units_are_an_error() {
        let conn = test_conn();
        library(&conn);
        let patch = EntryPatch {
            rating: Some(3),
            ..EntryPatch::default()
        };
        assert!(update(&conn, &[], &[99], &patch, 10).is_err());
    }
}
