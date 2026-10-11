//! プレイリストの読み書き（docs/data-model.md `playlists` / `playlist_entries` / `playlist_state`）。

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::model::{
    Entry, EntryTarget, Granularity, LimitKind, PlaylistKind, QueueOptions, RepeatMode, UnitMatch,
};
use super::resolve::{Snapshot, UnitData, expand, resolve};
use super::smart::SmartRule;
use crate::items::{ItemSort, SortKey};
use crate::text::nfc;
use crate::unit::{DEFAULT_IMAGE_DURATION_MS, load_member_snapshot, play_info};
use crate::{CoreError, Result};

/// 位置の差がこれより小さくなったら、プレイリスト全体の位置を振り直す（§3.2）。
const MIN_GAP: f64 = 1e-9;

/// プレイリスト（`playlists` の 1 行）。
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: String,
    pub kind: PlaylistKind,
    pub granularity: Granularity,
    pub shuffle: bool,
    pub shuffle_seed: Option<u64>,
    pub seed_locked: bool,
    pub regenerate_on_play: bool,
    pub weight_by_rating: bool,
    pub max_same_kind_run: u32,
    pub dedup_items: bool,
    pub image_duration_ms: i64,
    pub bgm_through_images: bool,
    pub repeat_mode: RepeatMode,
    /// smart の条件（manual は None）。
    pub rule: Option<SmartRule>,
    pub limit: Option<(LimitKind, i64)>,
    pub unit_match: UnitMatch,
    pub include_loose_items: bool,
    /// smart でシャッフルしないときの並び順。
    pub sort: ItemSort,
    /// smart の並びを最後に作った日時。
    pub generated_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Playlist {
    /// キューを作るときの設定。
    pub fn queue_options(&self, now_ms: i64) -> QueueOptions {
        QueueOptions {
            granularity: self.granularity,
            shuffle: self.shuffle,
            weight_by_rating: self.weight_by_rating,
            max_same_kind_run: self.max_same_kind_run,
            dedup_items: self.dedup_items,
            image_duration_ms: self.image_duration_ms,
            now_ms,
        }
    }
}

const COLUMNS: &str = "id, name, kind, granularity, shuffle, shuffle_seed, seed_locked, regenerate_on_play,
     weight_by_rating, max_same_kind_run, dedup_items, image_duration_ms, bgm_through_images, repeat_mode,
     rule_json, limit_kind, limit_value, unit_match, include_loose_items, sort_key, sort_desc, generated_at,
     created_at, updated_at";

fn invalid(what: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("{what} の値が正しくありません: {value}").into(),
    )
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<Playlist> {
    let text = |i: usize| r.get::<_, String>(i);
    let kind = text(2)?;
    let granularity = text(3)?;
    let repeat = text(13)?;
    let rule_json: Option<String> = r.get(14)?;
    let limit_kind: Option<String> = r.get(15)?;
    let limit_value: Option<i64> = r.get(16)?;
    let unit_match = text(17)?;
    let sort_key = text(19)?;
    Ok(Playlist {
        id: r.get(0)?,
        name: text(1)?,
        kind: PlaylistKind::parse(&kind).ok_or_else(|| invalid("kind", &kind))?,
        granularity: Granularity::parse(&granularity)
            .ok_or_else(|| invalid("granularity", &granularity))?,
        shuffle: r.get(4)?,
        shuffle_seed: r.get::<_, Option<i64>>(5)?.map(|s| s as u64),
        seed_locked: r.get(6)?,
        regenerate_on_play: r.get(7)?,
        weight_by_rating: r.get(8)?,
        max_same_kind_run: u32::try_from(r.get::<_, i64>(9)?).unwrap_or(0),
        dedup_items: r.get(10)?,
        image_duration_ms: r.get(11)?,
        bgm_through_images: r.get(12)?,
        repeat_mode: RepeatMode::parse(&repeat).ok_or_else(|| invalid("repeat_mode", &repeat))?,
        // 読めない条件は空の条件として扱う（壊れたまま開けなくならないように）
        rule: rule_json.map(|json| serde_json::from_str(&json).unwrap_or_default()),
        limit: limit_kind
            .and_then(|k| LimitKind::parse(&k))
            .zip(limit_value),
        unit_match: UnitMatch::parse(&unit_match)
            .ok_or_else(|| invalid("unit_match", &unit_match))?,
        include_loose_items: r.get(18)?,
        sort: ItemSort {
            key: SortKey::parse(&sort_key).unwrap_or(SortKey::Imported),
            desc: r.get(20)?,
        },
        generated_at: r.get(21)?,
        created_at: r.get(22)?,
        updated_at: r.get(23)?,
    })
}

