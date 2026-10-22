//! プレイリストの DB の操作（作成・並べ替え・スマートの生成・再生のキュー・再開・追加）のテスト。

use rusqlite::{Connection, params};

use super::add::{AddOptions, AddSource, add};
use super::model::{EntryTarget, Granularity, LimitKind, PlaylistKind, UnitMatch};
use super::play::{PlayRequest, playlist_queue};
use super::smart::{RuleMatch, SmartRule};
use super::store::{self, NewEntry, NewPlaylist, PlayState, PlaylistPatch};
use crate::db::test_conn;
use crate::import::MediaKind;
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

#[test]
fn reorders_playlists_in_the_sidebar() {
    let conn = test_conn();
    let a = playlist(&conn, PlaylistKind::Manual, Granularity::File);
    let b = playlist(&conn, PlaylistKind::Smart, Granularity::File);
    let c = playlist(&conn, PlaylistKind::Manual, Granularity::Unit);
    let order = |conn: &Connection| -> Vec<i64> {
        store::list(conn).unwrap().iter().map(|p| p.id).collect()
    };
    assert_eq!(order(&conn), vec![a, b, c]);

    store::reorder(&conn, &[c, a, b]).unwrap();
    assert_eq!(order(&conn), vec![c, a, b]);
    // 渡さなかったもの・重ねて渡したもの・ないものがあっても、全部を 1 回ずつ並べる
    store::reorder(&conn, &[b, b, 999]).unwrap();
    assert_eq!(order(&conn), vec![b, c, a]);
}

#[test]
fn smart_units_resolve_quoted_tag_names() {
    // 「:」を含むタグは引用符で囲んで書き、タグとして判定する（search-query.md §2。ADR-127）
    let conn = test_conn();
    tag(&conn, 1, "Re:ゼロ");
    let a = item(&conn, "a", "image", 0);
    let b = item(&conn, "b", "image", 0);
    tag_item(&conn, 1, a);
    let unit = set_unit(&conn, "外伝", &[a, b]);
    let pl = playlist(&conn, PlaylistKind::Smart, Granularity::Unit);
    store::update(
        &conn,
        pl,
        &PlaylistPatch {
            rule: Some(SmartRule {
                match_mode: RuleMatch::All,
                clauses: vec!["\"Re:ゼロ\"".into()],
            }),
            unit_match: Some(UnitMatch::Any),
            ..PlaylistPatch::default()
        },
        1,
    )
    .unwrap();
    playlist_queue(&conn, pl, &PlayRequest::default(), &ctx()).unwrap();
    assert_eq!(targets(&conn, pl), vec![EntryTarget::Unit(unit)]);
}

/// 手動のプレイリストの中身の集計：フォント・3D を音楽に数えず、長さにも足さない。一覧と件数には出す
/// （入れられるが再生しないだけ。M11 の決定 11）。
#[test]
fn kind_counts_include_font_and_model() {
    let conn = test_conn();
    let image = item(&conn, "a", "image", 0);
    let font = item(&conn, "b", "font", 0);
    let model = item(&conn, "c", "model", 0);
    let audio = item(&conn, "d", "audio", 0);
    let loose_font = item(&conn, "e", "font", 0);
    let unit = set_unit(&conn, "素材", &[image, font, model, audio]);
    let pl = playlist(&conn, PlaylistKind::Manual, Granularity::Unit);
    store::add_entries(
        &conn,
        pl,
        &[
            NewEntry::from(EntryTarget::Unit(unit)),
            NewEntry::from(EntryTarget::Item(loose_font)),
        ],
        None,
        1,
    )
    .unwrap();
    let rows = store::contents(&conn, pl).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].items, vec![image, font, model, audio]);
    // 画像 8 秒 + 音楽 60 秒。フォント・3D は足さない
    assert_eq!(rows[0].duration_ms, 68_000);
    let k = rows[0].kind_counts;
    assert_eq!(
        (k.image, k.video, k.audio, k.font, k.model),
        (1, 0, 1, 1, 1)
    );
    assert_eq!(
        rows[0].kinds,
        vec![
            MediaKind::Image,
            MediaKind::Audio,
            MediaKind::Font,
            MediaKind::Model
        ]
    );
    assert_eq!(rows[1].kinds, vec![MediaKind::Font]);
    assert_eq!(rows[1].items, vec![loose_font]);
    assert_eq!(rows[1].duration_ms, 0);
    assert_eq!(store::item_count(&conn, pl).unwrap(), 5);
}

