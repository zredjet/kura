//! 受け入れテスト T1〜T15（docs/playlists.md §8）。

use std::collections::HashSet;

use rusqlite::{Connection, params};

use super::build_queue;
use super::model::{Entry, EntryTarget, Granularity, PlaylistKind, QueueOptions, UnitMatch};
use super::resolve::{ResolvedEntry, Snapshot, UnitData, resolve};
use super::smart::{UnitCond, UnitFacts, unit_matches};
use super::store::{self, NewEntry, NewPlaylist};
use crate::db::test_conn;
use crate::query::QueryContext;
use crate::queue::{Slot, UnitKey};
use crate::unit::{MemberInfo, OrderMode, UnitDef, UnitKind};

const DAY: i64 = 24 * 60 * 60 * 1000;
const NOW: i64 = 1_000 * DAY;

// ===== 純粋関数のテスト用のライブラリ =====

/// スナップショットを組み立てる。アイテムのタイトルは「i<ID>」、追加日は ID。
#[derive(Default)]
struct Lib {
    snap: Snapshot,
    next_entry: i64,
}

impl Lib {
    fn item(&mut self, id: i64, kind: &str) -> &mut Self {
        self.snap.members.items.insert(
            id,
            MemberInfo {
                title: format!("i{id}"),
                kind: kind.into(),
                imported_at: id,
                captured_at: None,
                duration_ms: (kind != "image").then_some(60_000),
                rating: 0,
                last_played_at: None,
                trashed: false,
            },
        );
        self
    }

    fn images(&mut self, ids: std::ops::RangeInclusive<i64>) -> &mut Self {
        for id in ids {
            self.item(id, "image");
        }
        self
    }

    fn rate(&mut self, id: i64, rating: i64) -> &mut Self {
        if let Some(info) = self.snap.members.items.get_mut(&id) {
            info.rating = rating;
        }
        self
    }

    fn trash(&mut self, id: i64) -> &mut Self {
        if let Some(info) = self.snap.members.items.get_mut(&id) {
            info.trashed = true;
        }
        self
    }

    /// set ユニット（manual の並び）。
    fn set_unit(&mut self, unit_id: i64, members: &[i64]) -> &mut Self {
        self.snap.units.insert(
            unit_id,
            UnitData {
                def: UnitDef {
                    kind: UnitKind::Set,
                    order_mode: OrderMode::Manual,
                    reverse: false,
                    items: members
                        .iter()
                        .enumerate()
                        .map(|(i, id)| (*id, i as f64))
                        .collect(),
                    folders: Vec::new(),
                },
                name: format!("u{unit_id}"),
                image_duration_ms: None,
                inner_shuffle: false,
                rating: 0,
                last_played_at: None,
            },
        );
        self
    }

    fn unit_mut(&mut self, unit_id: i64) -> &mut UnitData {
        self.snap
            .units
            .get_mut(&unit_id)
            .unwrap_or_else(|| panic!("ユニット {unit_id} がない"))
    }

    fn entry(&mut self, target: EntryTarget) -> &mut Self {
        self.next_entry += 1;
        let id = self.next_entry;
        self.snap.entries.push(Entry::new(id, id as f64, target));
        self
    }

    fn resolved(&self) -> Vec<ResolvedEntry> {
        resolve(&self.snap)
    }

    fn queue(&self, options: &QueueOptions, seed: u64) -> Vec<Slot> {
        build_queue(&resolve(&self.snap), &self.snap, options, seed)
    }
}

fn options(granularity: Granularity, shuffle: bool) -> QueueOptions {
    QueueOptions {
        granularity,
        shuffle,
        weight_by_rating: false,
        max_same_kind_run: 0,
        dedup_items: true,
        image_duration_ms: 8000,
        now_ms: NOW,
    }
}

fn items(queue: &[Slot]) -> Vec<i64> {
    queue.iter().map(|s| s.item_id).collect()
}

const A: i64 = 1;
const B: i64 = 2;

// ===== T1〜T15 =====