/// 1 つのプレイリスト。
pub fn get(conn: &Connection, id: i64) -> Result<Playlist> {
    conn.prepare_cached(&format!("SELECT {COLUMNS} FROM playlists WHERE id = ?1"))?
        .query_row([id], from_row)
        .optional()?
        .ok_or_else(|| CoreError::NotFound(format!("プレイリスト {id}")))
}

/// すべてのプレイリスト（並び順）。
pub fn list(conn: &Connection) -> Result<Vec<Playlist>> {
    let mut stmt =
        conn.prepare_cached(&format!("SELECT {COLUMNS} FROM playlists ORDER BY pos, id"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// サイドバーの並びを変える（`ids` の順にする。含まれないものは後ろに今の順で。ADR-90）。
pub fn reorder(conn: &Connection, ids: &[i64]) -> Result<()> {
    let mut order: Vec<i64> = Vec::with_capacity(ids.len());
    for id in ids {
        if !order.contains(id) {
            order.push(*id);
        }
    }
    for playlist in list(conn)? {
        if !order.contains(&playlist.id) {
            order.push(playlist.id);
        }
    }
    let mut stmt = conn.prepare_cached("UPDATE playlists SET pos = ?2 WHERE id = ?1")?;
    for (i, id) in order.iter().enumerate() {
        stmt.execute(params![id, (i + 1) as f64])?;
    }
    Ok(())
}

/// 新しいプレイリスト。
#[derive(Debug, Clone, PartialEq)]
pub struct NewPlaylist {
    pub name: String,
    pub kind: PlaylistKind,
    pub granularity: Granularity,
}

fn clean_name(name: &str) -> Result<String> {
    let name = nfc(name.trim());
    if name.is_empty() {
        return Err(CoreError::Invalid(
            "プレイリストの名前を入力してください".into(),
        ));
    }
    Ok(name)
}

/// プレイリストを作る。ID を返す。smart は条件が空（すべて）で、上限 50 項目から始める。
pub fn create(conn: &Connection, new: &NewPlaylist, now: i64) -> Result<i64> {
    let name = clean_name(&new.name)?;
    let pos: f64 = conn.query_row("SELECT coalesce(max(pos), 0) + 1 FROM playlists", [], |r| {
        r.get(0)
    })?;
    let smart = new.kind == PlaylistKind::Smart;
    let rule = smart
        .then(|| serde_json::to_string(&SmartRule::default()))
        .transpose()?;
    conn.prepare_cached(
        "INSERT INTO playlists (name, kind, granularity, shuffle, rule_json, limit_kind, limit_value,
                                pos, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
    )?
    .execute(params![
        name,
        new.kind.as_str(),
        new.granularity.as_str(),
        smart,
        rule,
        smart.then_some(LimitKind::Count.as_str()),
        smart.then_some(50i64),
        pos,
        now
    ])?;
    Ok(conn.last_insert_rowid())
}

/// 変える項目（None は変えない）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaylistPatch {
    pub name: Option<String>,
    pub shuffle: Option<bool>,
    pub seed_locked: Option<bool>,
    pub regenerate_on_play: Option<bool>,
    pub weight_by_rating: Option<bool>,
    pub max_same_kind_run: Option<u32>,
    pub dedup_items: Option<bool>,
    pub image_duration_ms: Option<i64>,
    pub bgm_through_images: Option<bool>,
    pub repeat_mode: Option<RepeatMode>,
    pub rule: Option<SmartRule>,
    /// `Some(None)` で上限なし。
    pub limit: Option<Option<(LimitKind, i64)>>,
    pub unit_match: Option<UnitMatch>,
    pub include_loose_items: Option<bool>,
    pub sort: Option<ItemSort>,
}

/// 設定を変える。
pub fn update(conn: &Connection, id: i64, patch: &PlaylistPatch, now: i64) -> Result<()> {
    let current = get(conn, id)?;
    let name = patch
        .name
        .as_deref()
        .map(clean_name)
        .transpose()?
        .unwrap_or(current.name);
    let image_ms = patch
        .image_duration_ms
        .unwrap_or(current.image_duration_ms)
        .clamp(1000, 60_000);
    let rule = match (&patch.rule, current.kind) {
        (Some(rule), PlaylistKind::Smart) => {
            rule.expr()?;
            Some(rule.clone())
        }
        (Some(_), PlaylistKind::Manual) => {
            return Err(CoreError::Invalid(
                "手動のプレイリストには条件を付けられません".into(),
            ));
        }
        (None, _) => current.rule,
    };
    let limit = patch.limit.unwrap_or(current.limit);
    if let Some((_, value)) = limit
        && value <= 0
    {
        return Err(CoreError::Invalid("上限は 1 以上にしてください".into()));
    }
    let sort = patch.sort.unwrap_or(current.sort);
    conn.prepare_cached(
        "UPDATE playlists SET name = ?2, shuffle = ?3, seed_locked = ?4, regenerate_on_play = ?5,
             weight_by_rating = ?6, max_same_kind_run = ?7, dedup_items = ?8, image_duration_ms = ?9,
             bgm_through_images = ?10, repeat_mode = ?11, rule_json = ?12, limit_kind = ?13,
             limit_value = ?14, unit_match = ?15, include_loose_items = ?16, sort_key = ?17,
             sort_desc = ?18, updated_at = ?19
         WHERE id = ?1",
    )?
    .execute(params![
        id,
        name,
        patch.shuffle.unwrap_or(current.shuffle),
        patch.seed_locked.unwrap_or(current.seed_locked),
        patch.regenerate_on_play.unwrap_or(current.regenerate_on_play),
        patch.weight_by_rating.unwrap_or(current.weight_by_rating),
        i64::from(patch.max_same_kind_run.unwrap_or(current.max_same_kind_run)),
        patch.dedup_items.unwrap_or(current.dedup_items),
        image_ms,
        patch.bgm_through_images.unwrap_or(current.bgm_through_images),
        patch.repeat_mode.unwrap_or(current.repeat_mode).as_str(),
        rule.map(|r| serde_json::to_string(&r)).transpose()?,
        limit.map(|(k, _)| k.as_str()),
        limit.map(|(_, v)| v),
        patch.unit_match.unwrap_or(current.unit_match).as_str(),
        patch
            .include_loose_items
            .unwrap_or(current.include_loose_items),
        sort.key.as_str(),
        sort.desc,
        now
    ])?;
    Ok(())
}

/// シードを変える（smart なら生成日時も）。
pub fn set_seed(
    conn: &Connection,
    id: i64,
    seed: Option<u64>,
    generated_at: Option<i64>,
) -> Result<()> {
    conn.prepare_cached(
        "UPDATE playlists SET shuffle_seed = ?2, generated_at = coalesce(?3, generated_at) WHERE id = ?1",
    )?
    .execute(params![id, seed.map(|s| s as i64), generated_at])?;
    Ok(())
}

/// 削除する（エントリ・再開の状態も消える。再生の記録はプレイリストとのつながりだけ外れる）。
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    get(conn, id)?;
    conn.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
    Ok(())
}

/// 単位を切り替える（エントリは変えない。§3.4）。
pub fn set_granularity(
    conn: &Connection,
    id: i64,
    granularity: Granularity,
    now: i64,
) -> Result<()> {
    get(conn, id)?;
    conn.prepare_cached("UPDATE playlists SET granularity = ?2, updated_at = ?3 WHERE id = ?1")?
        .execute(params![id, granularity.as_str(), now])?;
    Ok(())
}

// ===== エントリ =====

fn entry_from_row(r: &Row<'_>) -> rusqlite::Result<Entry> {
    let entry_type: String = r.get(2)?;
    let target = match entry_type.as_str() {
        "item" => EntryTarget::Item(r.get(3)?),
        "unit" => EntryTarget::Unit(r.get(4)?),
        "folder" => EntryTarget::Folder {
            folder_id: r.get(5)?,
            recursive: r.get(6)?,
        },
        other => return Err(invalid("entry_type", other)),
    };
    Ok(Entry {
        id: r.get(0)?,
        pos: r.get(1)?,
        target,
        image_duration_ms: r.get(7)?,
        start_ms: r.get(8)?,
        end_ms: r.get(9)?,
    })
}

/// エントリ（並び順）。
pub fn entries(conn: &Connection, id: i64) -> Result<Vec<Entry>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, pos, entry_type, item_id, unit_id, folder_id, recursive, image_duration_ms, start_ms, end_ms
         FROM playlist_entries WHERE playlist_id = ?1 ORDER BY pos, id",
    )?;
    let rows = stmt.query_map([id], entry_from_row)?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 追加するエントリ。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NewEntry {
    pub target: EntryTarget,
    pub image_duration_ms: Option<i64>,
}

