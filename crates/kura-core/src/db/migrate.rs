//! スキーマのマイグレーション。`PRAGMA user_version` に適用済みの数を記録する。

use rusqlite::Connection;

use crate::{CoreError, Result};

/// マイグレーションの一覧（連番順）。適用済みのファイルは編集しない。
pub const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../../migrations/0001_init.sql")),
    (
        "0002_thumb_pending_index",
        include_str!("../../migrations/0002_thumb_pending_index.sql"),
    ),
    (
        "0003_proxy_todo_index",
        include_str!("../../migrations/0003_proxy_todo_index.sql"),
    ),
    (
        "0004_playlist_generation",
        include_str!("../../migrations/0004_playlist_generation.sql"),
    ),
    (
        "0005_export_job",
        include_str!("../../migrations/0005_export_job.sql"),
    ),
    (
        "0006_unit_order_reverse",
        include_str!("../../migrations/0006_unit_order_reverse.sql"),
    ),
];

/// 未適用のマイグレーションを順に適用する。DB の方が新しければエラー。
pub fn migrate(conn: &mut Connection) -> Result<()> {
    migrate_until(conn, MIGRATIONS.len())
}

/// `count` 番目までのマイグレーションを適用する（テストで前の版のスキーマを作るのにも使う）。
pub(crate) fn migrate_until(conn: &mut Connection, count: usize) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let latest = i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);
    if current > latest {
        return Err(CoreError::Conflict(format!(
            "このライブラリは新しい版の Kura で作られています（スキーマ {current}、対応 {latest}）"
        )));
    }
    for (index, (name, sql)) in MIGRATIONS.iter().enumerate().take(count) {
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
    fn thumb_pending_index_keeps_existing_items() {
        // 0001 だけの DB にアイテムを入れてから 0002 を適用する
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_until(&mut conn, 1).unwrap();
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (1, x'01', 'jpg', 'image', 1, 'a', 'a.jpg', 10, 10)",
            [],
        )
        .unwrap();
        migrate(&mut conn).unwrap();

        let (count, state): (i64, String) = conn
            .query_row("SELECT COUNT(*), max(thumb_state) FROM items", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((count, state.as_str()), (1, "pending"));
        let plan = crate::db::query_plan(
            &conn,
            "SELECT id FROM items WHERE thumb_state = 'pending'
             ORDER BY imported_at DESC, id DESC LIMIT 64",
        );
        assert!(plan.contains("idx_items_thumb_pending"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }

    #[test]
    fn proxy_todo_index_settles_images_and_keeps_items() {
        // 0002 までの DB に画像・動画を入れてから 0003 を適用する
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_until(&mut conn, 2).unwrap();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (1, x'01', 'jpg', 'image', 1, 'a', 'a.jpg', 10, 10),
                    (2, x'02', 'mkv', 'video', 1, 'b', 'b.mkv', 20, 20),
                    (3, x'03', 'ogg', 'audio', 1, 'c', 'c.ogg', 30, 30);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();

        let states: Vec<(i64, String)> = conn
            .prepare("SELECT id, proxy_state FROM items ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            states,
            vec![
                (1, "not_needed".to_string()),
                (2, "unknown".to_string()),
                (3, "unknown".to_string())
            ]
        );
        for (state, index) in [
            ("unknown", "idx_items_proxy_unknown"),
            ("pending", "idx_items_proxy_pending"),
        ] {
            let plan = crate::db::query_plan(
                &conn,
                &format!(
                    "SELECT id FROM items WHERE proxy_state = '{state}' AND kind != 'image'
                     ORDER BY imported_at DESC, id DESC LIMIT 16"
                ),
            );
            assert!(plan.contains(index), "{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        }
    }

    #[test]
    fn playlist_generation_columns_keep_existing_playlists() {
        // 0003 までの DB にプレイリストと再開の状態を入れてから 0004 を適用する
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_until(&mut conn, 3).unwrap();
        conn.execute_batch(
            "INSERT INTO playlists (id, name, kind, rule_json, pos, created_at, updated_at)
             VALUES (1, '夜の作業', 'manual', NULL, 1, 10, 10);
             INSERT INTO playlist_state (playlist_id, seed, slot_index, position_ms, updated_at)
             VALUES (1, 7, 3, 1500, 20);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();

        let row: (String, Option<i64>, String, i64) = conn
            .query_row(
                "SELECT name, generated_at, sort_key, sort_desc FROM playlists WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(row, ("夜の作業".into(), None, "imported".into(), 1));
        let state: (i64, i64, Option<i64>) = conn
            .query_row(
                "SELECT seed, slot_index, item_id FROM playlist_state WHERE playlist_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(state, (7, 3, None));
        // 並び順は決まった値だけ
        assert!(
            conn.execute("UPDATE playlists SET sort_key = 'color' WHERE id = 1", [])
                .is_err()
        );
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

    #[test]
    fn unit_order_reverse_defaults_to_off_for_existing_units() {
        // 0005 までの DB にユニットを入れてから 0006 を適用する
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_until(&mut conn, 5).unwrap();
        conn.execute_batch(
            "INSERT INTO units (id, kind, name, order_mode, inner_shuffle, rating, note, created_at, updated_at)
             VALUES (3, 'set', '鎌倉', 'name', 1, 4, 'メモ', 10, 20);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();

        let row: (String, String, i64, i64, Option<String>, i64) = conn
            .query_row(
                "SELECT name, order_mode, inner_shuffle, rating, note, order_reverse FROM units WHERE id = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            ("鎌倉".into(), "name".into(), 1, 4, Some("メモ".into()), 0)
        );
        conn.execute("UPDATE units SET order_reverse = 1 WHERE id = 3", [])
            .unwrap();
        assert!(
            conn.execute("UPDATE units SET order_reverse = 2 WHERE id = 3", [])
                .is_err()
        );
    }

    #[test]
    fn export_job_kind_keeps_existing_jobs() {
        // 0004 までの DB にジョブを入れてから 0005 を適用する
        let mut conn = Connection::open_in_memory().unwrap();
        migrate_until(&mut conn, 4).unwrap();
        conn.execute_batch(
            "INSERT INTO jobs (id, kind, state, priority, input_json, progress, message, result_json, created_at, started_at)
             VALUES (7, 'download', 'paused', 2, '{\"url\":\"https://example.com\"}', 0.5, '途中', '{}', 100, 110),
                    (8, 'import', 'done', 0, '{}', 1.0, NULL, NULL, 200, 210);",
        )
        .unwrap();
        // 前の版では書き出しのジョブは入らない
        assert!(
            conn.execute(
                "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('export', 'queued', '{}', 1)",
                [],
            )
            .is_err()
        );
        migrate(&mut conn).unwrap();

        type JobRow = (
            i64,
            String,
            String,
            i64,
            String,
            Option<f64>,
            Option<String>,
        );
        let rows: Vec<JobRow> = conn
            .prepare("SELECT id, kind, state, priority, input_json, progress, message FROM jobs ORDER BY id")
            .unwrap()
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            (
                7,
                "download".into(),
                "paused".into(),
                2,
                "{\"url\":\"https://example.com\"}".into(),
                Some(0.5),
                Some("途中".into())
            )
        );
        conn.execute(
            "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('export', 'queued', '{}', 300)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO jobs (kind, state, input_json, created_at) VALUES ('other', 'queued', '{}', 1)",
                [],
            )
            .is_err()
        );
        let plan = crate::db::query_plan(
            &conn,
            "SELECT id FROM jobs WHERE state = 'queued' ORDER BY priority DESC, created_at LIMIT 1",
        );
        assert!(plan.contains("idx_jobs_queue"), "{plan}");
    }
}
