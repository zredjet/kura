//! 再生するキューを用意する（docs/playlists.md §5.2・§6.3・§6.5）。
//!
//! - シード：再開なら保存したもの、並べ直すなら新しいもの、シード固定なら保存したもの、それ以外は新しいもの。
//! - スマート：「再生のたびに作り直す」なら並びを作り直してエントリに保存する（並びはエントリの順）。
//! - S（シャッフルの切り替え）・並べ直すは、今のアイテムを先頭にして作り直す。

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};

use super::build::build_queue;
use super::generate::generate;
use super::model::{Entry, EntryTarget, Granularity, PlaylistKind, QueueOptions};
use super::resolve::{Snapshot, expand, resolve};
use super::store::{self, PlayState, PlaylistPatch};
use crate::Result;
use crate::query::QueryContext;
use crate::queue::{Slot, UnitKey, total_ms};
use crate::random::new_seed;

/// 再生の依頼。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayRequest {
    /// 保存した位置から続ける（§6.5）。
    pub resume: bool,
    /// シャッフルを切り替える（S）。None は今の設定のまま。
    pub shuffle: Option<bool>,
    /// 新しいシードで作り直す（並べ直す・リピートで先頭に戻るとき）。
    pub reseed: bool,
    /// このアイテム（のユニット）を先頭にして、そこから始める。
    pub start_item: Option<i64>,
    /// 並びはそのままで、このアイテムのスロットから始める（一覧の「ここから再生」。行のエントリは `from_entry`）。
    /// スマートは表示している並びを使い、作り直さない。キューにない種類（フォント・3D）のアイテムなら、一覧でその行より
    /// 後ろの、最初にキューにあるものから（エントリの行と、その先頭のメンバーの行は、ユニット単位ならエントリの最初のスロット
    /// から。[`entry_start`]。docs/playlists.md §1.1）。
    pub from_item: Option<i64>,
    /// `from_item` を押した一覧の行のエントリ（同じアイテムが複数のエントリにあっても、その行から始める）。None なら
    /// `from_item` を含む最初のエントリの行とみなす。
    pub from_entry: Option<i64>,
}

/// 再生するキュー。
#[derive(Debug, Clone, PartialEq)]
pub struct PlayQueue {
    pub slots: Vec<Slot>,
    pub seed: u64,
    pub granularity: Granularity,
    /// 始めるスロットと、その中の位置（再開のとき）。
    pub start_index: usize,
    pub start_ms: i64,
}

/// 保存したプレイリストのキュー（シード・スマートの並びを保存するので書き込みのトランザクションで呼ぶ）。
pub fn playlist_queue(
    conn: &Connection,
    id: i64,
    request: &PlayRequest,
    ctx: &QueryContext,
) -> Result<PlayQueue> {
    let now = ctx.now_ms;
    let mut playlist = store::get(conn, id)?;
    let toggled = request.shuffle.filter(|s| *s != playlist.shuffle);
    if let Some(shuffle) = toggled {
        store::update(
            conn,
            id,
            &PlaylistPatch {
                shuffle: Some(shuffle),
                ..PlaylistPatch::default()
            },
            now,
        )?;
        playlist.shuffle = shuffle;
    }
    let state = if request.resume {
        store::load_state(conn, id)?
    } else {
        None
    };
    let seed = match (state, playlist.shuffle_seed) {
        (Some(state), _) => state.seed,
        _ if request.reseed || toggled.is_some() => new_seed(),
        (None, Some(seed)) if playlist.seed_locked => seed,
        _ => new_seed(),
    };

    let smart = playlist.kind == PlaylistKind::Smart;
    if smart {
        let entries = store::entries(conn, id)?;
        let regenerate = entries.is_empty()
            || request.reseed
            || toggled.is_some()
            || (playlist.regenerate_on_play && state.is_none() && request.from_item.is_none());
        if regenerate {
            let generated = generate(conn, &playlist, seed, ctx)?;
            let list: Vec<Entry> = generated
                .iter()
                .enumerate()
                .map(|(i, e)| Entry {
                    image_duration_ms: e.image_duration_ms,
                    ..Entry::new(0, (i + 1) as f64, e.target)
                })
                .collect();
            store::replace_entries(conn, id, &list, now)?;
            store::set_seed(conn, id, Some(seed), Some(now))?;
        } else {
            store::set_seed(conn, id, Some(seed), None)?;
        }
    } else {
        store::set_seed(conn, id, Some(seed), None)?;
    }

    let snap = store::load_snapshot(conn, id)?;
    let mut options = playlist.queue_options(now);
    // スマートは並び（シャッフル・連続制限）を作るときに決めてあるので、エントリの順のまま
    if smart {
        options.shuffle = false;
    }
    let slots = build_queue(&resolve(&snap), &snap, &options, seed);
    Ok(finish(
        slots,
        seed,
        playlist.granularity,
        state,
        request,
        &snap,
    ))
}

