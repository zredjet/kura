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

// ---- 素材の事実のフィールド（M11。search-query.md §3・§4） ----

/// 素材の 1 件：種類・幅・高さ・解析の進み・事実（`media_facts` の列と値。None なら事実の行なし）。
struct Asset {
    id: i64,
    kind: &'static str,
    size: (Option<i64>, Option<i64>),
    analysis: &'static str,
    facts: Option<&'static str>,
}

/// 事実の列（`facts` はこの順の値を書いた SQL の式の並び）。
const FACT_COLUMNS: &str = "alpha, colors, pixel_art, frame_count, \
     sample_rate, bits, channels, loudness, true_peak, has_loop";

/// 画像・音声・動画・フォントを混ぜた小さなライブラリ。解析済み・未解析・失敗（計れない）・設定ありを含む。
fn asset_library() -> Connection {
    let conn = test_conn();
    fill_assets(&conn);
    conn
}

/// [`asset_library`] の行を入れる（ファイルの DB でも使う）。
fn fill_assets(conn: &Connection) {
    let n = now();
    let assets = [
        // 64×64 の二値の透過・16 色・自動でドット絵・1 コマ
        Asset {
            id: 11,
            kind: "image",
            size: (Some(64), Some(64)),
            analysis: "ready",
            facts: Some("1, 16, 1, 1, NULL, NULL, NULL, NULL, NULL, NULL"),
        },
        // 写真の JPEG：透過なし・色数は計らない・ドット絵でない（形式から）
        Asset {
            id: 12,
            kind: "image",
            size: (Some(4000), Some(3000)),
            analysis: "ready",
            facts: Some("0, NULL, 0, 1, NULL, NULL, NULL, NULL, NULL, NULL"),
        },
        // 半透明・200 色・8 コマのアニメ。自動ではドット絵でないが、設定で「はい」
        Asset {
            id: 13,
            kind: "image",
            size: (Some(128), Some(128)),
            analysis: "ready",
            facts: Some("2, 200, 0, 8, NULL, NULL, NULL, NULL, NULL, NULL"),
        },
        // 二値の透過・8 色。自動ではドット絵だが、設定で「いいえ」
        Asset {
            id: 14,
            kind: "image",
            size: (Some(32), Some(32)),
            analysis: "ready",
            facts: Some("1, 8, 1, 1, NULL, NULL, NULL, NULL, NULL, NULL"),
        },
        // まだ解析していない（幅・高さは 2 のべき乗）
        Asset {
            id: 15,
            kind: "image",
            size: (Some(256), Some(128)),
            analysis: "pending",
            facts: None,
        },
        // 解析に失敗（事実の行はあるが、値はすべて計れない）
        Asset {
            id: 16,
            kind: "image",
            size: (Some(100), Some(100)),
            analysis: "failed",
            facts: Some("NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL"),
        },
        // まだ解析していないが、設定で「ドット絵：はい」
        Asset {
            id: 17,
            kind: "image",
            size: (Some(512), Some(512)),
            analysis: "pending",
            facts: None,
        },
        // ゴミ箱の中（数えない）
        Asset {
            id: 18,
            kind: "image",
            size: (Some(16), Some(16)),
            analysis: "ready",
            facts: Some("1, 4, 1, 1, NULL, NULL, NULL, NULL, NULL, NULL"),
        },
        Asset {
            id: 19,
            kind: "image",
            size: (Some(16), Some(16)),
            analysis: "pending",
            facts: None,
        },
        // 48kHz・24 ビット・ステレオ・−14.2 LUFS・−1.0 dBTP・ループあり
        Asset {
            id: 21,
            kind: "audio",
            size: (None, None),
            analysis: "ready",
            facts: Some("NULL, NULL, NULL, NULL, 48000, 24, 2, -14.2, -1.0, 1"),
        },
        // 44.1kHz・非可逆（ビットは計れない）・モノラル・−20 LUFS・−0.3 dBTP・ループなし
        Asset {
            id: 22,
            kind: "audio",
            size: (None, None),
            analysis: "ready",
            facts: Some("NULL, NULL, NULL, NULL, 44100, NULL, 1, -20.0, -0.3, 0"),
        },
        // まだ解析していない音声
        Asset {
            id: 23,
            kind: "audio",
            size: (None, None),
            analysis: "pending",
            facts: None,
        },
        // 短くてラウドネスを計れない（16 ビット・ステレオ・−6 dBTP・ループなし）
        Asset {
            id: 24,
            kind: "audio",
            size: (None, None),
            analysis: "ready",
            facts: Some("NULL, NULL, NULL, NULL, 44100, 16, 2, NULL, -6.0, 0"),
        },
        Asset {
            id: 31,
            kind: "video",
            size: (Some(1024), Some(512)),
            analysis: "not_needed",
            facts: None,
        },
        Asset {
            id: 41,
            kind: "font",
            size: (None, None),
            analysis: "not_needed",
            facts: None,
        },
    ];
    for a in &assets {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, title, original_name,
                                imported_at, updated_at, analysis_state)
             VALUES (?1, ?2, 'x', ?3, 1, ?4, ?5, ?6, ?6, ?7, ?7, ?8)",
            params![
                a.id,
                vec![a.id as u8; 32],
                a.kind,
                a.size.0,
                a.size.1,
                format!("asset{}", a.id),
                n - a.id * HOUR,
                a.analysis
            ],
        )
        .unwrap();
        if let Some(values) = a.facts {
            let state = if a.analysis == "failed" {
                "failed"
            } else {
                "ok"
            };
            conn.execute(
                &format!(
                    "INSERT INTO media_facts (blob_id, state, facts_v, {FACT_COLUMNS}, analyzed_at)
                     SELECT b.id, ?2, 1, {values}, 0 FROM blobs b WHERE b.hash = ?1"
                ),
                params![vec![a.id as u8; 32], state],
            )
            .unwrap();
        }
    }
    conn.execute_batch(
        "UPDATE items SET trashed_at = 1 WHERE id IN (18, 19);
         INSERT INTO asset_settings (item_id, pixel_art, updated_at) VALUES (13, 1, 0), (14, 0, 0), (17, 1, 0);",
    )
    .unwrap();
}

