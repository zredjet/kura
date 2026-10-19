//! テストの道具：0006 までのすべての表を埋める準備と、中身・索引・トリガ・FTS のスナップショット。

use std::collections::BTreeMap;

use rusqlite::Connection;

/// 32 バイトのハッシュの SQL のリテラル（`x'0101…'`）。
pub(crate) fn h(b: u8) -> String {
    format!("x'{}'", format!("{b:02x}").repeat(32))
}

/// 1 つの値を読む。
pub(crate) fn one<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// 0006 までの DB に、items を参照するすべての子と、それ以外のすべての表の行を入れる
/// （表を足したらここにも行を足す。`fixture_fills_every_table` が見張る）。
pub(crate) fn fill_v6(conn: &Connection) {
    let sql = format!(
        "INSERT INTO items (id, hash, ext, kind, mime, size_bytes, width, height, duration_ms, codec_info, title,
                            original_name, original_path, source_url, source_site, rating, note, captured_at,
                            imported_at, updated_at, trashed_at, play_count, last_played_at, thumb_state, proxy_state)
           VALUES (1, {h1}, 'jpg', 'image', 'image/jpeg', 10, 1600, 900, NULL, NULL,
                   '夕方の海', '夕方の海.jpg', '/Users/a/夕方の海.jpg', NULL, 'local', 4, 'メモ', 5, 10, 11, NULL, 2, 12, 'ready', 'not_needed'),
                  (2, {h2}, 'mkv', 'video', NULL, 20, 1920, 1080, 61000, '{{\"codec\":\"h264\"}}',
                   '港の朝', '港の朝.mkv', NULL, 'https://example.com/v/2', 'youtube', 0, NULL, NULL, 20, 21, NULL, 0, NULL, 'pending', 'unknown'),
                  (3, {h3}, 'ogg', 'audio', NULL, 30, NULL, NULL, 180000, NULL,
                   '雨', '雨.ogg', NULL, NULL, 'local', 0, NULL, NULL, 30, 31, 40, 0, NULL, 'failed', 'pending'),
                  (4, x'04', 'png', 'image', NULL, 1, 1, 1, NULL, NULL, '短い', '短い.png', NULL, NULL, NULL, 0, NULL, NULL, 40, 40, NULL, 0, NULL, 'pending', 'unknown');
         INSERT INTO folders (id, parent_id, name, color, pos, created_at) VALUES (1, NULL, '写真', 'blue', 1, 1), (2, 1, '鎌倉', NULL, 1, 1);
         INSERT INTO item_folders (folder_id, item_id, pos) VALUES (1, 1, 1), (2, 2, NULL), (2, 3, 2);
         INSERT INTO tags (id, namespace, name, color, created_at) VALUES (1, '', '海', NULL, 1), (2, '作者', 'A', 'teal', 1);
         INSERT INTO tag_aliases (alias, tag_id) VALUES ('うみ', 1);
         INSERT INTO tag_parents (child_id, parent_id) VALUES (2, 1);
         INSERT INTO item_tags (tag_id, item_id, source) VALUES (1, 1, 'user'), (2, 2, 'download'), (1, 3, 'import');
         INSERT INTO units (id, kind, name, cover_item_id, order_mode, inner_shuffle, rating, note, origin, created_at, updated_at, order_reverse)
           VALUES (1, 'set', '鎌倉', 2, 'manual', 1, 3, 'メモ', 'download', 1, 2, 1), (2, 'folder', '写真', NULL, 'name', 0, 0, NULL, 'user', 1, 1, 0);
         INSERT INTO unit_items (unit_id, item_id, pos) VALUES (1, 1, 1), (1, 2, 2);
         INSERT INTO unit_folders (unit_id, folder_id, recursive, pos) VALUES (2, 1, 1, 1);
         INSERT INTO unit_tags (tag_id, unit_id) VALUES (1, 1);
         INSERT INTO smart_folders (id, name, query, pos, created_at) VALUES (1, '海', '海 種類:画像', 1, 1);
         INSERT INTO playlists (id, name, kind, pos, created_at, updated_at, generated_at, sort_key, sort_desc)
           VALUES (1, '夜', 'manual', 1, 1, 1, NULL, 'imported', 1), (2, '海', 'smart', 2, 1, 1, 9, 'name', 0);
         INSERT INTO playlist_entries (id, playlist_id, pos, entry_type, item_id, unit_id, folder_id, start_ms)
           VALUES (1, 1, 1, 'item', 1, NULL, NULL, 500), (2, 1, 2, 'unit', NULL, 1, NULL, NULL), (3, 1, 3, 'folder', NULL, NULL, 2, NULL), (4, 2, 1, 'item', 3, NULL, NULL, NULL);
         INSERT INTO playlist_state (playlist_id, seed, slot_index, position_ms, updated_at, item_id) VALUES (1, 7, 3, 1500, 20, 2);
         INSERT INTO play_events (id, item_id, unit_id, playlist_id, played_at, played_ms, completed)
           VALUES (1, 1, 1, 1, 100, 8000, 1), (2, 2, NULL, NULL, 200, 3000, 0), (3, 3, NULL, 2, 300, 1, 0);
         INSERT INTO jobs (id, kind, state, priority, input_json, progress, message, result_json, created_at, started_at)
           VALUES (7, 'download', 'paused', 2, '{{}}', 0.5, '途中', '{{}}', 100, 110), (8, 'export', 'queued', 0, '{{\"ids\":[1,2]}}', NULL, NULL, NULL, 200, NULL);
         INSERT INTO remote_sources (site, remote_id, item_id, unit_id, page_index, fetched_at) VALUES ('pixiv', '123', 1, 1, 0, 5), ('youtube', 'abc', 2, NULL, NULL, 6);
         INSERT INTO capture_rules (id, name, url_pattern, mode, dest_folder_id, pos) VALUES (1, 'r', 'https://example.com/*', 'viewport', 2, 1);
         INSERT INTO settings (key, value) VALUES ('playback.support', '\"abc\"');",
        h1 = h(1),
        h2 = h(2),
        h3 = h(3)
    );
    conn.execute_batch(&sql).unwrap();
}