impl From<EntryTarget> for NewEntry {
    fn from(target: EntryTarget) -> Self {
        Self {
            target,
            image_duration_ms: None,
        }
    }
}

fn insert_entry(
    conn: &Connection,
    playlist: i64,
    id: Option<i64>,
    pos: f64,
    target: EntryTarget,
    image_duration_ms: Option<i64>,
    trims: (Option<i64>, Option<i64>),
) -> Result<i64> {
    let (entry_type, item, unit, folder, recursive) = match target {
        EntryTarget::Item(i) => ("item", Some(i), None, None, true),
        EntryTarget::Unit(u) => ("unit", None, Some(u), None, true),
        EntryTarget::Folder {
            folder_id,
            recursive,
        } => ("folder", None, None, Some(folder_id), recursive),
    };
    conn.prepare_cached(
        "INSERT INTO playlist_entries (id, playlist_id, pos, entry_type, item_id, unit_id, folder_id, recursive,
                                       image_duration_ms, start_ms, end_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        id,
        playlist,
        pos,
        entry_type,
        item,
        unit,
        folder,
        recursive,
        image_duration_ms,
        trims.0,
        trims.1
    ])?;
    Ok(conn.last_insert_rowid())
}

/// 位置を 1, 2, 3… に振り直す。
fn renumber(conn: &Connection, id: i64) -> Result<()> {
    let mut update = conn.prepare_cached("UPDATE playlist_entries SET pos = ?2 WHERE id = ?1")?;
    for (i, entry) in entries(conn, id)?.iter().enumerate() {
        update.execute(params![entry.id, (i + 1) as f64])?;
    }
    Ok(())
}