/// パレットの件数と「未解析 N 件」を読み取りの接続 2 つで同時に数えても（`check_db`）、1 つの接続で順に数えたとき
/// （`check`）と同じ結果になる。
#[test]
fn check_db_matches_check() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::db::Db::open(&dir.path().join("library.sqlite")).unwrap();
    db.write(|tx| {
        fill_assets(tx);
        Ok(())
    })
    .unwrap();
    for text in [
        "透過:あり",
        "ドット絵:なし",
        "-アニメ:あり",
        "種類:画像 評価>=3",
        "透過:",
        "",
    ] {
        for view in [crate::items::ItemView::All, crate::items::ItemView::Trash] {
            let one = db
                .read(|conn| super::check(conn, text, view, &ctx()))
                .unwrap();
            let both = super::check_db(&db, text, view, &ctx()).unwrap();
            assert_eq!(both, one, "{text}");
        }
    }
}

#[test]
fn image_facts_match_only_known_values() {
    let conn = asset_library();
    let cases: &[(&str, &[i64])] = &[
        ("透過:あり", &[11, 13, 14]),
        ("透過:なし", &[12]),
        ("透過:半透明", &[13]),
        ("透過:二値", &[11, 14]),
        // `!=` は「わかっていて違う」もの。未解析（15・17）・計れない（16）・音声などは入らない
        ("透過!=あり", &[12]),
        ("透過!=なし", &[11, 13, 14]),
        // 除外だけが、未解析・計れないものを含む
        ("-透過:あり", &[12, 15, 16, 17, 21, 22, 23, 24, 31, 41]),
        // ドット絵は「設定 ?? 自動」。設定があれば未解析（17）でも決まる
        ("ドット絵:あり", &[11, 13, 17]),
        ("ドット絵:なし", &[12, 14]),
        ("ドット絵!=あり", &[12, 14]),
        ("-ドット絵:あり", &[12, 14, 15, 16, 21, 22, 23, 24, 31, 41]),
        ("色数<=16", &[11, 14]),
        ("色数>16", &[13]),
        ("色数:8", &[14]),
        // JPEG（12）の色数は計っていないので、`!=` にも入らない
        ("色数!=16", &[13, 14]),
        ("アニメ:あり", &[13]),
        ("アニメ:なし", &[11, 12, 14]),
        ("アニメ!=なし", &[13]),
        // 2 のべき乗は幅・高さから（解析は要らない）。画像だけ
        ("2のべき乗:あり", &[11, 13, 14, 15, 17]),
        ("2のべき乗:なし", &[12, 16]),
        ("2のべき乗!=あり", &[12, 16]),
        ("種類:画像 透過:あり 色数<=16", &[11, 14]),
    ];
    for (query, expected) in cases {
        assert_eq!(ids(&conn, query), *expected, "{query}");
    }
}