/// 最新の版で足した表（0009 の media_facts・analysis_results・asset_settings）にも行を入れる。`fill_v6` のあとに使う
/// （表を足したらここにも行を足す。`fixture_fills_every_table_at_the_latest_version` が見張る）。
pub(crate) fn fill_latest(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO media_facts (blob_id, state, decoder, reproducible, facts_v, alpha, colors, has_loop, loudness, analyzed_at)
           SELECT id, 'ok', 'png-rs-dec/1', 1, 1, 2, 16, 0, NULL, 1 FROM blobs;
         INSERT INTO analysis_results (blob_id, analysis, version, params, encoding, output, output_len, digest, created_at)
           SELECT id, 'image.alpha', 1, '{}', 'json', '{}', 2, x'00', 1 FROM blobs;
         INSERT INTO asset_settings (item_id, pixel_art, pixel_scale, grid_json, frame_ms, loop_start, loop_end, anim_tag, updated_at)
           VALUES (1, 1, 4, '{\"v\":1}', 100, NULL, NULL, 'idle', 1), (2, 0, NULL, NULL, NULL, NULL, NULL, NULL, 1),
                  (3, NULL, NULL, NULL, NULL, 10, 20, NULL, 1);",
    )
    .unwrap();
}

/// 名前の一覧を読む。
fn names(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// 表ごとの列（前の版の列だけで比べるのに使う）。
pub(crate) fn columns(conn: &Connection) -> BTreeMap<String, Vec<String>> {
    names(
        conn,
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .into_iter()
    .map(|t| {
        let cols = names(conn, &format!("SELECT name FROM pragma_table_info('{t}')"));
        (t, cols)
    })
    .collect()
}

/// すべての表の行（`only` にある表はその列だけ）・索引とトリガの定義・FTS の照合（rank=1）と件数。
pub(crate) fn snapshot_cols(
    conn: &Connection,
    only: &BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for t in names(
        conn,
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE 'items_fts_%' AND name <> 'items_fts'",
    ) {
        let mut cols = match only.get(&t) {
            Some(c) => c.clone(),
            None => names(conn, &format!("SELECT name FROM pragma_table_info('{t}')")),
        };
        cols.sort();
        let pk = names(
            conn,
            &format!("SELECT name FROM pragma_table_info('{t}') WHERE pk > 0 ORDER BY pk"),
        );
        let order = if pk.is_empty() {
            "rowid".to_string()
        } else {
            pk.join(", ")
        };
        let expr: Vec<String> = cols.iter().map(|c| format!("quote(\"{c}\")")).collect();
        let rows = names(
            conn,
            &format!(
                "SELECT {} FROM \"{t}\" ORDER BY {order}",
                expr.join(" || '|' || ")
            ),
        );
        out.insert(
            format!("table {t}"),
            format!("{}\n{}", cols.join(","), rows.join("\n")),
        );
    }
    let mut stmt = conn
        .prepare(
            "SELECT type || ' ' || name, tbl_name || ': ' || ifnull(sql, '(自動)')
               FROM sqlite_schema WHERE type IN ('index', 'trigger', 'view')",
        )
        .unwrap();
    let defs = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap();
    for row in defs {
        let (k, v) = row.unwrap();
        out.insert(k, v.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    let fts = conn.execute(
        "INSERT INTO items_fts(items_fts, rank) VALUES ('integrity-check', 1)",
        [],
    );
    out.insert("fts integrity".into(), format!("{:?}", fts.map(|_| ())));
    out.insert(
        "fts rows".into(),
        one::<i64>(conn, "SELECT count(*) FROM items_fts").to_string(),
    );
    out
}

/// すべての列でのスナップショット。
pub(crate) fn snapshot(conn: &Connection) -> BTreeMap<String, String> {
    snapshot_cols(conn, &BTreeMap::new())
}

/// 表の名前（sqlite_ と FTS の影の表を除く）。
pub(crate) fn tables(conn: &Connection) -> Vec<String> {
    names(
        conn,
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE 'items_fts%' ORDER BY name",
    )
}