#[test]
fn t1_file_granularity_expands_entries_in_order() {
    let mut lib = Lib::default();
    lib.images(1..=4)
        .set_unit(A, &[1, 2, 3])
        .entry(EntryTarget::Unit(A))
        .entry(EntryTarget::Item(4));
    let queue = lib.queue(&options(Granularity::File, false), 7);
    assert_eq!(items(&queue), vec![1, 2, 3, 4]);
}

#[test]
fn t2_unit_shuffle_keeps_unit_members_together_and_in_order() {
    let mut lib = Lib::default();
    lib.images(1..=10)
        .set_unit(A, &[1, 2, 3])
        .entry(EntryTarget::Unit(A));
    for id in 4..=10 {
        lib.entry(EntryTarget::Item(id));
    }
    let mut moved = false;
    for seed in 0..30 {
        let queue = items(&lib.queue(&options(Granularity::Unit, true), seed));
        assert_eq!(queue.len(), 10);
        let start = queue.iter().position(|&id| id == 1).unwrap_or(usize::MAX);
        assert_eq!(
            queue.get(start..start + 3),
            Some(&[1, 2, 3][..]),
            "seed {seed}: {queue:?}"
        );
        moved |= start != 0;
    }
    assert!(moved, "シャッフルでユニットの位置が変わっていない");
}

#[test]
fn t3_switching_granularity_keeps_entries_and_changes_only_the_queue() {
    let conn = test_conn();
    let ids = insert_items(&conn, 12, "image");
    insert_set_unit(&conn, A, "鎌倉", &ids[0..4]);
    let pl = store::create(&conn, &new_playlist(Granularity::Unit), 0).unwrap();
    let mut targets = vec![NewEntry::from(EntryTarget::Unit(A))];
    targets.extend(
        ids[4..]
            .iter()
            .map(|id| NewEntry::from(EntryTarget::Item(*id))),
    );
    store::add_entries(&conn, pl, &targets, None, 0).unwrap();

    let before = store::entries(&conn, pl).unwrap();
    let snap = store::load_snapshot(&conn, pl).unwrap();
    let unit_queue = items(&build_queue(
        &resolve(&snap),
        &snap,
        &options(Granularity::Unit, true),
        11,
    ));

    store::set_granularity(&conn, pl, Granularity::File, 1).unwrap();
    assert_eq!(
        store::entries(&conn, pl).unwrap(),
        before,
        "エントリが変わった"
    );
    let snap = store::load_snapshot(&conn, pl).unwrap();
    let file_queue = items(&build_queue(
        &resolve(&snap),
        &snap,
        &options(Granularity::File, true),
        11,
    ));
    assert_eq!(unit_queue.len(), 12);
    assert_eq!(file_queue.len(), 12);
    assert_ne!(unit_queue, file_queue);

    // 戻してもエントリはそのまま
    store::set_granularity(&conn, pl, Granularity::Unit, 2).unwrap();
    assert_eq!(store::entries(&conn, pl).unwrap(), before);
}

#[test]
fn t4_same_input_and_seed_give_the_same_queue() {
    let mut lib = Lib::default();
    lib.images(1..=12);
    for id in 1..=12 {
        lib.entry(EntryTarget::Item(id));
    }
    for granularity in [Granularity::File, Granularity::Unit] {
        let opts = QueueOptions {
            weight_by_rating: true,
            max_same_kind_run: 2,
            ..options(granularity, true)
        };
        let a = lib.queue(&opts, 99);
        let b = lib.queue(&opts, 99);
        assert_eq!(a, b);
        assert_ne!(items(&a), items(&lib.queue(&opts, 100)));
    }
}

#[test]
fn t5_folder_entries_follow_folder_contents() {
    let conn = test_conn();
    let ids = insert_items(&conn, 3, "image");
    insert_folder(&conn, 10, None, "写真");
    add_to_folder(&conn, 10, &ids[0..2]);
    let pl = store::create(&conn, &new_playlist(Granularity::File), 0).unwrap();
    store::add_entries(
        &conn,
        pl,
        &[NewEntry::from(EntryTarget::Folder {
            folder_id: 10,
            recursive: true,
        })],
        None,
        0,
    )
    .unwrap();
    let resolved = |conn: &Connection| resolve(&store::load_snapshot(conn, pl).unwrap());
    assert_eq!(resolved(&conn)[0].items.len(), 2);

    add_to_folder(&conn, 10, &ids[2..3]);
    let after = resolved(&conn);
    assert_eq!(after[0].items.len(), 3);
    assert!(after[0].items.contains(&ids[2]));
}