/// 色数の事実は 257 で打ち切る（257 は「256 より多い」で、数は計っていない）。256 を超える値との比較は、256 より多いどの数でも
/// 結果が同じになるもの（`>`・`!=` は 256 以下の値、`>=` は 257 以下の値）だけに 257 の行を入れ、ほかは計れない値と同じに
/// どれにも入れない（除外は含む。search-query.md §3・§4。Q2）。
#[test]
fn colors_above_the_cap_match_only_comparisons_they_decide() {
    let conn = asset_library();
    // 数万色の写真の PNG（色数は打ち切りの 257）
    conn.execute_batch(&format!(
        "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, title, original_name,
                            imported_at, updated_at, analysis_state)
         VALUES (51, x'{hash}', 'png', 'image', 1, 3000, 2000, 'photo', 'photo', 1, 1, 'ready');
         INSERT INTO media_facts (blob_id, state, facts_v, {FACT_COLUMNS}, analyzed_at)
           SELECT id, 'ok', 1, 0, 257, 0, 1, NULL, NULL, NULL, NULL, NULL, NULL, 0 FROM blobs WHERE hash = x'{hash}';",
        hash = "33".repeat(32)
    ))
    .unwrap();
    let cases: &[(&str, &[i64])] = &[
        // 結果が決まる比較
        ("色数>256", &[51]),
        ("色数>=257", &[51]),
        ("色数>16", &[13, 51]),
        ("色数!=16", &[13, 14, 51]),
        ("色数>=200", &[13, 51]),
        // 256 より多い値との比較・一致は、257 の行では決まらないので入れない（12 の JPEG は計っていない）
        ("色数<1000", &[11, 13, 14]),
        ("色数<=257", &[11, 13, 14]),
        ("色数<=256", &[11, 13, 14]),
        ("色数:257", &[]),
        ("色数=300", &[]),
        ("色数!=300", &[11, 13, 14]),
        ("色数>=300", &[]),
        ("色数>1000", &[]),
        // 除外は補集合なので、決まらない行も含む
        ("種類:画像 -色数<1000", &[12, 15, 16, 17, 51]),
    ];
    for (query, expected) in cases {
        assert_eq!(ids(&conn, query), *expected, "{query}");
    }
}

#[test]
fn audio_facts_match_only_known_values() {
    let conn = asset_library();
    let cases: &[(&str, &[i64])] = &[
        ("レート:44.1k", &[22, 24]),
        ("レート>=48kHz", &[21]),
        ("レート!=44100", &[21]),
        ("ビット:24", &[21]),
        // 非可逆（22）はビットを計れないので `!=` にも入らない
        ("ビット!=24", &[24]),
        ("チャンネル:ステレオ", &[21, 24]),
        ("チャンネル:モノラル", &[22]),
        ("チャンネル>=1", &[21, 22, 24]),
        ("ラウドネス>-16", &[21]),
        ("ラウドネス<-14", &[21, 22]),
        ("ラウドネス<=−14.2", &[21, 22]),
        ("ラウドネス<-14.2", &[22]),
        // 計れない（24）・未解析（23）はどの比較にも入らない
        ("ラウドネス<0", &[21, 22]),
        ("ピーク>-1", &[22]),
        ("ピーク>=-1", &[21, 22]),
        ("ピーク<-3", &[24]),
        ("ループ:あり", &[21]),
        ("ループ:なし", &[22, 24]),
        ("ループ!=あり", &[22, 24]),
        ("種類:音楽 -ループ:あり", &[22, 23, 24]),
        ("ループ:あり | 透過:半透明", &[13, 21]),
    ];
    for (query, expected) in cases {
        assert_eq!(ids(&conn, query), *expected, "{query}");
    }
}