/// その場のキュー（ライブラリの選択・⌘K の結果）。保存はしない。
pub fn entries_queue(
    conn: &Connection,
    entries: Vec<Entry>,
    options: &QueueOptions,
    request: &PlayRequest,
    seed: Option<u64>,
) -> Result<PlayQueue> {
    let seed = match seed {
        Some(seed) if !request.reseed => seed,
        _ => new_seed(),
    };
    let snap = store::snapshot_of(conn, entries)?;
    let slots = build_queue(&resolve(&snap), &snap, options, seed);
    Ok(finish(
        slots,
        seed,
        options.granularity,
        None,
        request,
        &snap,
    ))
}

/// 選択（アイテムと、まとめて表示したユニット）をエントリにする。ユニットを含めばユニット単位で再生する。
pub fn selection_entries(item_or_units: &[(bool, i64)]) -> (Vec<Entry>, Granularity) {
    let entries: Vec<Entry> = item_or_units
        .iter()
        .enumerate()
        .map(|(i, (is_unit, id))| {
            let target = if *is_unit {
                EntryTarget::Unit(*id)
            } else {
                EntryTarget::Item(*id)
            };
            Entry::new((i + 1) as i64, (i + 1) as f64, target)
        })
        .collect();
    let granularity = if item_or_units.iter().any(|(u, _)| *u) {
        Granularity::Unit
    } else {
        Granularity::File
    };
    (entries, granularity)
}

/// 始める位置を決める：再開ならそのスロット（アイテムが違えば同じアイテムの最初、なければ先頭）、
/// `start_item` ならそのアイテム（ユニット単位ならそのユニット）を先頭に移してそこから、
/// `from_item` なら並びはそのままで、行のエントリ（`from_entry`）のそのアイテムのスロットから（エントリにスロットがなければ
/// そのアイテムの最初のスロット、キューにない種類なら [`entry_start`]）。
fn finish(
    mut slots: Vec<Slot>,
    seed: u64,
    granularity: Granularity,
    state: Option<PlayState>,
    request: &PlayRequest,
    snap: &Snapshot,
) -> PlayQueue {
    let mut start_index = 0;
    let mut start_ms = 0;
    if let Some(state) = state {
        let at = state.slot_index as usize;
        let found = match state.item_id {
            Some(item) if slots.get(at).is_some_and(|s| s.item_id == item) => Some(at),
            Some(item) => slots.iter().position(|s| s.item_id == item),
            None => (at < slots.len()).then_some(at),
        };
        if let Some(found) = found {
            start_index = found;
            start_ms = state.position_ms;
        }
    } else if let Some(item) = request.start_item {
        start_index = put_first(&mut slots, item, granularity);
    } else if let Some(item) = request.from_item {
        // 押した行のエントリのスロット → そのアイテムの最初のスロット（`dedup_items` で後ろの重複を除いたときなど）
        let in_entry = request.from_entry.and_then(|entry| {
            slots
                .iter()
                .position(|s| s.item_id == item && s.entry_id == Some(entry))
        });
        start_index = in_entry
            .or_else(|| slots.iter().position(|s| s.item_id == item))
            .or_else(|| entry_start(&slots, snap, item, request.from_entry, granularity))
            .unwrap_or(0);
    }
    PlayQueue {
        slots,
        seed,
        granularity,
        start_index,
        start_ms,
    }
}

