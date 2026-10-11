//! SQL に変換した結果が、小さなダミーのライブラリで期待どおりになることのテスト（search-query.md §7）。

use rusqlite::{Connection, params, params_from_iter};

use super::compile::{DbResolver, QueryContext, compile};
use super::parse;
use crate::db::{query_plan, test_conn};
use crate::time::days_from_civil;

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;

/// 2026-10-11 12:00（日本時間）。
fn now() -> i64 {
    days_from_civil(2026, 10, 11) * DAY + 3 * HOUR
}

fn ctx() -> QueryContext {
    QueryContext {
        now_ms: now(),
        utc_offset_minutes: 540,
    }
}

/// 日本時間のその日の正午。
fn date(y: i64, m: i64, d: i64) -> i64 {
    days_from_civil(y, m, d) * DAY + 3 * HOUR
}

#[allow(clippy::too_many_arguments)]
fn item(
    conn: &Connection,
    id: i64,
    kind: &str,
    title: &str,
    original: &str,
    rating: i64,
    duration: Option<i64>,
    imported: i64,
    captured: Option<i64>,
    size: (Option<i64>, Option<i64>),
    plays: (i64, Option<i64>),
    source: (&str, Option<&str>),
    note: Option<&str>,
) {
    conn.execute(
        "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, duration_ms, title, original_name,
                            source_url, source_site, rating, note, captured_at, imported_at, updated_at,
                            play_count, last_played_at)
         VALUES (?1, ?2, 'x', ?3, 1, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14, ?15, ?16)",
        params![
            id,
            vec![id as u8; 32],
            kind,
            size.0,
            size.1,
            duration,
            title,
            original,
            source.1,
            source.0,
            rating,
            note,
            captured,
            imported,
            plays.0,
            plays.1
        ],
    )
    .unwrap();
}

fn library() -> Connection {
    let conn = test_conn();
    let n = now();
    let none = (0, None);
    item(
        &conn,
        1,
        "image",
        "夕方の海",
        "IMG_0001.jpg",
        5,
        None,
        n - HOUR,
        Some(date(2024, 5, 1)),
        (Some(4000), Some(3000)),
        none,
        ("local", None),
        Some("構図がよい"),
    );
    item(
        &conn,
        2,
        "image",
        "駅前 夜景",
        "IMG_0002.heic",
        4,
        None,
        n - 2 * DAY,
        Some(date(2025, 12, 31)),
        (Some(3000), Some(4000)),
        (2, Some(n - 40 * DAY)),
        ("pixiv", Some("https://www.pixiv.net/artworks/1")),
        None,
    );
    item(
        &conn,
        3,
        "video",
        "ライブ",
        "live.mov",
        3,
        Some(240_000),
        n - 10 * DAY,
        None,
        (Some(1920), Some(1080)),
        (1, Some(n - DAY)),
        ("youtube", Some("https://youtu.be/x")),
        Some("ラフ"),
    );
    item(
        &conn,
        4,
        "video",
        "古い動画",
        "old.mkv",
        0,
        Some(60_000),
        n - 40 * DAY,
        Some(date(2020, 1, 1)),
        (Some(640), Some(480)),
        none,
        ("local", None),
        None,
    );
    item(
        &conn,
        5,
        "audio",
        "song",
        "song.flac",
        2,
        Some(180_000),
        n - DAY,
        None,
        (None, None),
        (5, Some(n - 2 * HOUR)),
        ("local", None),
        None,
    );
    item(
        &conn,
        6,
        "image",
        "正方形",
        "sq.png",
        1,
        None,
        n - 100 * DAY,
        None,
        (Some(1000), Some(1000)),
        none,
        ("capture", None),
        Some("駅前の資料"),
    );
    item(
        &conn,
        7,
        "image",
        "ラフ画",
        "rough.png",
        0,
        None,
        n - 3 * DAY,
        None,
        (Some(800), Some(600)),
        none,
        ("local", None),
        None,
    );
    item(
        &conn,
        8,
        "image",
        "捨てた",
        "trash.png",
        5,
        None,
        n - HOUR,
        None,
        (Some(10), Some(10)),
        none,
        ("local", None),
        None,
    );
    conn.execute_batch(
        "UPDATE items SET trashed_at = 1 WHERE id = 8;
         INSERT INTO tags (id, namespace, name, created_at) VALUES
           (1, '', '夕景', 0), (2, '', '海', 0), (3, '', '港', 0), (4, '作者', 'もりの', 0), (5, '', '未整理', 0), (6, '', '空', 0);
         INSERT INTO tag_aliases (alias, tag_id) VALUES ('夕焼け', 1);
         INSERT INTO tag_parents (child_id, parent_id) VALUES (1, 6);
         INSERT INTO item_tags (tag_id, item_id) VALUES
           (1, 1), (1, 2), (2, 1), (2, 3), (3, 4), (4, 1), (4, 5), (5, 7), (6, 6), (1, 8);
         INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (1, NULL, '写真', 1, 0), (2, 1, '旅行', 1, 0);
         INSERT INTO item_folders (folder_id, item_id) VALUES (1, 6), (2, 1), (2, 7);
         INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES
           (1, 'set', '鎌倉', 'manual', 0, 0), (2, 'folder', '写真', 'name', 0, 0);
         INSERT INTO unit_items (unit_id, item_id, pos) VALUES (1, 2, 1), (1, 3, 2);
         INSERT INTO unit_folders (unit_id, folder_id, recursive, pos) VALUES (2, 1, 1, 1);",
    )
    .unwrap();
    conn
}