/// ドット絵の検索（この SQL）と素材の欄の実効値（`asset::effective_pixel_art`）が同じ規則であること。設定は素材の欄の
/// 書き手（`asset::set_settings`）、自動の判定は解析の書き手（`analysis::store::write`）で入れ、設定 3 通り × 自動
/// 4 通り（未解析・解析済みで材料が足りない・いいえ・はい）のすべてで照らす。
/// - `ドット絵:あり` は実効値が真のもの、`-ドット絵:あり` はその補集合（画像のほかの種類も入る）。
/// - `ドット絵:なし`・`ドット絵!=あり` は実効値が偽で、設定か自動の判定があるもの（どちらもなければ決まっていない
///   ので入れない。search-query.md §4。素材の欄は表示のために偽とする）。
/// - 「未解析 N 件」は、解析待ちで設定のないもの（設定があれば解析の前でも決まる）。
#[test]
fn pixel_art_search_follows_the_effective_value_of_the_asset_panel() {
    use crate::analysis::facts::{FACTS_V, MediaFacts, PIXEL_RULE};
    use crate::analysis::store::{ItemWrite, Write, write};
    use crate::asset::{Change, SettingsPatch, TRISTATE, effective_pixel_art, set_settings};

    // 自動の判定：None は未解析（pending）、Some(None) は解析済みで材料が足りない（NULL）
    let autos: [Option<Option<bool>>; 4] = [None, Some(None), Some(Some(false)), Some(Some(true))];
    let conn = test_conn();
    let mut cases = Vec::new();
    let mut id = 0_i64;
    for setting in TRISTATE {
        for auto in autos {
            id += 1;
            let hash = [u8::try_from(id).unwrap(); 32];
            conn.execute(
                "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, title, original_name,
                                    imported_at, updated_at, analysis_state)
                 VALUES (?1, ?2, 'png', 'image', 1, 64, 64, 't', 't', 0, 0, 'pending')",
                params![id, hash.as_slice()],
            )
            .unwrap();
            if let Some(auto) = auto {
                let facts = MediaFacts {
                    facts_v: FACTS_V,
                    alpha: Some(1),
                    pixel_art: auto,
                    pixel_rule: Some(PIXEL_RULE),
                    frame_count: Some(1),
                    ..MediaFacts::default()
                };
                let ready = ItemWrite {
                    id,
                    hash,
                    write: Write::Ready {
                        results: Vec::new(),
                        keep: None,
                        facts: Box::new(facts),
                        stamp: None,
                    },
                };
                assert_eq!(write(&conn, &[ready], 0).unwrap().written, 1);
            }
            if let Some(value) = setting {
                let patch = SettingsPatch {
                    pixel_art: Some(Change::Set(value)),
                    ..SettingsPatch::default()
                };
                assert_eq!(set_settings(&conn, &[id], &patch, 0).unwrap(), 1);
            }
            cases.push((id, setting, auto));
        }
    }
    // 画像でないもの（除外にだけ入る）
    conn.execute(
        "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at,
                            analysis_state)
         VALUES (99, ?1, 'ogg', 'audio', 1, 't', 't', 0, 0, 'pending')",
        params![[99_u8; 32].as_slice()],
    )
    .unwrap();

    let effective = |s: Option<bool>, a: Option<Option<bool>>| effective_pixel_art(s, a.flatten());
    let pick = |want: &dyn Fn(Option<bool>, Option<Option<bool>>) -> bool| -> Vec<i64> {
        cases
            .iter()
            .filter(|(_, s, a)| want(*s, *a))
            .map(|(id, _, _)| *id)
            .collect()
    };
    let yes = pick(&|s, a| effective(s, a));
    let no_known = pick(&|s, a| !effective(s, a) && (s.is_some() || a.flatten().is_some()));
    let mut not_yes = pick(&|s, a| !effective(s, a));
    not_yes.push(99);
    assert_eq!(ids(&conn, "ドット絵:あり"), yes);
    assert_eq!(ids(&conn, "ドット絵!=なし"), yes);
    assert_eq!(ids(&conn, "ドット絵:なし"), no_known);
    assert_eq!(ids(&conn, "ドット絵!=あり"), no_known);
    assert_eq!(ids(&conn, "-ドット絵:あり"), not_yes);
    // 素材の欄の実効値も同じ関数の値
    for (id, setting, auto) in &cases {
        let info = crate::asset::get(&conn, *id).unwrap();
        assert_eq!(
            info.effective.pixel_art,
            effective(*setting, *auto),
            "{id}: {setting:?} {auto:?}"
        );
    }
    let waiting = pick(&|s, a| a.is_none() && s.is_none()).len() as u64;
    assert_eq!(unanalyzed(&conn, "ドット絵:あり"), Some(waiting));
    assert_eq!(unanalyzed(&conn, "ドット絵:なし"), Some(waiting));
}

/// 格子の設定の状態（アニメの検索のテスト）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridCase {
    /// 格子を設定していない。
    None,
    /// 格子を設定した（素材の欄の書き手 `asset::set_settings` で）。
    Set,
    /// 知らない版の grid_json（素材の欄は格子なしとして読む）。
    UnknownVersion,
    /// 壊れた grid_json（同じく格子なし）。
    Broken,
}