/// `before`（エントリ ID。None なら末尾）の前に `n` 個入れるときの位置。間が狭すぎれば振り直してから求める。
fn slots_before(conn: &Connection, id: i64, before: Option<i64>, n: usize) -> Result<Vec<f64>> {
    for attempt in 0..2 {
        let list = entries(conn, id)?;
        let (lo, hi) = match before.and_then(|b| list.iter().position(|e| e.id == b)) {
            Some(i) => (
                if i == 0 {
                    list[0].pos - 1.0
                } else {
                    list[i - 1].pos
                },
                list[i].pos,
            ),
            None => {
                let last = list.last().map_or(0.0, |e| e.pos);
                (last, last + n as f64 + 1.0)
            }
        };
        let step = (hi - lo) / (n as f64 + 1.0);
        if step >= MIN_GAP || attempt == 1 {
            return Ok((1..=n).map(|k| lo + step * k as f64).collect());
        }
        renumber(conn, id)?;
    }
    Ok(Vec::new())
}

fn touch(conn: &Connection, id: i64, now: i64) -> Result<()> {
    conn.prepare_cached("UPDATE playlists SET updated_at = ?2 WHERE id = ?1")?
        .execute(params![id, now])?;
    Ok(())
}

/// エントリを `before`（エントリ ID）の前に、なければ末尾に追加する。追加したエントリの ID を返す。
pub fn add_entries(
    conn: &Connection,
    id: i64,
    new: &[NewEntry],
    before: Option<i64>,
    now: i64,
) -> Result<Vec<i64>> {
    get(conn, id)?;
    let positions = slots_before(conn, id, before, new.len())?;
    let mut ids = Vec::with_capacity(new.len());
    for (entry, pos) in new.iter().zip(positions) {
        ids.push(insert_entry(
            conn,
            id,
            None,
            pos,
            entry.target,
            entry.image_duration_ms,
            (None, None),
        )?);
    }
    touch(conn, id, now)?;
    Ok(ids)
}