#[test]
fn t6_non_recursive_folder_entries_skip_child_folders() {
    let conn = test_conn();
    let ids = insert_items(&conn, 3, "image");
    insert_folder(&conn, 10, None, "写真");
    insert_folder(&conn, 11, Some(10), "2026 旅行");
    add_to_folder(&conn, 10, &ids[0..1]);
    add_to_folder(&conn, 11, &ids[1..3]);
    let pl = store::create(&conn, &new_playlist(Granularity::File), 0).unwrap();
    store::add_entries(
        &conn,
        pl,
        &[
            NewEntry::from(EntryTarget::Folder {
                folder_id: 10,
                recursive: false,
            }),
            NewEntry::from(EntryTarget::Folder {
                folder_id: 10,
                recursive: true,
            }),
        ],
        None,
        0,
    )
    .unwrap();
    let resolved = resolve(&store::load_snapshot(&conn, pl).unwrap());
    assert_eq!(resolved[0].items, vec![ids[0]]);
    assert_eq!(resolved[1].items.len(), 3);
    assert_eq!(
        resolved[0].unit_key,
        Some(UnitKey::Folder {
            folder_id: 10,
            recursive: false
        })
    );
}

#[test]
fn t7_dedup_only_in_file_granularity() {
    let mut lib = Lib::default();
    lib.images(1..=3)
        .set_unit(A, &[1, 2])
        .set_unit(B, &[2, 3])
        .entry(EntryTarget::Unit(A))
        .entry(EntryTarget::Unit(B));
    assert_eq!(
        items(&lib.queue(&options(Granularity::File, false), 1)),
        vec![1, 2, 3]
    );
    assert_eq!(
        items(&lib.queue(&options(Granularity::Unit, false), 1)),
        vec![1, 2, 2, 3]
    );
    // dedup_items = false ならファイル単位でも両方
    let keep = QueueOptions {
        dedup_items: false,
        ..options(Granularity::File, false)
    };
    assert_eq!(items(&lib.queue(&keep, 1)), vec![1, 2, 2, 3]);
}

#[test]
fn t8_max_same_kind_run_is_respected_when_possible() {
    let mut lib = Lib::default();
    lib.images(1..=6);
    for id in 7..=9 {
        lib.item(id, "audio");
    }
    for id in 10..=12 {
        lib.item(id, "video");
    }
    for id in 1..=12 {
        lib.entry(EntryTarget::Item(id));
    }
    for granularity in [Granularity::File, Granularity::Unit] {
        let opts = QueueOptions {
            max_same_kind_run: 2,
            ..options(granularity, true)
        };
        for seed in 0..200 {
            let queue = lib.queue(&opts, seed);
            assert_eq!(queue.len(), 12);
            let kinds: Vec<&str> = queue
                .iter()
                .map(|s| lib.snap.members.items[&s.item_id].kind.as_str())
                .collect();
            for window in kinds.windows(3) {
                assert!(
                    !(window[0] == window[1] && window[1] == window[2]),
                    "seed {seed}: {kinds:?}"
                );
            }
        }
    }
}