/// アニメの検索（この SQL）と、素材の欄・一覧のタイルの実効値（`asset::effective_animated`）が同じ規則「格子の設定 ?? 解析の
/// コマ」であること（M11 のユーザーの決定 16）。格子は素材の欄の書き手（`asset::set_settings`）、コマの数は解析の書き手
/// （`analysis::store::write`）で入れ、格子 4 通り（なし・あり・知らない版・壊れた JSON）× コマ 4 通り（未解析・解析済みで
/// 計れない・1 コマ・8 コマ）のすべてで照らす。
/// - `アニメ:あり`・`アニメ!=なし` は実効値が真のもの（格子があれば、未解析でも 1 コマでも）。`-アニメ:あり` はその補集合
///   （画像のほかの種類も入る）。
/// - `アニメ:なし`・`アニメ!=あり` は実効値が偽で、決まっているもの（格子がなく、コマが 1 つと分かっている）。未解析・計れない
///   ものは入れない（search-query.md §4。素材の欄とタイルは表示のために偽とする）。
/// - 知らない版・壊れた格子は、素材の欄と同じく格子なしとして読む。
/// - 「未解析 N 件」は、解析待ちで格子のないもの（格子があれば解析の前でも決まる）。
#[test]
fn anim_search_follows_the_effective_value_of_the_asset_panel() {
    use crate::analysis::facts::{FACTS_V, MediaFacts};
    use crate::analysis::store::{ItemWrite, Write, write};
    use crate::asset::{Change, GridSetting, SettingsPatch, effective_animated, set_settings};

    // コマの数：None は未解析（pending）、Some(None) は解析済みで計れない（NULL）
    let frames: [Option<Option<i64>>; 4] = [None, Some(None), Some(Some(1)), Some(Some(8))];
    let grids = [
        GridCase::None,
        GridCase::Set,
        GridCase::UnknownVersion,
        GridCase::Broken,
    ];
    let conn = test_conn();
    let mut cases = Vec::new();
    let mut id = 0_i64;
    for grid in grids {
        for frame in frames {
            id += 1;
            let hash = [u8::try_from(id).unwrap(); 32];
            conn.execute(
                "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, title, original_name,
                                    imported_at, updated_at, analysis_state)
                 VALUES (?1, ?2, 'png', 'image', 1, 64, 32, 't', 't', 0, 0, 'pending')",
                params![id, hash.as_slice()],
            )
            .unwrap();
            if let Some(count) = frame {
                let facts = MediaFacts {
                    facts_v: FACTS_V,
                    alpha: Some(0),
                    frame_count: count,
                    ..MediaFacts::default()
                };
                let ready = ItemWrite {
                    id,
                    hash,
                    write: Write::Ready {
                        results: Vec::new(),
                        keep: None,
                        facts: Box::new(facts),
                        stamp: None,
                    },
                };
                assert_eq!(write(&conn, &[ready], 0).unwrap().written, 1);
            }
            let raw = |json: &str| {
                conn.execute(
                    "INSERT INTO asset_settings (item_id, grid_json, updated_at) VALUES (?1, ?2, 0)",
                    params![id, json],
                )
                .unwrap();
            };
            match grid {
                GridCase::None => {}
                GridCase::Set => {
                    let patch = SettingsPatch {
                        grid: Some(Change::Set(GridSetting {
                            cell_w: 16,
                            cell_h: 16,
                            offset_x: 0,
                            offset_y: 0,
                            gap_x: 0,
                            gap_y: 0,
                            count: Some(6),
                        })),
                        ..SettingsPatch::default()
                    };
                    assert_eq!(set_settings(&conn, &[id], &patch, 0).unwrap(), 1);
                }
                GridCase::UnknownVersion => raw(r#"{"v":2,"cellW":16,"cellH":16}"#),
                GridCase::Broken => raw(r#"{"v":1,"cellW":"#),
            }
            cases.push((id, grid, frame));
        }
    }
    // 画像でないもの（除外にだけ入る）
    conn.execute(
        "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at,
                            analysis_state)
         VALUES (99, ?1, 'ogg', 'audio', 1, 't', 't', 0, 0, 'pending')",
        params![[99_u8; 32].as_slice()],
    )
    .unwrap();

    let effective = |grid: GridCase, frame: Option<Option<i64>>| {
        effective_animated(
            frame.flatten().map(|n| u32::try_from(n).unwrap()),
            grid == GridCase::Set,
        )
    };
    let pick = |want: &dyn Fn(GridCase, Option<Option<i64>>) -> bool| -> Vec<i64> {
        cases
            .iter()
            .filter(|(_, g, f)| want(*g, *f))
            .map(|(id, _, _)| *id)
            .collect()
    };
    let yes = pick(&|g, f| effective(g, f));
    let no_known = pick(&|g, f| !effective(g, f) && f.flatten().is_some());
    let mut not_yes = pick(&|g, f| !effective(g, f));
    not_yes.push(99);
    // 格子を設定した 4 件は、未解析・計れない・1 コマでもアニメ
    assert_eq!(yes, vec![4, 5, 6, 7, 8, 12, 16]);
    assert_eq!(ids(&conn, "アニメ:あり"), yes);
    assert_eq!(ids(&conn, "アニメ!=なし"), yes);
    assert_eq!(ids(&conn, "アニメ:なし"), no_known);
    assert_eq!(ids(&conn, "アニメ!=あり"), no_known);
    assert_eq!(ids(&conn, "-アニメ:あり"), not_yes);
    // 素材の欄と一覧の概要（タイルの印）も同じ関数の値
    let all: Vec<i64> = cases.iter().map(|c| c.0).collect();
    let rows = crate::items::summaries(&conn, &all).unwrap();
    for (row, (id, grid, frame)) in rows.iter().zip(&cases) {
        let want = effective(*grid, *frame);
        let info = crate::asset::get(&conn, *id).unwrap();
        assert_eq!(info.effective.animated, want, "{id}: {grid:?} {frame:?}");
        assert_eq!((row.id, row.animated), (*id, want), "{grid:?} {frame:?}");
    }
    let waiting = pick(&|g, f| f.is_none() && g != GridCase::Set).len() as u64;
    assert_eq!(waiting, 3);
    assert_eq!(unanalyzed(&conn, "アニメ:あり"), Some(waiting));
    assert_eq!(unanalyzed(&conn, "アニメ:なし"), Some(waiting));
    assert_eq!(unanalyzed(&conn, "-アニメ:あり"), Some(waiting));
}

