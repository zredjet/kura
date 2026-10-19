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
        .prepare(
            "SELECT id, kind, state, priority, input_json, progress, message FROM jobs ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
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

// ===== runner：Rebuild（docs/data-model.md §5）=====

use crate::db::testing::{columns, fill_v6, one, snapshot, snapshot_cols, tables};

/// テスト専用の Rebuild：0006 の items を、列を 1 つ足して作り直す（索引 9 つと FTS のトリガ 3 つも作り直す）。
const TEST_REBUILD_ITEMS: &str = "
CREATE TABLE items_new (
  id              INTEGER PRIMARY KEY,
  hash            BLOB    NOT NULL UNIQUE,
  ext             TEXT    NOT NULL,
  kind            TEXT    NOT NULL CHECK (kind IN ('image','video','audio')),
  mime            TEXT,
  size_bytes      INTEGER NOT NULL,
  width           INTEGER,
  height          INTEGER,
  duration_ms     INTEGER,
  codec_info      TEXT,
  title           TEXT    NOT NULL,
  original_name   TEXT    NOT NULL,
  original_path   TEXT,
  source_url      TEXT,
  source_site     TEXT,
  rating          INTEGER NOT NULL DEFAULT 0 CHECK (rating BETWEEN 0 AND 5),
  note            TEXT,
  captured_at     INTEGER,
  imported_at     INTEGER NOT NULL,
  updated_at      INTEGER NOT NULL,
  trashed_at      INTEGER,
  play_count      INTEGER NOT NULL DEFAULT 0,
  last_played_at  INTEGER,
  thumb_state     TEXT    NOT NULL DEFAULT 'pending' CHECK (thumb_state IN ('pending','ready','failed')),
  proxy_state     TEXT    NOT NULL DEFAULT 'unknown' CHECK (proxy_state IN ('unknown','not_needed','pending','ready','failed')),
  test_added      INTEGER NOT NULL DEFAULT 0
);
INSERT INTO items_new (id, hash, ext, kind, mime, size_bytes, width, height, duration_ms, codec_info, title,
                       original_name, original_path, source_url, source_site, rating, note, captured_at,
                       imported_at, updated_at, trashed_at, play_count, last_played_at, thumb_state, proxy_state)
  SELECT id, hash, ext, kind, mime, size_bytes, width, height, duration_ms, codec_info, title,
         original_name, original_path, source_url, source_site, rating, note, captured_at,
         imported_at, updated_at, trashed_at, play_count, last_played_at, thumb_state, proxy_state
  FROM items;
DROP TABLE items;
ALTER TABLE items_new RENAME TO items;
CREATE INDEX idx_items_imported ON items(imported_at DESC, id DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_kind     ON items(kind, imported_at DESC) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_rating   ON items(rating) WHERE trashed_at IS NULL;
CREATE INDEX idx_items_played   ON items(last_played_at);
CREATE INDEX idx_items_trashed  ON items(trashed_at) WHERE trashed_at IS NOT NULL;
CREATE INDEX idx_items_size     ON items(size_bytes);
CREATE INDEX idx_items_thumb_pending ON items(imported_at DESC, id DESC) WHERE thumb_state = 'pending';
CREATE INDEX idx_items_proxy_unknown ON items(imported_at DESC, id DESC) WHERE proxy_state = 'unknown';
CREATE INDEX idx_items_proxy_pending ON items(imported_at DESC, id DESC) WHERE proxy_state = 'pending';
CREATE TRIGGER items_fts_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, title, original_name, note, source_url)
  VALUES (new.id, new.title, new.original_name, new.note, new.source_url);
END;
CREATE TRIGGER items_fts_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, original_name, note, source_url)
  VALUES ('delete', old.id, old.title, old.original_name, old.note, old.source_url);
END;
CREATE TRIGGER items_fts_au AFTER UPDATE OF title, original_name, note, source_url ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, title, original_name, note, source_url)
  VALUES ('delete', old.id, old.title, old.original_name, old.note, old.source_url);
  INSERT INTO items_fts(rowid, title, original_name, note, source_url)
  VALUES (new.id, new.title, new.original_name, new.note, new.source_url);
END;
";

const TEST_REBUILD: Migration = rebuild("test_rebuild_items", TEST_REBUILD_ITEMS);

/// 版 `n` まで当てたインメモリの DB（foreign_keys は ON）。
fn at(n: usize) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    migrate_until(&mut conn, n).unwrap();
    conn
}