#[test]
fn t9_weighted_shuffle_favors_higher_ratings() {
    let mut lib = Lib::default();
    lib.images(1..=20).rate(1, 5).rate(2, 1);
    for id in 1..=20 {
        lib.entry(EntryTarget::Item(id));
    }
    let opts = QueueOptions {
        weight_by_rating: true,
        ..options(Granularity::File, true)
    };
    let (mut five, mut one) = (0, 0);
    for seed in 0..1000 {
        let queue = items(&lib.queue(&opts, seed));
        let pos = |id: i64| queue.iter().position(|&x| x == id).unwrap_or(usize::MAX);
        five += usize::from(pos(1) < 10);
        one += usize::from(pos(2) < 10);
    }
    // 評価 5（重み 2.0）は前半に来やすく、評価 1（0.6）は来にくい
    assert!(five > 650, "評価 5 が前半に来た割合 {five}/1000");
    assert!(one < 450, "評価 1 が前半に来た割合 {one}/1000");
    assert!(five - one > 250, "{five} と {one}");

    // 最終再生が 7 日以内なら重みが 1/4 になる
    if let Some(info) = lib.snap.members.items.get_mut(&1) {
        info.last_played_at = Some(NOW - DAY);
    }
    let mut recent = 0;
    for seed in 0..1000 {
        let queue = items(&lib.queue(&opts, seed));
        recent += usize::from(queue.iter().position(|&x| x == 1).unwrap_or(99) < 10);
    }
    assert!(recent < five - 200, "{recent} と {five}");
}

#[test]
fn t10_smart_unit_matching_by_unit_match() {
    let night: HashSet<i64> = HashSet::from([100]);
    let facts = |own: &[i64], members: &[&[i64]]| UnitFacts {
        own_tags: own.iter().copied().collect(),
        members: (1..=members.len() as i64).collect(),
        member_tags: members
            .iter()
            .map(|tags| tags.iter().copied().collect())
            .collect(),
        ..UnitFacts::default()
    };
    let own = facts(&[100], &[&[], &[]]);
    let some = facts(&[], &[&[100], &[]]);
    let every = facts(&[], &[&[100], &[100, 7]]);
    let none = facts(&[], &[&[7], &[]]);
    let ctx = QueryContext {
        now_ms: NOW,
        utc_offset_minutes: 540,
    };
    let has = UnitCond::Tags(night);
    let check = |cond: &UnitCond, m: UnitMatch| {
        [&own, &some, &every, &none].map(|f| unit_matches(cond, f, m, &ctx))
    };
    assert_eq!(check(&has, UnitMatch::Any), [true, true, true, false]);
    assert_eq!(check(&has, UnitMatch::All), [true, false, true, false]);
    assert_eq!(
        check(&has, UnitMatch::UnitOnly),
        [true, false, false, false]
    );
    // 「含まない」は「含む」の否定
    let not = UnitCond::Not(Box::new(has.clone()));
    assert_eq!(check(&not, UnitMatch::Any), [false, false, false, true]);
    assert_eq!(check(&not, UnitMatch::All), [false, true, false, true]);
    assert_eq!(check(&not, UnitMatch::UnitOnly), [false, true, true, true]);
}

#[test]
fn t11_trashed_items_and_emptied_units_are_skipped() {
    let mut lib = Lib::default();
    lib.images(1..=6)
        .set_unit(A, &[1, 2])
        .set_unit(B, &[3, 4])
        .entry(EntryTarget::Unit(A))
        .entry(EntryTarget::Item(5))
        .entry(EntryTarget::Unit(B))
        .entry(EntryTarget::Item(6))
        .trash(2)
        .trash(3)
        .trash(4)
        .trash(6);
    let resolved = lib.resolved();
    assert_eq!(
        resolved
            .iter()
            .map(|r| (r.entry_id, r.items.clone()))
            .collect::<Vec<_>>(),
        vec![(1, vec![1]), (2, vec![5])]
    );
    for granularity in [Granularity::File, Granularity::Unit] {
        assert_eq!(
            items(&lib.queue(&options(granularity, false), 1)),
            vec![1, 5]
        );
    }
}