/// 読まずに形式から書く事実（JPEG とアルファのない HEIC。決定 5・15）は、透過・ドット絵・アニメの「なし」に入り、色数の
/// 検索には入らない。
#[test]
fn facts_written_from_the_format_answer_the_none_side() {
    use crate::analysis::facts::from_format;
    use crate::analysis::store::{ItemWrite, Write, write};

    let conn = test_conn();
    for (id, ext) in [(1_i64, "jpg"), (2, "heic")] {
        let hash = [u8::try_from(id).unwrap(); 32];
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, title, original_name,
                                imported_at, updated_at, analysis_state)
             VALUES (?1, ?2, ?3, 'image', 1, 4032, 3024, 't', 't', 0, 0, 'pending')",
            params![id, hash.as_slice(), ext],
        )
        .unwrap();
        let format = if ext == "jpg" { "jpeg" } else { "heic" };
        let ready = ItemWrite {
            id,
            hash,
            write: Write::Ready {
                results: Vec::new(),
                keep: None,
                facts: Box::new(from_format(format)),
                stamp: None,
            },
        };
        assert_eq!(write(&conn, &[ready], 0).unwrap().written, 1);
    }
    assert_eq!(ids(&conn, "アニメ:なし"), vec![1, 2]);
    assert_eq!(ids(&conn, "透過:なし"), vec![1, 2]);
    assert_eq!(ids(&conn, "ドット絵:なし"), vec![1, 2]);
    assert!(ids(&conn, "アニメ:あり").is_empty());
    assert!(ids(&conn, "色数<=256").is_empty(), "JPEG の色数は計らない");
    assert!(ids(&conn, "色数>256").is_empty(), "JPEG の色数は計らない");
}

/// パレットの「未解析 N 件」：解析がまだで、解析が終わると一致が変わりうるもの。
fn unanalyzed(conn: &Connection, query: &str) -> Option<u64> {
    let check = super::check(conn, query, crate::items::ItemView::All, &ctx()).unwrap();
    assert!(check.error.is_none(), "{query}: {:?}", check.error);
    check.unanalyzed
}

#[test]
fn unanalyzed_counts_items_whose_match_awaits_analysis() {
    let conn = asset_library();
    let cases: &[(&str, Option<u64>)] = &[
        // 解析で決まるフィールドを使わなければ数えない
        ("", None),
        ("評価>=4", None),
        ("2のべき乗:あり", None),
        ("種類:画像", None),
        // 未解析の画像は 15・17（ゴミ箱の 19 は数えない。失敗した 16 は計れないと決まっている）
        ("透過:あり", Some(2)),
        ("透過!=あり", Some(2)),
        // 除外では、未解析の画像は今は含まれるが、解析が終わると外れうる
        ("-透過:あり", Some(2)),
        // 17 は設定で決まっている
        ("ドット絵:あり", Some(1)),
        ("-ドット絵:あり", Some(1)),
        // 音声の事実は、未解析の音声（23）だけ
        ("ラウドネス>-14", Some(1)),
        ("-ループ:あり", Some(1)),
        // ほかの条件で決まるものは数えない
        ("種類:音楽 透過:あり", Some(0)),
        ("透過:あり | 種類:音楽", Some(2)),
        ("ループ:あり | 透過:あり", Some(3)),
        ("名前:asset15 透過:あり", Some(1)),
        ("-(透過:あり | 名前:asset15)", Some(1)),
    ];
    for (query, expected) in cases {
        assert_eq!(unanalyzed(&conn, query), *expected, "{query}");
    }
    // 解析が終われば数えない
    conn.execute_batch("UPDATE items SET analysis_state = 'ready' WHERE id IN (15, 17, 23)")
        .unwrap();
    assert_eq!(unanalyzed(&conn, "透過:あり"), Some(0));
}