/// スマートの候補にフォント・3D を入れない（M11 の決定 11）。件数の上限をフォントが食わず、ユニット単位の
/// `種類=画像` はフォント・3D のメンバーを数えずに判定し、フォント・3D だけのユニットは候補にならない。
#[test]
fn smart_candidates_skip_unplayable_kinds() {
    let conn = test_conn();
    // 名前順ではフォントが先に来る
    let fonts: Vec<i64> = (0..10)
        .map(|i| item(&conn, &format!("a{i}"), "font", 5))
        .collect();
    let model = item(&conn, "b", "model", 5);
    let images: Vec<i64> = (1..=3)
        .map(|i| item(&conn, &format!("p{i}"), "image", 0))
        .collect();
    let set = |conn: &Connection, pl: i64, clauses: &[&str]| {
        store::update(
            conn,
            pl,
            &PlaylistPatch {
                shuffle: Some(false),
                rule: Some(SmartRule {
                    match_mode: RuleMatch::All,
                    clauses: clauses.iter().map(|c| c.to_string()).collect(),
                }),
                sort: Some(ItemSort {
                    key: SortKey::Name,
                    desc: false,
                }),
                limit: Some(Some((LimitKind::Count, 3))),
                include_loose_items: Some(false),
                ..PlaylistPatch::default()
            },
            1,
        )
        .unwrap();
        playlist_queue(conn, pl, &PlayRequest::default(), &ctx()).unwrap();
        targets(conn, pl)
    };
    let as_items = |ids: &[i64]| {
        ids.iter()
            .map(|i| EntryTarget::Item(*i))
            .collect::<Vec<_>>()
    };
    let files = playlist(&conn, PlaylistKind::Smart, Granularity::File);
    assert_eq!(set(&conn, files, &[]), as_items(&images));
    assert_eq!(set(&conn, files, &["評価>=0"]), as_items(&images));
    // 種類で選んでも、フォント・3D は候補にならない
    assert_eq!(set(&conn, files, &["種類:フォント"]), vec![]);
    assert_eq!(set(&conn, files, &["種類:3D"]), vec![]);

    set_unit(&conn, "フォント集", &fonts[..3]);
    let mixed = set_unit(&conn, "素材", &[images[0], fonts[3], model, images[1]]);
    let units = playlist(&conn, PlaylistKind::Smart, Granularity::Unit);
    // フォントだけのユニットは候補にならない
    assert_eq!(set(&conn, units, &[]), vec![EntryTarget::Unit(mixed)]);
    // `種類=画像`（メンバーが画像だけ）はフォント・3D を数えない
    assert_eq!(
        set(&conn, units, &["種類=画像"]),
        vec![EntryTarget::Unit(mixed)]
    );
    assert_eq!(set(&conn, units, &["種類:フォント"]), vec![]);
}

