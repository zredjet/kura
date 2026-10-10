//! スキーマのマイグレーション。`PRAGMA user_version` に適用済みの数を記録する。

use rusqlite::Connection;

use crate::{CoreError, Result};

/// マイグレーションの一覧（連番順）。適用済みのファイルは編集しない。
pub const MIGRATIONS: &[(&str, &str)] =
    &[("0001_init", include_str!("../../migrations/0001_init.sql"))];

/// 未適用のマイグレーションを順に適用する。DB の方が新しければエラー。
pub fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let latest = i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);
    if current > latest {
        return Err(CoreError::Conflict(format!(
            "このライブラリは新しい版の Kura で作られています（スキーマ {current}、対応 {latest}）"
        )));
    }
    for (index, (name, sql)) in MIGRATIONS.iter().enumerate() {
        let version = i64::try_from(index).unwrap_or(i64::MAX) + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(migration = name, "マイグレーションを適用しました");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_names(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn applies_all_migrations_to_empty_db() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();

        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);

        let tables = table_names(&conn);
        for expected in [
            "items",
            "folders",
            "item_folders",
            "tags",
            "tag_aliases",
            "tag_parents",
            "item_tags",
            "units",
            "unit_items",
            "unit_folders",
            "unit_tags",
            "smart_folders",
            "playlists",
            "playlist_entries",
            "playlist_state",
            "play_events",
            "jobs",
            "remote_sources",
            "capture_rules",
            "settings",
            "items_fts",
        ] {
            assert!(tables.iter().any(|t| t == expected), "{expected} がない");
        }
    }

    #[test]
    fn running_twice_is_a_no_op() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        migrate(&mut conn).unwrap();
    }

    #[test]
    fn refuses_newer_schema() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        assert!(matches!(migrate(&mut conn), Err(CoreError::Conflict(_))));
    }

    #[test]
    fn fts_follows_item_changes() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (1, x'00', 'jpg', 'image', 1, '夕方の海', '夕方の海.jpg', 0, 0)",
            [],
        )
        .unwrap();
        let hits = |q: &str| -> i64 {
            conn.query_row(
                "SELECT COUNT(*) FROM items_fts WHERE items_fts MATCH ?1",
                [q],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(hits("夕方の"), 1);

        conn.execute("UPDATE items SET title = '港の朝' WHERE id = 1", [])
            .unwrap();
        assert_eq!(hits("港の朝"), 1);
        assert_eq!(hits("title:夕方の"), 0);

        conn.execute("DELETE FROM items WHERE id = 1", []).unwrap();
        assert_eq!(hits("港の朝"), 0);
    }
}