#[test]
fn unanalyzed_counts_follow_the_list() {
    let conn = asset_library();
    conn.execute_batch(
        "INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (1, 'set', '素材', 'manual', 0, 0);
         INSERT INTO unit_items (unit_id, item_id, pos) VALUES (1, 15, 1), (1, 11, 2);",
    )
    .unwrap();
    let check = super::check(
        &conn,
        "透過:あり",
        crate::items::ItemView::Unit { id: 1 },
        &ctx(),
    )
    .unwrap();
    assert_eq!(check.unanalyzed, Some(1));
    let check = super::check(&conn, "透過:あり", crate::items::ItemView::Trash, &ctx()).unwrap();
    assert_eq!(check.unanalyzed, Some(1));
}

/// EXPLAIN 用に、バインドする値を埋め込む（テストだけ）。
fn explain(conn: &Connection, sql: &str, params: &[rusqlite::types::Value]) -> String {
    use rusqlite::types::Value;
    let mut sql = sql.to_string();
    for p in params {
        let literal = match p {
            Value::Integer(v) => v.to_string(),
            Value::Real(v) => format!("{v:?}"),
            Value::Text(t) => format!("'{t}'"),
            other => panic!("{other:?}"),
        };
        sql = sql.replacen('?', &literal, 1);
    }
    query_plan(conn, &sql)
}

#[test]
fn fact_conditions_are_list_subqueries_not_correlated() {
    let conn = asset_library();
    for (query, index) in [
        ("透過:あり", Some("idx_media_facts_alpha")),
        ("透過!=あり", Some("idx_media_facts_alpha")),
        ("色数<=16", Some("idx_media_facts_colors")),
        ("色数<1000", Some("idx_media_facts_colors")),
        ("色数>256", Some("idx_media_facts_colors")),
        ("ラウドネス>-14", Some("idx_media_facts_loudness")),
        ("ループ:あり", Some("idx_media_facts_loop")),
        ("ドット絵:あり", Some("idx_media_facts_pixel_art")),
        ("レート:48k", None),
        ("アニメ:あり", None),
        ("-透過:あり ループ:あり", Some("idx_media_facts_loop")),
    ] {
        let parsed = parse(query).unwrap();
        let cond = compile(&parsed.expr, &DbResolver { conn: &conn }, &ctx()).unwrap();
        let sql = format!(
            "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND ({}) ORDER BY i.imported_at DESC, i.id DESC",
            cond.sql
        );
        let plan = explain(&conn, &sql, &cond.params);
        assert!(plan.contains("LIST SUBQUERY"), "{query}\n{plan}");
        assert!(!plan.contains("CORRELATED"), "{query}\n{plan}");
        if let Some(index) = index {
            assert!(plan.contains(index), "{query}\n{plan}");
        }
    }
}

/// アニメの条件（格子の設定 ?? 解析のコマ。決定 16）は items を走査しない：格子の集合は `asset_settings`（変えたアイテムにだけ
/// 行がある）から、コマの数は事実から集合を作り、items は主キーとハッシュで引く（OR は MULTI-INDEX OR）。
#[test]
fn anim_conditions_find_items_without_scanning_them() {
    let conn = asset_library();
    for query in [
        "アニメ:あり",
        "アニメ!=なし",
        "アニメ:なし",
        "アニメ!=あり",
        "種類:画像 アニメ:あり",
        "透過:あり アニメ:なし",
        "アニメ:あり ドット絵:あり",
    ] {
        let parsed = parse(query).unwrap();
        let cond = compile(&parsed.expr, &DbResolver { conn: &conn }, &ctx()).unwrap();
        let sql = format!(
            "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND ({}) ORDER BY i.imported_at DESC, i.id DESC",
            cond.sql
        );
        let plan = explain(&conn, &sql, &cond.params);
        assert!(plan.contains("asset_settings"), "{query}\n{plan}");
        assert!(plan.contains("LIST SUBQUERY"), "{query}\n{plan}");
        assert!(!plan.contains("CORRELATED"), "{query}\n{plan}");
        assert!(
            !plan
                .split(" / ")
                .any(|step| step == "SCAN i" || step.starts_with("SCAN i ")),
            "{query}\n{plan}"
        );
    }
}