/// エントリを `before` の前（None なら末尾）へ移す。移すもの同士の順は保つ。
pub fn move_entries(
    conn: &Connection,
    id: i64,
    entry_ids: &[i64],
    before: Option<i64>,
    now: i64,
) -> Result<()> {
    let moving: HashSet<i64> = entry_ids.iter().copied().collect();
    if before.is_some_and(|b| moving.contains(&b)) {
        return Ok(());
    }
    let ordered: Vec<i64> = entries(conn, id)?
        .into_iter()
        .map(|e| e.id)
        .filter(|e| moving.contains(e))
        .collect();
    let positions = slots_before(conn, id, before, ordered.len())?;
    let mut update = conn.prepare_cached(
        "UPDATE playlist_entries SET pos = ?3 WHERE id = ?1 AND playlist_id = ?2",
    )?;
    for (entry, pos) in ordered.iter().zip(positions) {
        update.execute(params![entry, id, pos])?;
    }
    touch(conn, id, now)
}

/// エントリを外す。
pub fn remove_entries(conn: &Connection, id: i64, entry_ids: &[i64], now: i64) -> Result<()> {
    let mut delete =
        conn.prepare_cached("DELETE FROM playlist_entries WHERE id = ?1 AND playlist_id = ?2")?;
    for entry in entry_ids {
        delete.execute(params![entry, id])?;
    }
    touch(conn, id, now)
}

/// エントリをすべて置き換える（「ファイルに固定」の取り消し、スマートの生成）。ID・位置もそのまま入れる。
pub fn replace_entries(conn: &Connection, id: i64, list: &[Entry], now: i64) -> Result<()> {
    conn.prepare_cached("DELETE FROM playlist_entries WHERE playlist_id = ?1")?
        .execute([id])?;
    for entry in list {
        insert_entry(
            conn,
            id,
            (entry.id > 0).then_some(entry.id),
            entry.pos,
            entry.target,
            entry.image_duration_ms,
            (entry.start_ms, entry.end_ms),
        )?;
    }
    touch(conn, id, now)
}

/// 前のエントリに戻す（「ファイルに固定」の取り消し）。消えたアイテム・ユニット・フォルダを指すものは戻さない。
pub fn restore_entries(conn: &Connection, id: i64, list: &[Entry], now: i64) -> Result<()> {
    let exists = |sql: &str, key: i64| -> Result<bool> {
        Ok(conn.prepare_cached(sql)?.query_row([key], |r| r.get(0))?)
    };
    let mut kept = Vec::with_capacity(list.len());
    for entry in list {
        let ok = match entry.target {
            EntryTarget::Item(i) => exists("SELECT EXISTS (SELECT 1 FROM items WHERE id = ?1)", i)?,
            EntryTarget::Unit(u) => exists("SELECT EXISTS (SELECT 1 FROM units WHERE id = ?1)", u)?,
            EntryTarget::Folder { folder_id, .. } => exists(
                "SELECT EXISTS (SELECT 1 FROM folders WHERE id = ?1)",
                folder_id,
            )?,
        };
        if ok {
            kept.push(entry.clone());
        }
    }
    replace_entries(conn, id, &kept, now)
}