/// キューにない種類（フォント・3D）の `item` から「ここから再生」するときに始めるスロット（docs/playlists.md §1.1）。
/// 行のエントリは `entry`（`item` を含まないか None なら、`item` を含む最初のエントリ（`pos` の順）。M11 の 2 回目の確認 P2：
/// 同じアイテムが 2 つのエントリにあると、後ろのエントリの行でも前のエントリで探していた）。
///
/// - ユニット単位：エントリの行（先頭のメンバーを渡す）と、▸ で開いた中身の先頭のメンバーの行は、そのエントリの最初の
///   スロット。そのエントリが丸ごとキューにない（フォント・3D だけ）なら、その後ろのエントリで最初にキューにあるものの最初の
///   スロット。開いた中身の先頭でないメンバーの行は、そのエントリのメンバーの並び（[`expand`] の順）で `item` より後ろにある、
///   最初にキューにあるメンバーのスロット（なければ後ろのエントリの最初のスロット。2 回目の確認 P1）。
/// - ファイル単位（一覧の行はメンバー 1 件ずつ）：一覧の行の順（エントリの順に、メンバーを [`expand`] の順）で `item` より
///   後ろにある、最初にキューにあるメンバーのスロット（そのエントリの中になければ後ろのエントリへ。M11 の確かめ R4）。
///
/// どれもなければ None（先頭から）。
fn entry_start(
    slots: &[Slot],
    snap: &Snapshot,
    item: i64,
    entry: Option<i64>,
    granularity: Granularity,
) -> Option<usize> {
    let mut entries: Vec<_> = snap.entries.iter().collect();
    entries.sort_by(|a, b| a.pos.total_cmp(&b.pos).then_with(|| a.id.cmp(&b.id)));
    let row_of = |i: usize| {
        let members = expand(snap, entries.get(i)?.target).1;
        let k = members.iter().position(|m| *m == item)?;
        Some((i, k, members))
    };
    let (at, k, members) = entry
        .and_then(|id| entries.iter().position(|e| e.id == id))
        .and_then(row_of)
        .or_else(|| (0..entries.len()).find_map(row_of))?;
    let here = entries.get(at)?.id;
    // 後ろのエントリで最初にキューにあるものの最初のスロット
    let first_of_later = |from: usize| {
        entries
            .get(from..)
            .unwrap_or_default()
            .iter()
            .find_map(|e| slots.iter().position(|s| s.entry_id == Some(e.id)))
    };
    if granularity == Granularity::Unit && k == 0 {
        return first_of_later(at);
    }
    // (エントリ, アイテム) → 最初のスロット（シャッフルしていても、一覧の行のアイテムのスロットから始める）
    let mut first: BTreeMap<(i64, i64), usize> = BTreeMap::new();
    for (i, s) in slots.iter().enumerate() {
        if let Some(entry) = s.entry_id {
            first.entry((entry, s.item_id)).or_insert(i);
        }
    }
    let slot_of = |entry: i64, member: i64| first.get(&(entry, member)).copied();
    let in_entry = members
        .get(k + 1..)
        .unwrap_or_default()
        .iter()
        .find_map(|m| slot_of(here, *m));
    match granularity {
        // ユニットごとに流すので、後ろのエントリはその最初から
        Granularity::Unit => in_entry.or_else(|| first_of_later(at + 1)),
        Granularity::File => in_entry.or_else(|| {
            entries
                .get(at + 1..)
                .unwrap_or_default()
                .iter()
                .find_map(|e| {
                    expand(snap, e.target)
                        .1
                        .into_iter()
                        .find_map(|m| slot_of(e.id, m))
                })
        }),
    }
}

/// `item` のスロット（ユニット単位ならそのグループ）を先頭に移し、始めるスロットの番号を返す。
pub fn put_first(slots: &mut Vec<Slot>, item: i64, granularity: Granularity) -> usize {
    let Some(at) = slots.iter().position(|s| s.item_id == item) else {
        return 0;
    };
    match granularity {
        Granularity::File => {
            let slot = slots.remove(at);
            slots.insert(0, slot);
            0
        }
        Granularity::Unit => {
            let entry = slots[at].entry_id;
            let start = (0..=at)
                .rev()
                .take_while(|&i| slots[i].entry_id == entry)
                .last()
                .unwrap_or(at);
            let end = (at..slots.len())
                .take_while(|&i| slots[i].entry_id == entry)
                .last()
                .map_or(at + 1, |i| i + 1);
            let group: Vec<Slot> = slots.drain(start..end).collect();
            for (i, slot) in group.into_iter().enumerate() {
                slots.insert(i, slot);
            }
            at - start
        }
    }
}

/// キューのまとまり（ユニット単位で再生するときの、ユニット・フォルダ・単体のアイテム）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueGroup {
    pub entry_id: i64,
    pub target: EntryTarget,
    /// ユニット名・フォルダ名・アイテムのタイトル。
    pub name: String,
    pub start: usize,
    pub len: usize,
}

/// 画面に出すキューの情報：まとまり（ユニット単位のときだけ）と、スロットのまとまりの番号、合計の長さ。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueInfo {
    pub groups: Vec<QueueGroup>,
    pub group_index: Vec<usize>,
    pub total_ms: i64,
}

