//! プレイリストの DB の操作（作成・並べ替え・スマートの生成・再生のキュー・再開・追加）のテスト。

use rusqlite::{Connection, params};

use super::add::{AddOptions, AddSource, add};
use super::model::{EntryTarget, Granularity, LimitKind, PlaylistKind, UnitMatch};
use super::play::{PlayRequest, playlist_queue};
use super::smart::{RuleMatch, SmartRule};
use super::store::{self, NewEntry, NewPlaylist, PlayState, PlaylistPatch};
use crate::db::test_conn;
use crate::items::{ItemSort, SortKey};
use crate::query::QueryContext;

const DAY: i64 = 24 * 60 * 60 * 1000;

fn ctx() -> QueryContext {
    QueryContext {
        now_ms: 1000 * DAY,
        utc_offset_minutes: 540,
    }
}

/// アイテムを入れる（タイトル・種類・評価）。ID を返す。
fn item(conn: &Connection, title: &str, kind: &str, rating: i64) -> i64 {
    let n: i64 = conn
        .query_row("SELECT count(*) FROM items", [], |r| r.get(0))
        .unwrap();
    conn.execute(
        "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, rating, duration_ms)
         VALUES (?1, 'x', ?2, 1, ?3, ?3, ?4, ?4, ?5, ?6)",
        params![
            (n + 1).to_le_bytes().to_vec(),
            kind,
            title,
            n + 1,
            rating,
            (kind != "image").then_some(60_000i64)
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn tag(conn: &Connection, id: i64, name: &str) {
    conn.execute(
        "INSERT INTO tags (id, namespace, name, created_at) VALUES (?1, '', ?2, 0)",
        params![id, name],
    )
    .unwrap();
}

fn tag_item(conn: &Connection, tag: i64, item: i64) {
    conn.execute(
        "INSERT INTO item_tags (tag_id, item_id, source) VALUES (?1, ?2, 'user')",
        params![tag, item],
    )
    .unwrap();
}

fn set_unit(conn: &Connection, name: &str, members: &[i64]) -> i64 {
    crate::unit::create_set(conn, name, members, 0).unwrap()
}

fn playlist(conn: &Connection, kind: PlaylistKind, granularity: Granularity) -> i64 {
    store::create(
        conn,
        &NewPlaylist {
            name: "夜の作業".into(),
            kind,
            granularity,
        },
        0,
    )
    .unwrap()
}

fn targets(conn: &Connection, id: i64) -> Vec<EntryTarget> {
    store::entries(conn, id)
        .unwrap()
        .into_iter()
        .map(|e| e.target)
        .collect()
}

#[test]
fn creates_updates_and_deletes_playlists() {
    let conn = test_conn();
    let manual = playlist(&conn, PlaylistKind::Manual, Granularity::File);
    let smart = playlist(&conn, PlaylistKind::Smart, Granularity::Unit);
    let list = store::list(&conn).unwrap();
    assert_eq!(
        list.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![manual, smart]
    );
    let s = &list[1];
    assert_eq!(
        (s.kind, s.granularity, s.shuffle, s.limit),
        (
            PlaylistKind::Smart,
            Granularity::Unit,
            true,
            Some((LimitKind::Count, 50))
        )
    );
    assert_eq!(s.rule, Some(SmartRule::default()));
    assert!(
        store::create(
            &conn,
            &NewPlaylist {
                name: "  ".into(),
                kind: PlaylistKind::Manual,
                granularity: Granularity::File
            },
            0
        )
        .is_err()
    );

    store::update(
        &conn,
        manual,
        &PlaylistPatch {
            name: Some("朝".into()),
            image_duration_ms: Some(500),
            max_same_kind_run: Some(0),
            ..PlaylistPatch::default()
        },
        5,
    )
    .unwrap();
    let p = store::get(&conn, manual).unwrap();
    assert_eq!(
        (p.name.as_str(), p.image_duration_ms, p.max_same_kind_run),
        ("朝", 1000, 0)
    );
    // 手動に条件は付けられない。解析できない条件は保存しない
    let bad = |rule: &str| PlaylistPatch {
        rule: Some(SmartRule {
            match_mode: RuleMatch::All,
            clauses: vec![rule.into()],
        }),
        ..PlaylistPatch::default()
    };
    assert!(store::update(&conn, manual, &bad("評価>=3"), 6).is_err());
    assert!(store::update(&conn, smart, &bad("(海"), 6).is_err());
    store::update(&conn, smart, &bad("評価>=3"), 6).unwrap();

    store::delete(&conn, manual).unwrap();
    assert!(store::get(&conn, manual).is_err());
}

#[test]
fn moves_removes_and_renumbers_entries() {
    let conn = test_conn();
    let ids: Vec<i64> = (0..4)
        .map(|i| item(&conn, &format!("i{i}"), "image", 0))
        .collect();
    let pl = playlist(&conn, PlaylistKind::Manual, Granularity::File);
    let entry_ids = store::add_entries(
        &conn,
        pl,
        &ids.iter()
            .map(|i| NewEntry::from(EntryTarget::Item(*i)))
            .collect::<Vec<_>>(),
        None,
        0,
    )
    .unwrap();
    // 3 番目と 4 番目を先頭へ（順は保つ）
    store::move_entries(
        &conn,
        pl,
        &[entry_ids[3], entry_ids[2]],
        Some(entry_ids[0]),
        1,
    )
    .unwrap();
    let order = |conn: &Connection| -> Vec<EntryTarget> { targets(conn, pl) };
    assert_eq!(
        order(&conn),
        [2, 3, 0, 1].map(|i| EntryTarget::Item(ids[i])).to_vec()
    );
    // 同じ間に何度も入れると間が詰まるが、振り直して順を保つ
    for _ in 0..80 {
        store::move_entries(&conn, pl, &[entry_ids[1]], Some(entry_ids[0]), 2).unwrap();
        store::move_entries(&conn, pl, &[entry_ids[0]], Some(entry_ids[1]), 2).unwrap();
    }
    let entries = store::entries(&conn, pl).unwrap();
    assert!(entries.windows(2).all(|w| w[1].pos - w[0].pos >= 1e-9));
    store::remove_entries(&conn, pl, &[entry_ids[2]], 3).unwrap();
    assert_eq!(store::entries(&conn, pl).unwrap().len(), 3);

    // 取り消し：前のエントリにそのまま戻す
    let before = store::entries(&conn, pl).unwrap();
    store::replace_entries(&conn, pl, &[], 4).unwrap();
    store::replace_entries(&conn, pl, &before, 5).unwrap();
    assert_eq!(store::entries(&conn, pl).unwrap(), before);
}

#[test]
fn smart_files_follow_rules_order_and_limits() {
    let conn = test_conn();
    let b = item(&conn, "b", "image", 4);
    let a = item(&conn, "a", "audio", 5);
    let c = item(&conn, "c", "video", 3);
    item(&conn, "d", "image", 1);
    let pl = playlist(&conn, PlaylistKind::Smart, Granularity::File);
    store::update(
        &conn,
        pl,
        &PlaylistPatch {
            shuffle: Some(false),
            rule: Some(SmartRule {
                match_mode: RuleMatch::All,
                clauses: vec!["評価>=3".into()],
            }),
            sort: Some(ItemSort {
                key: SortKey::Name,
                desc: false,
            }),
            limit: Some(None),
            ..PlaylistPatch::default()
        },
        1,
    )
    .unwrap();
    let play = |conn: &Connection| {
        conn.execute_batch("BEGIN").unwrap();
        let q = playlist_queue(conn, pl, &PlayRequest::default(), &ctx()).unwrap();
        conn.execute_batch("COMMIT").unwrap();
        q.slots.iter().map(|s| s.item_id).collect::<Vec<_>>()
    };
    assert_eq!(play(&conn), vec![a, b, c]);
    let p = store::get(&conn, pl).unwrap();
    assert_eq!(p.generated_at, Some(ctx().now_ms));
    assert_eq!(
        targets(&conn, pl),
        vec![
            EntryTarget::Item(a),
            EntryTarget::Item(b),
            EntryTarget::Item(c)
        ]
    );

    // 件数の上限
    store::update(
        &conn,
        pl,
        &PlaylistPatch {
            limit: Some(Some((LimitKind::Count, 2))),
            ..PlaylistPatch::default()
        },
        2,
    )
    .unwrap();
    assert_eq!(play(&conn), vec![a, b]);
    // 合計時間の上限（音楽 60 秒 + 画像 8 秒 = 68 秒、次の動画で 128 秒）
    store::update(
        &conn,
        pl,
        &PlaylistPatch {
            limit: Some(Some((LimitKind::Duration, 100_000))),
            ..PlaylistPatch::default()
        },
        3,
    )
    .unwrap();
    assert_eq!(play(&conn), vec![a, b]);
    // いずれかに一致
    store::update(
        &conn,
        pl,
        &PlaylistPatch {
            limit: Some(None),
            rule: Some(SmartRule {
                match_mode: RuleMatch::Any,
                clauses: vec!["種類:動画".into(), "評価=5".into()],
            }),
            ..PlaylistPatch::default()
        },
        4,
    )
    .unwrap();
    assert_eq!(play(&conn), vec![a, c]);
}

#[test]
fn smart_units_use_unit_match_and_loose_items() {
    let conn = test_conn();
    tag(&conn, 1, "夜景");
    let x1 = item(&conn, "x1", "image", 0);
    let x2 = item(&conn, "x2", "image", 0);
    let y1 = item(&conn, "y1", "image", 0);
    let loose = item(&conn, "loose", "image", 0);
    tag_item(&conn, 1, x1);
    tag_item(&conn, 1, y1);
    tag_item(&conn, 1, loose);
    let half = set_unit(&conn, "半分", &[x1, x2]);
    let all = set_unit(&conn, "全部", &[y1]);
    let pl = playlist(&conn, PlaylistKind::Smart, Granularity::Unit);
    let set = |conn: &Connection, m: UnitMatch, loose: bool| {
        store::update(
            conn,
            pl,
            &PlaylistPatch {
                shuffle: Some(false),
                rule: Some(SmartRule {
                    match_mode: RuleMatch::All,
                    clauses: vec!["夜景".into()],
                }),
                sort: Some(ItemSort {
                    key: SortKey::Name,
                    desc: false,
                }),
                unit_match: Some(m),
                include_loose_items: Some(loose),
                ..PlaylistPatch::default()
            },
            1,
        )
        .unwrap();
        playlist_queue(conn, pl, &PlayRequest::default(), &ctx()).unwrap();
        targets(conn, pl)
    };
    // 名前順（「全部」<「半分」）
    assert_eq!(
        set(&conn, UnitMatch::Any, false),
        vec![EntryTarget::Unit(all), EntryTarget::Unit(half)]
    );
    assert_eq!(
        set(&conn, UnitMatch::All, false),
        vec![EntryTarget::Unit(all)]
    );
    assert_eq!(set(&conn, UnitMatch::UnitOnly, false), vec![]);
    // どのユニットにも属さないアイテムも 1 件ずつ候補に
    assert_eq!(
        set(&conn, UnitMatch::All, true),
        vec![EntryTarget::Item(loose), EntryTarget::Unit(all)]
    );
}

#[test]
fn smart_regenerates_on_play_unless_seed_is_locked() {
    let conn = test_conn();
    for i in 0..30 {
        item(&conn, &format!("i{i:02}"), "image", 0);
    }
    let pl = playlist(&conn, PlaylistKind::Smart, Granularity::File);
    let order = |conn: &Connection| -> Vec<i64> {
        playlist_queue(conn, pl, &PlayRequest::default(), &ctx())
            .unwrap()
            .slots
            .iter()
            .map(|s| s.item_id)
            .collect()
    };
    let first = order(&conn);
    assert_eq!(first.len(), 30);
    assert_ne!(first, order(&conn), "再生のたびにシャッフルし直していない");

    store::update(
        &conn,
        pl,
        &PlaylistPatch {
            seed_locked: Some(true),
            ..PlaylistPatch::default()
        },
        1,
    )
    .unwrap();
    let locked = order(&conn);
    assert_eq!(locked, order(&conn), "シードを固定しても並びが変わる");

    // 作り直さない設定なら、並べ直すまで同じ
    store::update(
        &conn,
        pl,
        &PlaylistPatch {
            seed_locked: Some(false),
            regenerate_on_play: Some(false),
            ..PlaylistPatch::default()
        },
        2,
    )
    .unwrap();
    let kept = order(&conn);
    assert_eq!(kept, order(&conn));
    let reseeded: Vec<i64> = playlist_queue(
        &conn,
        pl,
        &PlayRequest {
            reseed: true,
            ..PlayRequest::default()
        },
        &ctx(),
    )
    .unwrap()
    .slots
    .iter()
    .map(|s| s.item_id)
    .collect();
    assert_ne!(kept, reseeded);
}

#[test]
fn playing_from_a_row_keeps_the_generated_order() {
    let conn = test_conn();
    for i in 0..20 {
        item(&conn, &format!("i{i:02}"), "image", 0);
    }
    let pl = playlist(&conn, PlaylistKind::Smart, Granularity::File);
    let shown: Vec<i64> = playlist_queue(&conn, pl, &PlayRequest::default(), &ctx())
        .unwrap()
        .slots
        .iter()
        .map(|s| s.item_id)
        .collect();
    // 再生のたびに作り直す設定でも、一覧の「ここから再生」は表示している並びのまま
    let from = playlist_queue(
        &conn,
        pl,
        &PlayRequest {
            from_item: Some(shown[7]),
            ..PlayRequest::default()
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        from.slots.iter().map(|s| s.item_id).collect::<Vec<_>>(),
        shown
    );
    assert_eq!(from.start_index, 7);
}

#[test]
fn manual_queue_resumes_and_toggles_shuffle_from_the_current_item() {
    let conn = test_conn();
    let ids: Vec<i64> = (0..10)
        .map(|i| item(&conn, &format!("i{i}"), "image", 0))
        .collect();
    let pl = playlist(&conn, PlaylistKind::Manual, Granularity::File);
    store::add_entries(
        &conn,
        pl,
        &ids.iter()
            .map(|i| NewEntry::from(EntryTarget::Item(*i)))
            .collect::<Vec<_>>(),
        None,
        0,
    )
    .unwrap();
    let q = playlist_queue(&conn, pl, &PlayRequest::default(), &ctx()).unwrap();
    assert_eq!(q.slots.iter().map(|s| s.item_id).collect::<Vec<_>>(), ids);
    store::save_state(
        &conn,
        pl,
        &PlayState {
            seed: q.seed,
            slot_index: 4,
            item_id: Some(ids[4]),
            position_ms: 2500,
            updated_at: 1,
        },
    )
    .unwrap();
    let resumed = playlist_queue(
        &conn,
        pl,
        &PlayRequest {
            resume: true,
            ..PlayRequest::default()
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        (resumed.start_index, resumed.start_ms, resumed.seed),
        (4, 2500, q.seed)
    );

    // S：シャッフルにして、今のアイテムを先頭に
    let shuffled = playlist_queue(
        &conn,
        pl,
        &PlayRequest {
            shuffle: Some(true),
            start_item: Some(ids[4]),
            ..PlayRequest::default()
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        (shuffled.start_index, shuffled.slots[0].item_id),
        (0, ids[4])
    );
    assert!(store::get(&conn, pl).unwrap().shuffle);
    store::clear_state(&conn, pl).unwrap();
    assert_eq!(store::load_state(&conn, pl).unwrap(), None);
}

#[test]
fn adds_by_the_table_and_groups_items_into_a_unit() {
    let conn = test_conn();
    let a = item(&conn, "a", "image", 0);
    let b = item(&conn, "b", "image", 0);
    let c = item(&conn, "c", "image", 0);
    let unit = set_unit(&conn, "既存", &[b, c]);
    let file = playlist(&conn, PlaylistKind::Manual, Granularity::File);
    add(
        &conn,
        file,
        &[AddSource::Item(a), AddSource::Unit(unit)],
        &AddOptions::default(),
        None,
        1,
    )
    .unwrap();
    assert_eq!(
        targets(&conn, file),
        vec![
            EntryTarget::Item(a),
            EntryTarget::Item(b),
            EntryTarget::Item(c)
        ]
    );

    let by_unit = playlist(&conn, PlaylistKind::Manual, Granularity::Unit);
    let added = add(
        &conn,
        by_unit,
        &[AddSource::Item(a), AddSource::Item(c)],
        &AddOptions {
            invert: false,
            group_name: Some("鎌倉".into()),
        },
        None,
        2,
    )
    .unwrap();
    let created = added.unit_id.unwrap();
    assert_eq!(targets(&conn, by_unit), vec![EntryTarget::Unit(created)]);
    assert_eq!(crate::unit::members(&conn, created).unwrap(), vec![a, c]);
    // 先頭の前に入れる
    add(
        &conn,
        by_unit,
        &[AddSource::Unit(unit)],
        &AddOptions::default(),
        Some(added.entry_ids[0]),
        3,
    )
    .unwrap();
    assert_eq!(
        targets(&conn, by_unit),
        vec![EntryTarget::Unit(unit), EntryTarget::Unit(created)]
    );
}