/// 一覧の「ここから再生」（T16・T17 の続き）：エントリの行は先頭のメンバー（`coverItemId`。フォント・3D でもある）を渡すので、
/// 渡したアイテムがキューにない種類なら、そのアイテムを含むエントリの最初のスロットから始める（プレイリストの先頭からに
/// しない）。エントリが丸ごとキューにない（フォント・3D だけ）なら、その後ろで最初にキューにあるエントリから。ファイル単位・
/// ユニット単位、スマートの「今回の並び」でも同じ（Q1）。
#[test]
fn playing_from_a_row_whose_item_never_reaches_the_queue_starts_at_its_entry() {
    let conn = test_conn();
    let a = item(&conn, "a", "image", 0);
    let f = item(&conn, "f", "font", 0);
    let b = item(&conn, "b", "audio", 0);
    let g = item(&conn, "g", "model", 0);
    let c = item(&conn, "c", "video", 0);
    let ux = set_unit(&conn, "X", &[a]);
    let uy = set_unit(&conn, "Y", &[f, b]);
    let uz = set_unit(&conn, "Z", &[g]);
    let uw = set_unit(&conn, "W", &[c]);
    let from = |pl: i64, item: i64| {
        let q = playlist_queue(
            &conn,
            pl,
            &PlayRequest {
                from_item: Some(item),
                ..PlayRequest::default()
            },
            &ctx(),
        )
        .unwrap();
        q.slots[q.start_index].item_id
    };
    for granularity in [Granularity::Unit, Granularity::File] {
        let pl = playlist(&conn, PlaylistKind::Manual, granularity);
        store::add_entries(
            &conn,
            pl,
            &[ux, uy, uz, uw].map(|u| NewEntry::from(EntryTarget::Unit(u))),
            None,
            1,
        )
        .unwrap();
        // 行の先頭のメンバー（dto の coverItemId）はフォント
        let rows = store::contents(&conn, pl).unwrap();
        assert_eq!(rows[1].items.first().copied(), Some(f));
        assert_eq!(from(pl, f), b, "{granularity:?}：ユニット Y から");
        assert_eq!(
            from(pl, g),
            c,
            "{granularity:?}：3D だけの Z は後ろの W から"
        );
        assert_eq!(from(pl, b), b, "{granularity:?}");
    }
    // スマート（ユニット単位、条件なし、シャッフルなし）の今回の並びの行でも
    let smart = playlist(&conn, PlaylistKind::Smart, Granularity::Unit);
    store::update(
        &conn,
        smart,
        &PlaylistPatch {
            shuffle: Some(false),
            include_loose_items: Some(false),
            ..PlaylistPatch::default()
        },
        1,
    )
    .unwrap();
    playlist_queue(&conn, smart, &PlayRequest::default(), &ctx()).unwrap();
    assert_eq!(from(smart, f), b);
}