/// 「ファイルに固定」：unit / folder エントリ（`entry_ids` を省けば全部）を、その時点のメンバーの item エントリに
/// その場で置き換える。画像の表示時間はエントリの上書き → ユニットの設定を引き継ぐ。
/// 取り消しのために、置き換える前のエントリを返す（§3.4・T14）。
pub fn snapshot_to_files(
    conn: &Connection,
    id: i64,
    entry_ids: Option<&[i64]>,
    now: i64,
) -> Result<Vec<Entry>> {
    let snap = load_snapshot(conn, id)?;
    let previous = snap.entries.clone();
    let wanted: Option<HashSet<i64>> = entry_ids.map(|ids| ids.iter().copied().collect());
    let targets: Vec<usize> = previous
        .iter()
        .enumerate()
        .filter(|(_, e)| !matches!(e.target, EntryTarget::Item(_)))
        .filter(|(_, e)| wanted.as_ref().is_none_or(|w| w.contains(&e.id)))
        .map(|(i, _)| i)
        .collect();
    for i in targets {
        let entry = &previous[i];
        let (_, items) = expand(&snap, entry.target);
        let next_pos = previous.get(i + 1).map_or(entry.pos + 1.0, |e| e.pos);
        let step = (next_pos - entry.pos) / (items.len().max(1) as f64);
        let unit_ms = match entry.target {
            EntryTarget::Unit(u) => snap.units.get(&u).and_then(|d| d.image_duration_ms),
            _ => None,
        };
        let image_ms = entry.image_duration_ms.or(unit_ms);
        conn.prepare_cached("DELETE FROM playlist_entries WHERE id = ?1")?
            .execute([entry.id])?;
        for (k, item) in items.iter().enumerate() {
            let is_image = snap
                .members
                .items
                .get(item)
                .is_some_and(|m| m.kind == "image");
            insert_entry(
                conn,
                id,
                None,
                entry.pos + step * k as f64,
                EntryTarget::Item(*item),
                image_ms.filter(|_| is_image),
                (None, None),
            )?;
        }
    }
    // 間が詰まりすぎていれば振り直す
    let after = entries(conn, id)?;
    if after.windows(2).any(|w| w[1].pos - w[0].pos < MIN_GAP) {
        renumber(conn, id)?;
    }
    touch(conn, id, now)?;
    Ok(previous)
}

// ===== スナップショット =====

/// エントリの列から、キューを作るのに要るデータを読む（その場のキューにも使う）。
pub fn snapshot_of(conn: &Connection, list: Vec<Entry>) -> Result<Snapshot> {
    let mut item_ids: HashSet<i64> = HashSet::new();
    let mut folders: Vec<(i64, bool)> = Vec::new();
    let mut units: HashMap<i64, UnitData> = HashMap::new();
    for entry in &list {
        match entry.target {
            EntryTarget::Item(item) => {
                item_ids.insert(item);
            }
            EntryTarget::Folder {
                folder_id,
                recursive,
            } => folders.push((folder_id, recursive)),
            EntryTarget::Unit(unit) => {
                if units.contains_key(&unit) {
                    continue;
                }
                if let Some(info) = play_info(conn, unit)? {
                    item_ids.extend(info.def.items.iter().map(|(i, _)| *i));
                    folders.extend(info.def.folders.iter().map(|f| (f.folder_id, f.recursive)));
                    units.insert(
                        unit,
                        UnitData {
                            def: info.def,
                            name: info.name,
                            image_duration_ms: info.image_duration_ms,
                            inner_shuffle: info.inner_shuffle,
                            rating: info.rating,
                            last_played_at: info.last_played_at,
                        },
                    );
                }
            }
        }
    }
    Ok(Snapshot {
        entries: list,
        units,
        members: load_member_snapshot(conn, item_ids, &folders)?,
    })
}

/// キューを作るのに要るデータを読む。
pub fn load_snapshot(conn: &Connection, id: i64) -> Result<Snapshot> {
    get(conn, id)?;
    snapshot_of(conn, entries(conn, id)?)
}

/// 展開したアイテムの数（サイドバー・見出しの件数）。
pub fn item_count(conn: &Connection, id: i64) -> Result<usize> {
    let snap = load_snapshot(conn, id)?;
    Ok(resolve(&snap).iter().map(|r| r.items.len()).sum())
}

// ===== 再開（§6.5） =====

/// 再開の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayState {
    pub seed: u64,
    pub slot_index: u32,
    /// そのスロットのアイテム（エントリが変わっていたら先頭から）。
    pub item_id: Option<i64>,
    pub position_ms: i64,
    pub updated_at: i64,
}

/// 再開の状態を保存する。
pub fn save_state(conn: &Connection, id: i64, state: &PlayState) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO playlist_state (playlist_id, seed, slot_index, item_id, position_ms, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(playlist_id) DO UPDATE SET seed = excluded.seed, slot_index = excluded.slot_index,
             item_id = excluded.item_id, position_ms = excluded.position_ms, updated_at = excluded.updated_at",
    )?
    .execute(params![
        id,
        state.seed as i64,
        i64::from(state.slot_index),
        state.item_id,
        state.position_ms.max(0),
        state.updated_at
    ])?;
    Ok(())
}