/// 0006 の DB を、すべての表を埋めて作る。
fn filled_v6() -> Connection {
    let conn = at(6);
    fill_v6(&conn);
    conn
}

/// 0006 の DB に 0006 までと `extra` を並べた一覧。
fn v6_then(extra: Migration) -> Vec<Migration> {
    MIGRATIONS.iter().take(6).copied().chain([extra]).collect()
}

#[test]
fn fixture_fills_every_table() {
    // 表を足したら fill_v6 にも行を足す（空の表は「残る」ことを確かめられない）
    let conn = filled_v6();
    for t in tables(&conn) {
        let n: i64 = one(&conn, &format!("SELECT count(*) FROM \"{t}\""));
        assert!(n > 0, "{t} が空");
    }
}

#[test]
fn rebuild_keeps_children_fts_indexes_and_triggers() {
    let mut conn = filled_v6();
    let cols = columns(&conn);
    let before = snapshot_cols(&conn, &cols);
    assert_eq!(before["fts integrity"], "Ok(())");
    let tolerated = apply_rebuild(&mut conn, &TEST_REBUILD, 7).unwrap();
    assert_eq!(tolerated, 0);
    let after = snapshot_cols(&conn, &cols);
    for (key, value) in &before {
        assert_eq!(after.get(key), Some(value), "{key} が変わった");
    }
    assert_eq!(
        after.keys().filter(|k| !before.contains_key(*k)).count(),
        0,
        "増えたものはない"
    );
    assert_eq!(applied(&conn).unwrap(), 7);
    assert_eq!(one::<i64>(&conn, "PRAGMA foreign_keys"), 1);
    assert!(violations(&conn).unwrap().is_empty());
    // 足した列は最後にある
    let names: String = one(
        &conn,
        "SELECT group_concat(name, ',') FROM pragma_table_info('items')",
    );
    assert!(names.ends_with("proxy_state,test_added"), "{names}");
    // 作り直したあとも、子の CASCADE と FTS のトリガが新しい items に効く
    conn.execute("DELETE FROM items WHERE id = 1", []).unwrap();
    assert_eq!(
        one::<i64>(&conn, "SELECT count(*) FROM item_tags WHERE item_id = 1"),
        0
    );
    assert_eq!(
        one::<i64>(
            &conn,
            "SELECT count(*) FROM items_fts WHERE items_fts MATCH '\"夕方の海\"'"
        ),
        0
    );
}

#[test]
fn naive_rebuild_in_a_transaction_deletes_children() {
    // なぜ Rebuild が要るか（Plain の当て方）。PRAGMA foreign_keys = OFF はトランザクションの中では効かない
    let mut conn = filled_v6();
    let tx = conn.transaction().unwrap();
    tx.execute_batch(&format!("PRAGMA foreign_keys = OFF;\n{TEST_REBUILD_ITEMS}"))
        .unwrap();
    tx.commit().unwrap();
    let tags: i64 = one(&conn, "SELECT count(*) FROM item_tags");
    let cover: Option<i64> = one(&conn, "SELECT cover_item_id FROM units WHERE id = 1");
    assert_eq!((tags, cover), (0, None));
}

#[test]
fn rebuild_rolls_back_and_restores_foreign_keys_when_it_would_break_references() {
    let mut conn = filled_v6();
    let before = snapshot(&conn);
    // 子（item_folders・unit_folders・playlist_entries・capture_rules）が参照するフォルダを消す
    let bad = rebuild("bad", "DELETE FROM folders WHERE id = 2;");
    let err = apply_rebuild(&mut conn, &bad, 7).unwrap_err();
    assert!(matches!(err, CoreError::Conflict(_)), "{err}");
    assert!(err.to_string().contains("bad"), "{err}");
    assert_eq!(applied(&conn).unwrap(), 6);
    assert_eq!(one::<i64>(&conn, "PRAGMA foreign_keys"), 1);
    assert_eq!(snapshot(&conn), before);
    // SQL の途中の文の誤りでも戻る
    let broken = rebuild(
        "broken",
        "DELETE FROM folders WHERE id = 2; SELECT * FROM no_such_table;",
    );
    assert!(apply_rebuild(&mut conn, &broken, 7).is_err());
    assert_eq!(one::<i64>(&conn, "PRAGMA foreign_keys"), 1);
    assert_eq!(snapshot(&conn), before);
}