/// キューのまとまりと合計の長さを読む。
pub fn describe(conn: &Connection, queue: &PlayQueue) -> Result<QueueInfo> {
    let mut info = QueueInfo {
        total_ms: total_ms(conn, &queue.slots)?,
        ..QueueInfo::default()
    };
    if queue.granularity != Granularity::Unit {
        return Ok(info);
    }
    let name = |sql: &str, id: i64| -> Result<String> {
        Ok(conn
            .prepare_cached(sql)?
            .query_row([id], |r| r.get::<_, String>(0))
            .optional()?
            .unwrap_or_default())
    };
    for (i, slot) in queue.slots.iter().enumerate() {
        // ユニット単位ではエントリごとに続いて並ぶので、エントリが変わったところで新しいまとまり
        let continues = i > 0 && queue.slots[i - 1].entry_id == slot.entry_id;
        match info.groups.last_mut() {
            Some(last) if continues => last.len += 1,
            _ => {
                let (target, title) = match slot.unit_key {
                    Some(UnitKey::Unit(id)) => (
                        EntryTarget::Unit(id),
                        name("SELECT name FROM units WHERE id = ?1", id)?,
                    ),
                    Some(UnitKey::Folder {
                        folder_id,
                        recursive,
                    }) => (
                        EntryTarget::Folder {
                            folder_id,
                            recursive,
                        },
                        name("SELECT name FROM folders WHERE id = ?1", folder_id)?,
                    ),
                    None => (
                        EntryTarget::Item(slot.item_id),
                        name("SELECT title FROM items WHERE id = ?1", slot.item_id)?,
                    ),
                };
                info.groups.push(QueueGroup {
                    entry_id: slot.entry_id.unwrap_or(-1),
                    target,
                    name: title,
                    start: i,
                    len: 1,
                });
            }
        }
        info.group_index.push(info.groups.len() - 1);
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::UnitKey;

    fn slot(item: i64, entry: i64) -> Slot {
        Slot {
            item_id: item,
            entry_id: Some(entry),
            unit_key: Some(UnitKey::Unit(entry)),
            index_in_unit: 0,
            unit_len: 1,
            image_duration_ms: None,
            start_ms: None,
            end_ms: None,
        }
    }

    fn items(slots: &[Slot]) -> Vec<i64> {
        slots.iter().map(|s| s.item_id).collect()
    }

    #[test]
    fn puts_the_current_item_or_its_unit_first() {
        let base = vec![slot(1, 1), slot(2, 2), slot(3, 2), slot(4, 2), slot(5, 3)];
        let mut file = base.clone();
        assert_eq!(put_first(&mut file, 3, Granularity::File), 0);
        assert_eq!(items(&file), vec![3, 1, 2, 4, 5]);
        let mut unit = base.clone();
        assert_eq!(put_first(&mut unit, 3, Granularity::Unit), 1);
        assert_eq!(items(&unit), vec![2, 3, 4, 1, 5]);
        let mut missing = base;
        assert_eq!(put_first(&mut missing, 9, Granularity::Unit), 0);
    }

    #[test]
    fn resume_finds_the_saved_item() {
        let slots = vec![slot(1, 1), slot(2, 2), slot(3, 3)];
        let state = |slot_index, item_id| PlayState {
            seed: 1,
            slot_index,
            item_id,
            position_ms: 5000,
            updated_at: 0,
        };
        let q = finish(
            slots.clone(),
            1,
            Granularity::File,
            Some(state(1, Some(2))),
            &PlayRequest::default(),
            &Snapshot::default(),
        );
        assert_eq!((q.start_index, q.start_ms), (1, 5000));
        // エントリが変わってスロットがずれていても、同じアイテムから
        let q = finish(
            slots.clone(),
            1,
            Granularity::File,
            Some(state(0, Some(3))),
            &PlayRequest::default(),
            &Snapshot::default(),
        );
        assert_eq!((q.start_index, q.start_ms), (2, 5000));
        // 見つからなければ先頭から
        let none = PlayRequest::default();
        let q = finish(
            slots.clone(),
            1,
            Granularity::File,
            Some(state(1, Some(9))),
            &none,
            &Snapshot::default(),
        );
        assert_eq!((q.start_index, q.start_ms), (0, 0));
        // 一覧の「ここから再生」：並びはそのまま
        let from = PlayRequest {
            from_item: Some(3),
            ..PlayRequest::default()
        };
        let q = finish(
            slots,
            1,
            Granularity::File,
            None,
            &from,
            &Snapshot::default(),
        );
        assert_eq!(q.start_index, 2);
        assert_eq!(q.slots[0].item_id, 1);
    }

    #[test]
    fn selections_with_units_play_by_unit() {
        let (entries, g) = selection_entries(&[(false, 5), (true, 2)]);
        assert_eq!(g, Granularity::Unit);
        assert_eq!(
            entries.iter().map(|e| e.target).collect::<Vec<_>>(),
            vec![EntryTarget::Item(5), EntryTarget::Unit(2)]
        );
        assert_eq!(selection_entries(&[(false, 5)]).1, Granularity::File);
    }
}