fn ids(conn: &Connection, query: &str) -> Vec<i64> {
    let parsed = parse(query).unwrap_or_else(|e| panic!("{query}: {e:?}"));
    let cond = compile(&parsed.expr, &DbResolver { conn }, &ctx()).unwrap();
    let sql = format!(
        "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND ({}) ORDER BY i.id",
        cond.sql
    );
    let mut stmt = conn
        .prepare(&sql)
        .unwrap_or_else(|e| panic!("{query}: {sql}: {e}"));
    stmt.query_map(params_from_iter(cond.params.iter()), |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

#[test]
fn spec_examples_find_the_expected_items() {
    let conn = library();
    let cases: &[(&str, &[i64])] = &[
        ("夕景", &[1, 2]),
        ("夕景 海", &[1]),
        ("海 | 港", &[1, 3, 4]),
        ("-未整理", &[1, 2, 3, 4, 5, 6]),
        ("作者:もりの", &[1, 5]),
        ("種類:動画", &[3, 4]),
        ("種類:画像,動画", &[1, 2, 3, 4, 6, 7]),
        ("評価>=4", &[1, 2]),
        ("長さ>3分", &[3]),
        ("追加:7日以内", &[1, 2, 5, 7]),
        ("再生:未", &[1, 4, 6, 7]),
        ("フォルダ:\"写真/旅行\"", &[1, 7]),
        ("ユニット:なし", &[4, 5]),
        ("\"駅前 夜\"", &[2]),
        ("(海 | 港) 評価>=3 -ラフ", &[1]),
        ("", &[1, 2, 3, 4, 5, 6, 7]),
    ];
    for (query, expected) in cases {
        assert_eq!(ids(&conn, query), *expected, "{query}");
    }
}

#[test]
fn tags_resolve_aliases_parents_and_namespaces() {
    let conn = library();
    assert_eq!(ids(&conn, "夕焼け"), vec![1, 2]);
    // 親タグは子孫のタグの付いたものも含む
    assert_eq!(ids(&conn, "空"), vec![1, 2, 6]);
    assert_eq!(ids(&conn, "作者:いない"), Vec::<i64>::new());
    // タグが 3 つの AND（INTERSECT でまとめる）
    assert_eq!(ids(&conn, "夕景 海 作者:もりの"), vec![1]);
    assert_eq!(ids(&conn, "種類:画像 評価>=4 夕景"), vec![1, 2]);
    assert_eq!(ids(&conn, "-(夕景 | 港)"), vec![3, 5, 6, 7]);
}

#[test]
fn words_without_a_tag_search_the_text() {
    let conn = library();
    // 2 文字以下は LIKE（タイトル・メモ）
    assert_eq!(ids(&conn, "駅前"), vec![2, 6]);
    // 3 文字以上は FTS5 trigram（元ファイル名）
    assert_eq!(ids(&conn, "IMG_0001"), vec![1]);
    assert_eq!(ids(&conn, "img_000"), vec![1, 2]);
    assert_eq!(ids(&conn, "youtu.be"), vec![3]);
    // % や _ はそのままの文字として探す
    assert_eq!(ids(&conn, "\"_0\""), vec![1, 2]);
    assert_eq!(ids(&conn, "%"), Vec::<i64>::new());
}

#[test]
fn fields_compare_values() {
    let conn = library();
    let cases: &[(&str, &[i64])] = &[
        ("種類!=画像", &[3, 4, 5]),
        ("種類=動画", &[3, 4]),
        ("評価:0", &[4, 7]),
        ("長さ>=3分", &[3, 5]),
        ("長さ<2分", &[4]),
        ("追加:今日", &[1]),
        ("追加:昨日", &[5]),
        ("追加:30日以上前", &[4, 6]),
        ("撮影:2024", &[1]),
        ("撮影<2025", &[1, 4]),
        ("撮影>2024", &[2]),
        ("撮影!=2024", &[2, 3, 4, 5, 6, 7]),
        ("撮影:2025-12-31..2026-01-05", &[2]),
        ("再生:30日以上前", &[2]),
        ("再生!=未", &[2, 3, 5]),
        ("再生回数:0", &[1, 4, 6, 7]),
        ("再生回数>=2", &[2, 5]),
        ("フォルダ:写真", &[1, 6, 7]),
        ("フォルダ=写真", &[6]),
        ("フォルダ:ない", &[]),
        ("-フォルダ:写真", &[2, 3, 4, 5]),
        ("ユニット:鎌倉", &[2, 3]),
        ("ユニット:写真", &[1, 6, 7]),
        ("ユニット:あり", &[1, 2, 3, 6, 7]),
        ("ユニット!=鎌倉", &[1, 4, 5, 6, 7]),
        ("名前:IMG_", &[1, 2]),
        ("名前=SONG", &[5]),
        ("url:pixiv.net", &[2]),
        ("メモ:構図", &[1]),
        ("メモ!=構図", &[2, 3, 4, 5, 6, 7]),
        ("取得元:pixiv", &[2]),
        ("取得元!=local", &[2, 3, 6]),
        ("タグ数>=2", &[1]),
        ("タグ数:1", &[2, 3, 4, 5, 6, 7]),
        ("幅>=3000", &[1, 2]),
        ("向き:縦", &[2]),
        ("向き:正方形", &[6]),
        ("向き:横", &[1, 3, 4, 7]),
    ];
    for (query, expected) in cases {
        assert_eq!(ids(&conn, query), *expected, "{query}");
    }
}

#[test]
fn time_zone_moves_day_boundaries() {
    let conn = library();
    // 日本時間では「今日」の 1 件（11:00）。UTC では 10 月 11 日の 02:00 なので同じく今日。
    // 時差 -12 時間なら、今は 10 月 10 日 15:00、1 は 14:00 なので今日。
    let parsed = parse("追加:今日").unwrap();
    let cond = compile(
        &parsed.expr,
        &DbResolver { conn: &conn },
        &QueryContext {
            now_ms: now(),
            utc_offset_minutes: -720,
        },
    )
    .unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND ({}) ORDER BY i.id",
            cond.sql
        ))
        .unwrap();
    let found: Vec<i64> = stmt
        .query_map(params_from_iter(cond.params.iter()), |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(found, vec![1]);
}

#[test]
fn tag_intersection_uses_the_primary_key() {
    let conn = library();
    let parsed = parse("夕景 海 作者:もりの").unwrap();
    let cond = compile(&parsed.expr, &DbResolver { conn: &conn }, &ctx()).unwrap();
    assert!(cond.sql.contains("INTERSECT"), "{}", cond.sql);
    // EXPLAIN 用に値を埋め込む（テストだけ）
    let mut sql = format!("SELECT i.id FROM items i WHERE {}", cond.sql);
    for p in &cond.params {
        if let rusqlite::types::Value::Integer(v) = p {
            sql = sql.replacen('?', &v.to_string(), 1);
        }
    }
    let plan = query_plan(&conn, &sql);
    assert!(!plan.contains("SCAN item_tags"), "{plan}");
}

#[test]
fn quoted_words_resolve_tags_before_searching_the_text() {
    let conn = library();
    // 空白や「:」を含む、名前空間なしのタグ（pixiv の「Re:ゼロ…」など）
    conn.execute_batch(
        "INSERT INTO tags (id, namespace, name, created_at) VALUES (7, '', 'Re:ゼロ', 0), (8, '', '夕方の 海', 0);
         INSERT INTO tag_aliases (alias, tag_id) VALUES ('リゼロ 外伝', 7);
         INSERT INTO item_tags (tag_id, item_id) VALUES (7, 3), (8, 4);",
    )
    .unwrap();
    // 引用符の語も、別名 → 正式タグ → 全文検索の順（search-query.md §2）
    assert_eq!(ids(&conn, "\"Re:ゼロ\""), vec![3]);
    assert_eq!(ids(&conn, "\"夕方の 海\""), vec![4]);
    assert_eq!(ids(&conn, "\"リゼロ 外伝\""), vec![3]);
    assert_eq!(ids(&conn, "-\"Re:ゼロ\" 種類:動画"), vec![4]);
    // 同名のタグがなければ今までどおり全文検索
    assert_eq!(ids(&conn, "\"駅前 夜\""), vec![2]);
    // タグの一覧から入れる条件は、そのタグだけに一致する
    for (ns, name, expected) in [
        ("", "Re:ゼロ", vec![3]),
        ("", "夕方の 海", vec![4]),
        ("作者", "もりの", vec![1, 5]),
    ] {
        let clause = super::suggest::tag_clause(ns, name);
        assert_eq!(ids(&conn, &clause), expected, "{clause}");
    }
    // AND の中でもタグとしてまとめて絞る
    assert_eq!(ids(&conn, "\"Re:ゼロ\" 海"), vec![3]);
}