#[test]
fn t12_image_duration_priority() {
    let mut lib = Lib::default();
    lib.images(1..=5)
        .item(6, "audio")
        .set_unit(A, &[1, 2])
        .set_unit(B, &[3])
        .entry(EntryTarget::Unit(A))
        .entry(EntryTarget::Unit(B))
        .entry(EntryTarget::Item(4))
        .entry(EntryTarget::Item(5))
        .entry(EntryTarget::Item(6));
    lib.unit_mut(A).image_duration_ms = Some(5000);
    lib.unit_mut(B).image_duration_ms = Some(5000);
    // エントリの上書き（ユニットより強い）
    lib.snap.entries[1].image_duration_ms = Some(3000);
    lib.snap.entries[3].image_duration_ms = Some(2000);
    let queue = lib.queue(
        &QueueOptions {
            image_duration_ms: 7000,
            ..options(Granularity::File, false)
        },
        1,
    );
    let durations: Vec<Option<i64>> = queue.iter().map(|s| s.image_duration_ms).collect();
    // エントリ → ユニット → プレイリスト。音楽・動画は None
    assert_eq!(
        durations,
        vec![
            Some(5000),
            Some(5000),
            Some(3000),
            Some(7000),
            Some(2000),
            None
        ]
    );
}

#[test]
fn t13_inner_shuffle_does_not_depend_on_unit_order() {
    let build = |order: &[EntryTarget], seed: u64| {
        let mut lib = Lib::default();
        lib.images(1..=20)
            .set_unit(A, &[1, 2, 3, 4, 5, 6, 7, 8])
            .set_unit(B, &[11, 12, 13]);
        lib.unit_mut(A).inner_shuffle = true;
        for target in order {
            lib.entry(*target);
        }
        let queue = items(&lib.queue(&options(Granularity::Unit, true), seed));
        queue.into_iter().filter(|id| *id <= 8).collect::<Vec<_>>()
    };
    let one = [
        EntryTarget::Unit(A),
        EntryTarget::Unit(B),
        EntryTarget::Item(20),
    ];
    let other = [
        EntryTarget::Item(19),
        EntryTarget::Unit(B),
        EntryTarget::Item(20),
        EntryTarget::Unit(A),
    ];
    let mut shuffled = false;
    for seed in 0..20 {
        let a = build(&one, seed);
        assert_eq!(a, build(&other, seed), "seed {seed}");
        assert_eq!(a.len(), 8);
        shuffled |= a != vec![1, 2, 3, 4, 5, 6, 7, 8];
    }
    assert!(shuffled, "ユニット内がシャッフルされていない");
    // シャッフルしないプレイリストでも、ユニット内だけはシャッフルする
    let mut lib = Lib::default();
    lib.images(1..=8)
        .set_unit(A, &[1, 2, 3, 4, 5, 6, 7, 8])
        .entry(EntryTarget::Unit(A));
    lib.unit_mut(A).inner_shuffle = true;
    let plain = (0..10)
        .map(|seed| items(&lib.queue(&options(Granularity::Unit, false), seed)))
        .collect::<Vec<_>>();
    assert!(plain.iter().any(|q| q != &vec![1, 2, 3, 4, 5, 6, 7, 8]));
}

#[test]
fn t14_snapshot_to_files_replaces_unit_and_folder_entries_in_place() {
    let conn = test_conn();
    let ids = insert_items(&conn, 6, "image");
    insert_set_unit(&conn, A, "鎌倉", &ids[1..3]);
    insert_folder(&conn, 10, None, "写真");
    add_to_folder(&conn, 10, &ids[3..5]);
    let pl = store::create(&conn, &new_playlist(Granularity::Unit), 0).unwrap();
    store::add_entries(
        &conn,
        pl,
        &[
            NewEntry::from(EntryTarget::Item(ids[0])),
            NewEntry::from(EntryTarget::Unit(A)),
            NewEntry::from(EntryTarget::Folder {
                folder_id: 10,
                recursive: true,
            }),
            NewEntry::from(EntryTarget::Item(ids[5])),
        ],
        None,
        0,
    )
    .unwrap();
    let before = store::entries(&conn, pl).unwrap();
    let previous = store::snapshot_to_files(&conn, pl, None, 1).unwrap();
    assert_eq!(previous, before);
    let after = store::entries(&conn, pl).unwrap();
    let targets: Vec<EntryTarget> = after.iter().map(|e| e.target).collect();
    let mut expected = vec![EntryTarget::Item(ids[0])];
    expected.extend(ids[1..5].iter().map(|id| EntryTarget::Item(*id)));
    expected.push(EntryTarget::Item(ids[5]));
    // フォルダの中は名前順（i<ID>）
    assert_eq!(targets, expected);
    assert!(after.windows(2).all(|w| w[0].pos < w[1].pos));
}