#[test]
fn rebuild_tolerates_violations_that_existed_before() {
    // 外部のツールで直したなどで、当てる前から違反がある DB も開ける（新しい違反だけを断る）
    let broken_v6 = || {
        let conn = filled_v6();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO item_tags (tag_id, item_id) VALUES (1, 999);
             PRAGMA foreign_keys = ON;",
        )
        .unwrap();
        conn
    };
    let mut conn = broken_v6();
    let tolerated = apply_rebuild(&mut conn, &TEST_REBUILD, 7).unwrap();
    assert_eq!(tolerated, 1);
    assert_eq!(applied(&conn).unwrap(), 7);
    assert_eq!(violations(&conn).unwrap().len(), 1);
    assert_eq!(one::<i64>(&conn, "PRAGMA foreign_keys"), 1);
    // 前からの違反と同じ形（WITHOUT ROWID なので rowid はない）でも、数が増えれば新しい違反として断る
    let mut conn = broken_v6();
    let more = rebuild(
        "more",
        "INSERT INTO item_tags (tag_id, item_id) VALUES (2, 999);",
    );
    assert!(matches!(
        apply_rebuild(&mut conn, &more, 7),
        Err(CoreError::Conflict(_))
    ));
    assert_eq!(applied(&conn).unwrap(), 6);
    assert_eq!(one::<i64>(&conn, "PRAGMA foreign_keys"), 1);
}

#[test]
fn rebuild_refuses_inside_a_transaction() {
    let mut conn = filled_v6();
    conn.execute_batch("BEGIN;").unwrap();
    assert!(matches!(
        apply_rebuild(&mut conn, &rebuild("x", "SELECT 1;"), 7),
        Err(CoreError::Invalid(_))
    ));
    conn.execute_batch("ROLLBACK;").unwrap();
    assert_eq!(one::<i64>(&conn, "PRAGMA foreign_keys"), 1);
    assert_eq!(applied(&conn).unwrap(), 6);
}

#[test]
fn rebuild_runs_through_the_runner_and_reports() {
    let mut conn = filled_v6();
    let list = v6_then(TEST_REBUILD);
    let report = run(&mut conn, &list, list.len()).unwrap();
    assert_eq!((report.from, report.to), (6, 7));
    assert_eq!(report.applied.len(), 1);
    assert_eq!(report.applied[0].name, "test_rebuild_items");
    assert_eq!(report.tolerated_violations, 0);
    assert_eq!(one::<i64>(&conn, "SELECT count(*) FROM item_tags"), 3);
    // 失敗したら、どのマイグレーションかを返す
    let mut conn = filled_v6();
    let bad = v6_then(rebuild("bad", "DELETE FROM folders;"));
    let (index, err) = run(&mut conn, &bad, bad.len()).unwrap_err();
    assert_eq!(index, Some(6));
    assert!(matches!(err, CoreError::Conflict(_)), "{err}");
    assert_eq!(applied(&conn).unwrap(), 6);
}

#[test]
fn plain_migrations_never_drop_a_table_that_others_reference() {
    let mut conn = Connection::open_in_memory().unwrap();
    for (index, m) in MIGRATIONS.iter().enumerate() {
        if m.kind == Kind::Plain {
            for word in m.sql.split("DROP TABLE").skip(1) {
                let table = word
                    .split(|c: char| c == ';' || c.is_whitespace())
                    .find(|s| !s.is_empty())
                    .unwrap();
                let referenced: i64 = conn
                    .query_row(
                        "SELECT count(*) FROM sqlite_schema s, pragma_foreign_key_list(s.name) f
                          WHERE s.type = 'table' AND f.\"table\" = ?1",
                        [table],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(
                    referenced, 0,
                    "{} が参照される {table} を DROP する（Rebuild にする）",
                    m.name
                );
            }
        }
        migrate_until(&mut conn, index + 1).unwrap();
    }
}

#[test]
fn reports_what_it_applied() {
    let mut conn = Connection::open_in_memory().unwrap();
    let report = migrate(&mut conn).unwrap();
    assert_eq!((report.from, report.to), (0, MIGRATIONS.len()));
    assert_eq!(
        report.applied.iter().map(|a| a.name).collect::<Vec<_>>(),
        MIGRATIONS.iter().map(|m| m.name).collect::<Vec<_>>()
    );
    let again = migrate(&mut conn).unwrap();
    assert!(again.applied.is_empty());
    assert_eq!((again.from, again.to), (MIGRATIONS.len(), MIGRATIONS.len()));
    assert!(!pending(&conn).unwrap());
}