/// M11 の確かめ R4（決定 19）：ファイル単位の一覧ではユニットのメンバーが 1 行ずつ並ぶので、途中のフォント・3D の行から
/// 「ここから再生」すると、押した行より後ろで最初に再生できるメンバーから始める（ユニットの中になければ後ろのエントリへ）。
/// ユニット単位の一覧にも ▸ で開いた中身のメンバーが 1 行ずつ並ぶので、先頭でないメンバーの行は同じく後ろから（2 回目の
/// 確認 P1）。行のエントリを渡さない依頼（`from_entry` なし）は、アイテムを含む最初のエントリの行とみなす。
#[test]
fn playing_from_a_font_row_in_the_middle_of_a_unit_starts_after_it_by_file() {
    let conn = test_conn();
    let a = item(&conn, "a", "image", 0);
    let b = item(&conn, "b", "audio", 0);
    let f = item(&conn, "f", "font", 0);
    let c = item(&conn, "c", "video", 0);
    let d = item(&conn, "d", "audio", 0);
    let g = item(&conn, "g", "model", 0);
    let e = item(&conn, "e", "image", 0);
    let ux = set_unit(&conn, "X", &[a]);
    let uy = set_unit(&conn, "Y", &[b, f, c]);
    let uz = set_unit(&conn, "Z", &[d, g]);
    let uw = set_unit(&conn, "W", &[e]);
    let from = |pl: i64, item: i64| {
        let q = playlist_queue(
            &conn,
            pl,
            &PlayRequest {
                from_item: Some(item),
                ..PlayRequest::default()
            },
            &ctx(),
        )
        .unwrap();
        q.slots[q.start_index].item_id
    };
    let make = |granularity: Granularity, shuffle: bool| {
        let pl = playlist(&conn, PlaylistKind::Manual, granularity);
        store::add_entries(
            &conn,
            pl,
            &[ux, uy, uz, uw].map(|u| NewEntry::from(EntryTarget::Unit(u))),
            None,
            1,
        )
        .unwrap();
        store::update(
            &conn,
            pl,
            &PlaylistPatch {
                shuffle: Some(shuffle),
                ..PlaylistPatch::default()
            },
            1,
        )
        .unwrap();
        // 一覧の行の並び（メンバーの順）
        let rows = store::contents(&conn, pl).unwrap();
        assert_eq!(rows[1].items, vec![b, f, c]);
        assert_eq!(rows[2].items, vec![d, g]);
        pl
    };
    for shuffle in [false, true] {
        let file = make(Granularity::File, shuffle);
        assert_eq!(
            from(file, f),
            c,
            "ファイル単位（シャッフル {shuffle}）：f の行の次の c から"
        );
        assert_eq!(
            from(file, g),
            e,
            "ファイル単位（シャッフル {shuffle}）：ユニットの最後の 3D の行は後ろのエントリの e から"
        );
        assert_eq!(from(file, c), c, "再生できる行はその行から");
        let unit = make(Granularity::Unit, shuffle);
        // ユニット単位でも、先頭でないメンバーは ▸ で開いた中身の行なので、押した行より後ろから（2 回目の確認 P1。
        // R4 の直しはユニット単位を「そのエントリの最初から」のまま残していた）
        assert_eq!(
            from(unit, f),
            c,
            "ユニット単位（シャッフル {shuffle}）：開いた Y の f の行の次の c から"
        );
        assert_eq!(
            from(unit, g),
            e,
            "ユニット単位（シャッフル {shuffle}）：開いた Z の最後の 3D の行は後ろのエントリ W の最初から"
        );
        assert_eq!(
            from(unit, b),
            b,
            "ユニット単位：Y の行（先頭のメンバー）は Y の最初から"
        );
    }
}