#[test]
fn facts_in_one_and_share_one_subquery() {
    let conn = asset_library();
    let sql = |query: &str| {
        let parsed = parse(query).unwrap();
        compile(&parsed.expr, &DbResolver { conn: &conn }, &ctx())
            .unwrap()
            .sql
    };
    // 同じ AND の事実は 1 つの部分問い合わせにまとめ、種類の索引に引っ張られないよう `+i.kind` にする
    let merged = sql("種類:画像 透過:あり 色数<=16");
    assert_eq!(merged.matches("media_facts").count(), 1, "{merged}");
    assert!(merged.contains("+i.kind"), "{merged}");
    // 除外の中の事実は集合で引けないので、種類の索引を使ってよい
    let excluded = sql("種類:音楽 -ループ:あり");
    assert!(!excluded.contains("+i.kind"), "{excluded}");
    // ドット絵（設定が勝つ）と OR の中はまとめない
    assert_eq!(
        sql("ドット絵:あり 透過:あり")
            .matches("media_facts")
            .count(),
        2
    );
    // `アニメ:なし` は事実（コマが 1 つ）をまとめ、格子がないことを足す。`アニメ:あり` は格子との OR なのでまとめない（決定 16）
    let still = sql("透過:あり アニメ:なし");
    assert_eq!(still.matches("media_facts").count(), 1, "{still}");
    assert_eq!(still.matches("asset_settings").count(), 1, "{still}");
    assert_eq!(
        sql("透過:あり アニメ:あり").matches("media_facts").count(),
        2
    );
    // 設定と事実の集合で引くドット絵・アニメと同じ AND の種類も、種類の索引に引っ張られないようにする
    for query in ["種類:画像 アニメ:あり", "種類:画像 ドット絵:あり"] {
        let merged = sql(query);
        assert!(merged.contains("+i.kind"), "{merged}");
    }
    assert!(!sql("種類:画像 -アニメ:あり").contains("+i.kind"));
    assert_eq!(
        sql("(透過:あり | ループ:あり) 色数<=16")
            .matches("media_facts")
            .count(),
        3
    );
    // まとめても結果は同じ
    assert_eq!(ids(&conn, "透過:あり 色数<=16 アニメ:なし"), vec![11, 14]);
    assert_eq!(ids(&conn, "透過:あり アニメ:なし"), vec![11, 14]);
    assert_eq!(ids(&conn, "種類:画像 アニメ:あり"), vec![13]);
    assert_eq!(ids(&conn, "-(透過:あり 色数<=8)"), {
        let mut all = vec![11, 12, 13, 15, 16, 17, 21, 22, 23, 24, 31, 41];
        all.sort_unstable();
        all
    });
    assert_eq!(
        ids(&conn, "種類:画像 透過:あり 2のべき乗:あり"),
        vec![11, 13, 14]
    );
}

#[test]
fn unanalyzed_count_reads_only_pending_items() {
    let conn = asset_library();
    let parsed = parse("透過:あり ラウドネス>-14 ドット絵:あり").unwrap();
    let cond = super::compile_unanalyzed(&parsed.expr, &DbResolver { conn: &conn }, &ctx())
        .unwrap()
        .unwrap();
    // 事実の表は読まない（種類と素材の設定だけで決まる）
    assert!(!cond.sql.contains("media_facts"), "{}", cond.sql);
    let sql = format!(
        "SELECT count(*) FROM items i WHERE i.trashed_at IS NULL AND ({})",
        cond.sql
    );
    let plan = explain(&conn, &sql, &cond.params);
    assert!(plan.contains("idx_items_analysis_pending"), "{plan}");
    assert!(!plan.contains("CORRELATED"), "{plan}");
}

#[test]
fn unit_smart_rules_judge_facts_by_any_member() {
    use crate::playlist::UnitMatch;
    use crate::playlist::generate::to_unit_cond;
    use crate::playlist::smart::{UnitCond, UnitFacts, unit_matches};
    let conn = asset_library();
    let facts = |members: Vec<i64>| UnitFacts {
        member_tags: vec![Default::default(); members.len()],
        members,
        ..UnitFacts::default()
    };
    for (query, members, expected) in [
        ("透過:あり", vec![12, 13], true),
        ("透過:あり", vec![12, 15], false),
        ("ループ:あり", vec![21, 22], true),
        ("ループ:あり", vec![22, 23], false),
        ("-ループ:あり", vec![22, 23], true),
        ("ラウドネス>-16", vec![21], true),
        ("ドット絵:あり", vec![17], true),
        ("2のべき乗:あり", vec![12, 15], true),
    ] {
        let expr = parse(query).unwrap().expr;
        let cond = to_unit_cond(&conn, &expr, &ctx()).unwrap();
        // 解析の事実はユニットの集計値ではないので、メンバーのどれかで判定する（playlists.md §4.2）
        let leaf = match &cond {
            UnitCond::Not(inner) => inner.as_ref(),
            other => other,
        };
        assert!(matches!(leaf, UnitCond::AnyMember(_)), "{query}: {cond:?}");
        assert_eq!(
            unit_matches(&cond, &facts(members.clone()), UnitMatch::Any, &ctx()),
            expected,
            "{query} {members:?}"
        );
    }
}