/// 再開の状態（なければ None）。
pub fn load_state(conn: &Connection, id: i64) -> Result<Option<PlayState>> {
    Ok(conn
        .prepare_cached(
            "SELECT seed, slot_index, item_id, position_ms, updated_at FROM playlist_state WHERE playlist_id = ?1",
        )?
        .query_row([id], |r| {
            Ok(PlayState {
                seed: r.get::<_, i64>(0)? as u64,
                slot_index: u32::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                item_id: r.get(2)?,
                position_ms: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })
        .optional()?)
}

/// 再開の状態を消す（最後まで再生したとき）。
pub fn clear_state(conn: &Connection, id: i64) -> Result<()> {
    conn.prepare_cached("DELETE FROM playlist_state WHERE playlist_id = ?1")?
        .execute([id])?;
    Ok(())
}

/// 画像の表示時間の既定値（その場のキュー用）。
pub fn default_image_ms() -> i64 {
    DEFAULT_IMAGE_DURATION_MS
}

// ===== 画面に出す中身 =====

/// エントリ 1 行の表示用の情報（ユニット単位の一覧、スマートの「今回の並び」）。
#[derive(Debug, Clone, PartialEq)]
pub struct EntryRow {
    pub entry: Entry,
    /// ユニット名・フォルダ名・アイテムのタイトル。
    pub name: String,
    /// 展開したアイテム（ゴミ箱を除く）。
    pub items: Vec<i64>,
    /// 合計時間（画像は表示時間）。
    pub duration_ms: i64,
    /// メンバーの種類（image → video → audio の順）。
    pub kinds: Vec<&'static str>,
    /// 種類ごとの件数（画像・動画・音楽）。
    pub kind_counts: [u32; 3],
}

/// プレイリストの中身：エントリごとの行（空になったものも含む）。
pub fn contents(conn: &Connection, id: i64) -> Result<Vec<EntryRow>> {
    let playlist = get(conn, id)?;
    let snap = load_snapshot(conn, id)?;
    let mut folder_name = conn.prepare_cached("SELECT name FROM folders WHERE id = ?1")?;
    let mut rows = Vec::with_capacity(snap.entries.len());
    for entry in &snap.entries {
        let (_, items) = expand(&snap, entry.target);
        let unit = match entry.target {
            EntryTarget::Unit(u) => snap.units.get(&u),
            _ => None,
        };
        let name = match entry.target {
            EntryTarget::Item(item) => snap
                .members
                .items
                .get(&item)
                .map(|m| m.title.clone())
                .unwrap_or_default(),
            EntryTarget::Unit(_) => unit.map(|u| u.name.clone()).unwrap_or_default(),
            EntryTarget::Folder { folder_id, .. } => folder_name
                .query_row([folder_id], |r| r.get::<_, String>(0))
                .optional()?
                .unwrap_or_default(),
        };
        let image_ms = entry
            .image_duration_ms
            .or(unit.and_then(|u| u.image_duration_ms))
            .unwrap_or(playlist.image_duration_ms);
        let mut counts = [0u32; 3];
        let mut duration_ms = 0;
        for info in items.iter().filter_map(|i| snap.members.items.get(i)) {
            match info.kind.as_str() {
                "image" => {
                    counts[0] += 1;
                    duration_ms += image_ms;
                }
                "video" => {
                    counts[1] += 1;
                    duration_ms += info.duration_ms.unwrap_or(0);
                }
                _ => {
                    counts[2] += 1;
                    duration_ms += info.duration_ms.unwrap_or(0);
                }
            }
        }
        let kinds = ["image", "video", "audio"]
            .into_iter()
            .zip(counts)
            .filter_map(|(k, n)| (n > 0).then_some(k))
            .collect();
        rows.push(EntryRow {
            entry: entry.clone(),
            name,
            items,
            duration_ms,
            kinds,
            kind_counts: counts,
        });
    }
    Ok(rows)
}