/// M11 の 2 回目の確認 P1・P2（決定 19「後ろで最初に再生できる行から」）：一覧の「ここから再生」は押した行のエントリも渡す
/// （`from_entry`）。ユニット単位で ▸ で開いた中身の途中のフォント・3D の行は、ファイル単位と同じく、そのエントリの中で
/// 押した行より後ろで最初に再生できるメンバーから（なければ後ろのエントリの最初から。ユニットの先頭に戻らない。P1）。
/// エントリの行と、開いた中身の先頭のメンバーの行は、今までどおりそのエントリの最初から。同じアイテムが 2 つのエントリに
/// あっても、押した行のエントリで探す（前のエントリの行として扱わない。再生できるアイテムも、そのエントリのスロットから。P2）。
#[test]
fn playing_from_a_row_starts_within_its_entry() {
    let conn = test_conn();
    let a = item(&conn, "a", "image", 0);
    let b = item(&conn, "b", "audio", 0);
    let f = item(&conn, "f", "font", 0);
    let c = item(&conn, "c", "video", 0);
    let d = item(&conn, "d", "audio", 0);
    let g = item(&conn, "g", "model", 0);
    let e = item(&conn, "e", "image", 0);
    let h = item(&conn, "h", "font", 0);
    // 始めるスロットの（アイテム, エントリ）
    let from = |pl: i64, entry: i64, item: i64| {
        let q = playlist_queue(
            &conn,
            pl,
            &PlayRequest {
                from_item: Some(item),
                from_entry: Some(entry),
                ..PlayRequest::default()
            },
            &ctx(),
        )
        .unwrap();
        let s = &q.slots[q.start_index];
        (s.item_id, s.entry_id.unwrap_or(0))
    };
    let make = |granularity: Granularity, shuffle: bool, units: &[i64]| {
        let pl = playlist(&conn, PlaylistKind::Manual, granularity);
        let entries: Vec<NewEntry> = units
            .iter()
            .map(|u| NewEntry::from(EntryTarget::Unit(*u)))
            .collect();
        store::add_entries(&conn, pl, &entries, None, 1).unwrap();
        store::update(
            &conn,
            pl,
            &PlaylistPatch {
                shuffle: Some(shuffle),
                ..PlaylistPatch::default()
            },
            1,
        )
        .unwrap();
        let ids: Vec<i64> = store::contents(&conn, pl)
            .unwrap()
            .iter()
            .map(|r| r.entry.id)
            .collect();
        (pl, ids)
    };

    // P1：ユニット単位で開いた中身の行。X(a)・Y(b, f, c)・Z(d, g)・V(h, e)
    let ux = set_unit(&conn, "X", &[a]);
    let uy = set_unit(&conn, "Y", &[b, f, c]);
    let uz = set_unit(&conn, "Z", &[d, g]);
    let uv = set_unit(&conn, "V", &[h, e]);
    for shuffle in [false, true] {
        let (pl, ids) = make(Granularity::Unit, shuffle, &[ux, uy, uz, uv]);
        let (y, z, v) = (ids[1], ids[2], ids[3]);
        assert_eq!(
            from(pl, y, f),
            (c, y),
            "シャッフル {shuffle}：Y の中の f の行は、1 行上の b でなく後ろの c から"
        );
        assert_eq!(
            from(pl, z, g),
            (e, v),
            "シャッフル {shuffle}：Z の最後の 3D の行は、後ろのエントリ V の最初から"
        );
        assert_eq!(from(pl, y, c), (c, y), "再生できる行はその行から");
        assert_eq!(
            from(pl, y, b),
            (b, y),
            "Y の行（先頭のメンバー）は Y の最初から"
        );
        assert_eq!(
            from(pl, v, h),
            (e, v),
            "先頭のメンバーがフォントの V の行は V の最初から"
        );
    }

    // P2：同じフォントが 2 つのエントリにある。X2(f, a)・Y2(b, f, c)。ファイル単位の一覧の行は
    // 「X2 から・f・a・Y2 から・b・f・c」、ユニット単位で両方を開くと「X2・f・a・Y2・b・f・c」
    let ux2 = set_unit(&conn, "X2", &[f, a]);
    let uy2 = set_unit(&conn, "Y2", &[b, f, c]);
    for granularity in [Granularity::File, Granularity::Unit] {
        for shuffle in [false, true] {
            let (pl, ids) = make(granularity, shuffle, &[ux2, uy2]);
            let (x2, y2) = (ids[0], ids[1]);
            assert_eq!(
                from(pl, y2, f),
                (c, y2),
                "{granularity:?}・シャッフル {shuffle}：Y2 の f の行は、前の X2 の a でなく c から"
            );
            assert_eq!(
                from(pl, x2, f),
                (a, x2),
                "{granularity:?}・シャッフル {shuffle}：X2 の f の行は a から"
            );
        }
    }

    // P2 の続き：再生できるアイテムが 2 つのエントリにあっても、押した行のエントリのスロットから。X3(b, a)・Y3(c, b)
    let ux3 = set_unit(&conn, "X3", &[b, a]);
    let uy3 = set_unit(&conn, "Y3", &[c, b]);
    for shuffle in [false, true] {
        let (pl, ids) = make(Granularity::File, shuffle, &[ux3, uy3]);
        // 既定の「重複を除く」では、後ろの Y3 の b はキューにない（b は X3 の位置で 1 度だけ流れる）ので、b のスロットから
        assert_eq!(from(pl, ids[1], b), (b, ids[0]), "シャッフル {shuffle}");
        store::update(
            &conn,
            pl,
            &PlaylistPatch {
                dedup_items: Some(false),
                ..PlaylistPatch::default()
            },
            1,
        )
        .unwrap();
        assert_eq!(from(pl, ids[1], b), (b, ids[1]), "シャッフル {shuffle}");
        assert_eq!(from(pl, ids[0], b), (b, ids[0]), "シャッフル {shuffle}");
    }
}