/// 1 万エントリ・展開後 5 万アイテムで resolve + build_queue が 50ms 以内（§5.6）。
/// `cargo test --release -p kura-core t15 -- --ignored --nocapture`
#[test]
#[ignore]
fn t15_resolve_and_build_queue_are_fast() {
    let mut lib = Lib::default();
    let mut next_item = 1i64;
    let mut take = |n: i64| {
        let ids: Vec<i64> = (next_item..next_item + n).collect();
        next_item += n;
        ids
    };
    let mut plan = Vec::new();
    // 7,500 件の item エントリと、17 件ずつの 2,500 ユニット（計 50,000 アイテム）
    for i in 0..10_000i64 {
        if i % 4 == 3 {
            plan.push((Some(i), take(17)));
        } else {
            plan.push((None, take(1)));
        }
    }
    for (unit, ids) in &plan {
        for id in ids {
            let kind = ["image", "image", "audio", "video"][(*id % 4) as usize];
            lib.item(*id, kind).rate(*id, *id % 6);
        }
        match unit {
            Some(u) => {
                lib.set_unit(*u, ids).entry(EntryTarget::Unit(*u));
            }
            None => {
                lib.entry(EntryTarget::Item(ids[0]));
            }
        }
    }
    assert_eq!(lib.snap.members.items.len(), 50_000);
    for granularity in [Granularity::File, Granularity::Unit] {
        let opts = QueueOptions {
            weight_by_rating: true,
            max_same_kind_run: 2,
            ..options(granularity, true)
        };
        let started = std::time::Instant::now();
        let resolved = resolve(&lib.snap);
        let queue = build_queue(&resolved, &lib.snap, &opts, 42);
        let elapsed = started.elapsed();
        eprintln!(
            "T15 {granularity:?}: {elapsed:?}（{} スロット）",
            queue.len()
        );
        assert_eq!(queue.len(), 50_000);
        if !cfg!(debug_assertions) {
            assert!(elapsed.as_millis() <= 50, "{elapsed:?}");
        }
    }
}

// ===== DB を使うテストの準備 =====

fn new_playlist(granularity: Granularity) -> NewPlaylist {
    NewPlaylist {
        name: "夜の作業".into(),
        kind: PlaylistKind::Manual,
        granularity,
    }
}

/// `n` 件のアイテムを入れて ID を返す（タイトルは i01, i02, …）。
fn insert_items(conn: &Connection, n: i64, kind: &str) -> Vec<i64> {
    (1..=n)
        .map(|i| {
            conn.execute(
                "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                 VALUES (?1, 'jpg', ?2, 1, ?3, ?3, ?4, ?4)",
                params![i.to_le_bytes().to_vec(), kind, format!("i{i:02}"), i],
            )
            .unwrap();
            conn.last_insert_rowid()
        })
        .collect()
}

fn insert_set_unit(conn: &Connection, id: i64, name: &str, members: &[i64]) {
    conn.execute(
        "INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (?1, 'set', ?2, 'manual', 0, 0)",
        params![id, name],
    )
    .unwrap();
    for (pos, item) in members.iter().enumerate() {
        conn.execute(
            "INSERT INTO unit_items (unit_id, item_id, pos) VALUES (?1, ?2, ?3)",
            params![id, item, pos as f64],
        )
        .unwrap();
    }
}

fn insert_folder(conn: &Connection, id: i64, parent: Option<i64>, name: &str) {
    conn.execute(
        "INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (?1, ?2, ?3, ?1, 0)",
        params![id, parent, name],
    )
    .unwrap();
}

fn add_to_folder(conn: &Connection, folder: i64, items: &[i64]) {
    for item in items {
        conn.execute(
            "INSERT INTO item_folders (folder_id, item_id) VALUES (?1, ?2)",
            params![folder, item],
        )
        .unwrap();
    }
}
