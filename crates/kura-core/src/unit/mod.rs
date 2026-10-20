//! ユニット（管理単位。docs/playlists.md §2）：作成・編集・メンバーの変更・解除と、表示用の要約。
//! メンバーの決め方は `resolve`（純粋関数）にあり、ここでは DB から必要なデータを読んで渡す。

mod resolve;

pub use resolve::{
    MemberInfo, MemberSnapshot, MemberStats, OrderMode, UnitDef, UnitFolder, UnitKind, aggregate,
    member_duration_ms, resolve_members,
};

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};

use crate::asset::{self, Alpha, AnalysisState};
use crate::folders;
use crate::import::MediaKind;
use crate::items::aspect_code;
use crate::library::hex;
use crate::tags::{TagRef, display_order};
use crate::text::nfc;
use crate::{CoreError, Result};

/// 画像の表示時間の既定値（設定画面は M9。docs/playlists.md §5.3）。
pub const DEFAULT_IMAGE_DURATION_MS: i64 = 8000;

/// 一度に要約を取れる件数。
pub const MAX_SUMMARIES: usize = 200;

/// グリッドの一覧でユニットを表す ID の印（`0x8000_0000 | unit_id`。docs/architecture.md §3）。
pub const LIST_FLAG: u32 = 0x8000_0000;

/// 所属するユニット。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitRef {
    pub id: i64,
    pub name: String,
    pub kind: UnitKind,
}

/// units の 1 行。
#[derive(Debug, Clone, PartialEq)]
struct UnitRow {
    id: i64,
    kind: UnitKind,
    name: String,
    cover_item_id: Option<i64>,
    order_mode: OrderMode,
    order_reverse: bool,
    inner_shuffle: bool,
    image_duration_ms: Option<i64>,
    rating: i64,
    note: Option<String>,
    origin: String,
    source_url: Option<String>,
    created_at: i64,
    play_count: i64,
    last_played_at: Option<i64>,
}

fn load_row(conn: &Connection, id: i64) -> Result<UnitRow> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, kind, name, cover_item_id, order_mode, inner_shuffle, image_duration_ms, rating, note,
                origin, source_url, created_at, play_count, last_played_at, order_reverse
         FROM units WHERE id = ?1",
    )?;
    let row = stmt
        .query_row([id], |row| {
            let kind: String = row.get(1)?;
            let order: String = row.get(4)?;
            Ok((
                UnitKind::parse(&kind),
                OrderMode::parse(&order),
                UnitRow {
                    id: row.get(0)?,
                    kind: UnitKind::Set,
                    name: row.get(2)?,
                    cover_item_id: row.get(3)?,
                    order_mode: OrderMode::Manual,
                    order_reverse: row.get::<_, i64>(14)? != 0,
                    inner_shuffle: row.get::<_, i64>(5)? != 0,
                    image_duration_ms: row.get(6)?,
                    rating: row.get(7)?,
                    note: row.get(8)?,
                    origin: row.get(9)?,
                    source_url: row.get(10)?,
                    created_at: row.get(11)?,
                    play_count: row.get(12)?,
                    last_played_at: row.get(13)?,
                },
            ))
        })
        .optional()?;
    let Some((Some(kind), Some(order_mode), row)) = row else {
        return Err(CoreError::NotFound(format!("ユニット {id}")));
    };
    Ok(UnitRow {
        kind,
        order_mode,
        ..row
    })
}

fn load_def(conn: &Connection, row: &UnitRow) -> Result<UnitDef> {
    let mut def = UnitDef {
        kind: row.kind,
        order_mode: row.order_mode,
        reverse: row.order_reverse,
        items: Vec::new(),
        folders: Vec::new(),
    };
    match row.kind {
        UnitKind::Set => {
            let mut stmt =
                conn.prepare_cached("SELECT item_id, pos FROM unit_items WHERE unit_id = ?1")?;
            let rows = stmt.query_map([row.id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            def.items = rows.collect::<std::result::Result<_, _>>()?;
        }
        UnitKind::Folder => {
            let mut stmt = conn.prepare_cached(
                "SELECT folder_id, recursive, pos FROM unit_folders WHERE unit_id = ?1",
            )?;
            let rows = stmt.query_map([row.id], |r| {
                Ok(UnitFolder {
                    folder_id: r.get(0)?,
                    recursive: r.get::<_, i64>(1)? != 0,
                    pos: r.get(2)?,
                })
            })?;
            def.folders = rows.collect::<std::result::Result<_, _>>()?;
        }
    }
    Ok(def)
}

/// プレイリストのキューを作るのに要るユニットの情報（docs/playlists.md §5）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlayInfo {
    pub def: UnitDef,
    pub name: String,
    pub image_duration_ms: Option<i64>,
    pub inner_shuffle: bool,
    pub rating: i64,
    pub last_played_at: Option<i64>,
    pub play_count: i64,
    pub created_at: i64,
    pub source_url: Option<String>,
    pub note: Option<String>,
}

/// ユニットの再生用の情報。なければ None。
pub(crate) fn play_info(conn: &Connection, id: i64) -> Result<Option<PlayInfo>> {
    let row = match load_row(conn, id) {
        Ok(row) => row,
        Err(CoreError::NotFound(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    let def = load_def(conn, &row)?;
    Ok(Some(PlayInfo {
        def,
        name: row.name,
        image_duration_ms: row.image_duration_ms,
        inner_shuffle: row.inner_shuffle,
        rating: row.rating,
        last_played_at: row.last_played_at,
        play_count: row.play_count,
        created_at: row.created_at,
        source_url: row.source_url,
        note: row.note,
    }))
}

/// すべてのユニットの ID（作成順）。
pub(crate) fn all_ids(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached("SELECT id FROM units ORDER BY id")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// ユニット自体のタグ。
pub(crate) fn own_tag_ids(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare_cached("SELECT tag_id FROM unit_tags WHERE unit_id = ?1")?;
    let rows = stmt.query_map([id], |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// いくつかのユニットの解決に必要なデータを読む。
fn load_snapshot(conn: &Connection, defs: &[&UnitDef]) -> Result<MemberSnapshot> {
    let mut item_ids: HashSet<i64> = HashSet::new();
    let mut folders: Vec<(i64, bool)> = Vec::new();
    for def in defs {
        item_ids.extend(def.items.iter().map(|(id, _)| *id));
        folders.extend(def.folders.iter().map(|r| (r.folder_id, r.recursive)));
    }
    load_member_snapshot(conn, item_ids, &folders)
}

/// アイテムとフォルダ（`(folder_id, recursive)`）の中身を解決するためのデータを読む（プレイリストでも使う）。
pub(crate) fn load_member_snapshot(
    conn: &Connection,
    mut item_ids: HashSet<i64>,
    folders: &[(i64, bool)],
) -> Result<MemberSnapshot> {
    let mut snap = MemberSnapshot::default();
    if !folders.is_empty() {
        let mut stmt = conn.prepare_cached("SELECT id, parent_id FROM folders")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
        })?;
        for row in rows {
            let (id, parent) = row?;
            if let Some(parent) = parent {
                snap.children.entry(parent).or_default().push(id);
            }
        }
        // 参照フォルダ（recursive なら子孫も）の中身だけを読む。たどるのは参照ごと、読むのは 1 回ずつ
        let mut wanted: Vec<i64> = Vec::new();
        let mut loaded: HashSet<i64> = HashSet::new();
        for &(folder_id, recursive) in folders {
            let mut visited: HashSet<i64> = HashSet::new();
            let mut stack = vec![folder_id];
            while let Some(f) = stack.pop() {
                if !visited.insert(f) {
                    continue;
                }
                if loaded.insert(f) {
                    wanted.push(f);
                }
                if recursive {
                    stack.extend(snap.children.get(&f).into_iter().flatten().copied());
                }
            }
        }
        let mut stmt =
            conn.prepare_cached("SELECT item_id FROM item_folders WHERE folder_id = ?1")?;
        for folder in wanted {
            let rows = stmt.query_map([folder], |r| r.get::<_, i64>(0))?;
            let ids = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            item_ids.extend(ids.iter().copied());
            snap.folder_items.insert(folder, ids);
        }
    }
    let mut stmt = conn.prepare_cached(
        "SELECT title, kind, imported_at, captured_at, duration_ms, rating, last_played_at,
                trashed_at IS NOT NULL
         FROM items WHERE id = ?1",
    )?;
    for id in item_ids {
        let info = stmt
            .query_row([id], |r| {
                Ok(MemberInfo {
                    title: r.get(0)?,
                    kind: r.get(1)?,
                    imported_at: r.get(2)?,
                    captured_at: r.get(3)?,
                    duration_ms: r.get(4)?,
                    rating: r.get(5)?,
                    last_played_at: r.get(6)?,
                    trashed: r.get(7)?,
                })
            })
            .optional()?;
        if let Some(info) = info {
            snap.items.insert(id, info);
        }
    }
    Ok(snap)
}

/// ユニットのメンバーを並び順で返す（ゴミ箱は除く）。
pub fn members(conn: &Connection, id: i64) -> Result<Vec<i64>> {
    let row = load_row(conn, id)?;
    let def = load_def(conn, &row)?;
    let snap = load_snapshot(conn, &[&def])?;
    Ok(resolve_members(&def, &snap))
}

/// 表紙（グリッド・インスペクタに出す画像）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub item_id: i64,
    pub hash: String,
    pub kind: MediaKind,
    pub thumb_state: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    /// サムネイルを作り直した回数（URL の `?r=`。アイテムの概要と同じ）。
    pub thumb_rev: i64,
    /// 解析の進み具合（届いたら表紙を取り直す）。
    pub analysis_state: AnalysisState,
    /// 解析で分かった透過（市松模様を敷くか）。
    pub alpha: Option<Alpha>,
    /// ドット絵として表示するか（実効値。アイテムの概要と同じ規則）。
    pub pixel_art: bool,
}

/// グリッドのタイル・一覧に出すユニットの要約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSummary {
    pub id: i64,
    pub kind: UnitKind,
    pub name: String,
    pub cover: Option<Cover>,
    pub stats: MemberStats,
    /// ユニット自体の評価（0 は未評価）。`stats.rating` は集計値。
    pub own_rating: i64,
    pub origin: String,
    pub created_at: i64,
    pub last_played_at: Option<i64>,
}

/// 表紙の問い合わせ（アイテムの概要と同じく、事実は台帳の行から、設定はアイテムの主キーから引く）。
const COVER_SQL: &str =
    "SELECT i.hash, i.kind, i.thumb_state, i.width, i.height, i.thumb_rev, i.analysis_state, f.alpha,
            f.pixel_art, s.pixel_art
     FROM items i
     LEFT JOIN blobs b ON b.hash = i.hash
     LEFT JOIN media_facts f ON f.blob_id = b.id
     LEFT JOIN asset_settings s ON s.item_id = i.id
     WHERE i.id = ?1";

fn cover_of(conn: &Connection, row: &UnitRow, members: &[i64]) -> Result<Option<Cover>> {
    // 表紙に選んだものがメンバーにいなければ（ゴミ箱など）先頭のメンバー。
    // 逆順でも表紙は変えない（逆にする前の並びの先頭）
    let first = if row.order_reverse {
        members.last()
    } else {
        members.first()
    };
    let id = row
        .cover_item_id
        .filter(|c| members.contains(c))
        .or_else(|| first.copied());
    let Some(id) = id else {
        return Ok(None);
    };
    let mut stmt = conn.prepare_cached(COVER_SQL)?;
    Ok(stmt
        .query_row([id], |r| {
            let kind: MediaKind = r.get(1)?;
            // ドット絵の実効値はアイテムの概要と同じ関数で決める（表紙はコマの数を使わないので格子は渡さない）
            let effective = asset::effective(
                kind,
                (None, None),
                asset::EffectiveInputs {
                    auto_pixel_art: r.get::<_, Option<i64>>(8)?.map(|v| v != 0),
                    pixel_art_setting: r.get::<_, Option<i64>>(9)?.map(|v| v != 0),
                    ..asset::EffectiveInputs::default()
                },
            );
            Ok(Cover {
                item_id: id,
                hash: hex(&r.get::<_, Vec<u8>>(0)?),
                kind,
                thumb_state: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                thumb_rev: r.get(5)?,
                analysis_state: AnalysisState::parse(&r.get::<_, String>(6)?),
                alpha: r.get::<_, Option<i64>>(7)?.and_then(Alpha::from_code),
                pixel_art: effective.pixel_art,
            })
        })
        .optional()?)
}

fn summary_of(conn: &Connection, row: &UnitRow) -> Result<(UnitSummary, Vec<i64>)> {
    let def = load_def(conn, row)?;
    let snap = load_snapshot(conn, &[&def])?;
    let members = resolve_members(&def, &snap);
    let image_ms = row.image_duration_ms.unwrap_or(DEFAULT_IMAGE_DURATION_MS);
    let stats = aggregate(&members, &snap, row.rating, image_ms);
    let summary = UnitSummary {
        id: row.id,
        kind: row.kind,
        name: row.name.clone(),
        cover: cover_of(conn, row, &members)?,
        stats,
        own_rating: row.rating,
        origin: row.origin.clone(),
        created_at: row.created_at,
        last_played_at: row.last_played_at,
    };
    Ok((summary, members))
}

/// 見えている分のユニットの要約を、渡した ID の順で返す（見つからない ID は飛ばす）。
pub fn summaries(conn: &Connection, ids: &[i64]) -> Result<Vec<UnitSummary>> {
    if ids.len() > MAX_SUMMARIES {
        return Err(CoreError::Invalid(format!(
            "一度に取れるのは {MAX_SUMMARIES} 件までです（{} 件）",
            ids.len()
        )));
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        match load_row(conn, *id) {
            Ok(row) => out.push(summary_of(conn, &row)?.0),
            Err(CoreError::NotFound(_)) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(out)
}

/// folder ユニットの参照フォルダ（インスペクタ用にパス付き）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitFolderInfo {
    pub folder_id: i64,
    pub path: Vec<String>,
    pub recursive: bool,
}

/// ユニットのインスペクタ用の詳細。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitDetail {
    pub summary: UnitSummary,
    pub order_mode: OrderMode,
    pub order_reverse: bool,
    pub inner_shuffle: bool,
    pub image_duration_ms: Option<i64>,
    pub note: Option<String>,
    pub source_url: Option<String>,
    pub tags: Vec<TagRef>,
    pub folders: Vec<UnitFolderInfo>,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

/// 1 つのユニットの詳細。
pub fn detail(conn: &Connection, id: i64) -> Result<UnitDetail> {
    let row = load_row(conn, id)?;
    let (summary, _) = summary_of(conn, &row)?;
    let mut tags = {
        let mut stmt = conn.prepare_cached(
            "SELECT t.id, t.namespace, t.name, t.color FROM unit_tags m JOIN tags t ON t.id = m.tag_id
             WHERE m.unit_id = ?1",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok(TagRef {
                id: r.get(0)?,
                namespace: r.get(1)?,
                name: r.get(2)?,
                color: r.get(3)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    tags.sort_by(display_order);
    let folders = if row.kind == UnitKind::Folder {
        let tree = folders::tree(conn)?;
        let mut refs = load_def(conn, &row)?.folders;
        refs.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        refs.into_iter()
            .map(|r| UnitFolderInfo {
                folder_id: r.folder_id,
                path: folders::path_of(&tree, r.folder_id),
                recursive: r.recursive,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(UnitDetail {
        summary,
        order_mode: row.order_mode,
        order_reverse: row.order_reverse,
        inner_shuffle: row.inner_shuffle,
        image_duration_ms: row.image_duration_ms,
        note: row.note,
        source_url: row.source_url,
        tags,
        folders,
        play_count: row.play_count,
        last_played_at: row.last_played_at,
    })
}

fn clean_name(name: &str) -> Result<String> {
    let name = nfc(name.trim());
    if name.is_empty() {
        return Err(CoreError::Invalid(
            "ユニットの名前を入力してください".into(),
        ));
    }
    Ok(name)
}

fn ensure_items(conn: &Connection, ids: &[i64]) -> Result<()> {
    let mut stmt = conn.prepare_cached("SELECT 1 FROM items WHERE id = ?1")?;
    for id in ids {
        if !stmt.exists([id])? {
            return Err(CoreError::NotFound(format!("アイテム {id}")));
        }
    }
    Ok(())
}

fn ensure_folders(conn: &Connection, refs: &[(i64, bool)]) -> Result<()> {
    if refs.is_empty() {
        return Err(CoreError::Invalid(
            "参照するフォルダを 1 つ以上選んでください".into(),
        ));
    }
    let mut stmt = conn.prepare_cached("SELECT 1 FROM folders WHERE id = ?1")?;
    for (id, _) in refs {
        if !stmt.exists([id])? {
            return Err(CoreError::NotFound(format!("フォルダ {id}")));
        }
    }
    Ok(())
}

fn dedup_keep_order(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// set ユニットを作る（並びは渡した順＝manual、表紙は先頭）。作ったユニットの ID を返す。
pub fn create_set(conn: &Connection, name: &str, item_ids: &[i64], now: i64) -> Result<i64> {
    let name = clean_name(name)?;
    let ids = dedup_keep_order(item_ids);
    if ids.is_empty() {
        return Err(CoreError::Invalid(
            "ユニットにまとめるアイテムを選んでください".into(),
        ));
    }
    ensure_items(conn, &ids)?;
    conn.execute(
        "INSERT INTO units (kind, name, cover_item_id, order_mode, origin, created_at, updated_at)
         VALUES ('set', ?1, ?2, 'manual', 'user', ?3, ?3)",
        params![name, ids[0], now],
    )?;
    let unit = conn.last_insert_rowid();
    let mut stmt =
        conn.prepare_cached("INSERT INTO unit_items (unit_id, item_id, pos) VALUES (?1, ?2, ?3)")?;
    for (i, id) in ids.iter().enumerate() {
        stmt.execute(params![unit, id, i as f64 + 1.0])?;
    }
    Ok(unit)
}

/// folder ユニットを作る（並びは名前順）。`folders` は (フォルダ ID, 子孫も含めるか) の並び。
pub fn create_folder(
    conn: &Connection,
    name: &str,
    folders: &[(i64, bool)],
    now: i64,
) -> Result<i64> {
    let name = clean_name(name)?;
    ensure_folders(conn, folders)?;
    conn.execute(
        "INSERT INTO units (kind, name, order_mode, origin, created_at, updated_at)
         VALUES ('folder', ?1, 'name', 'user', ?2, ?2)",
        params![name, now],
    )?;
    let unit = conn.last_insert_rowid();
    write_folders(conn, unit, folders)?;
    Ok(unit)
}

fn write_folders(conn: &Connection, unit: i64, folders: &[(i64, bool)]) -> Result<()> {
    conn.execute("DELETE FROM unit_folders WHERE unit_id = ?1", [unit])?;
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO unit_folders (unit_id, folder_id, recursive, pos) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (i, (folder, recursive)) in folders.iter().enumerate() {
        stmt.execute(params![unit, folder, i64::from(*recursive), i as f64 + 1.0])?;
    }
    Ok(())
}

/// 変更する項目（None は変えない。`Some(None)` は値を消す）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitPatch {
    pub name: Option<String>,
    pub cover_item_id: Option<Option<i64>>,
    pub order_mode: Option<OrderMode>,
    pub order_reverse: Option<bool>,
    pub inner_shuffle: Option<bool>,
    pub image_duration_ms: Option<Option<i64>>,
    pub rating: Option<i64>,
    pub note: Option<Option<String>>,
}

/// ユニットの設定を変える。
pub fn update(conn: &Connection, id: i64, patch: &UnitPatch, now: i64) -> Result<()> {
    let row = load_row(conn, id)?;
    if let Some(name) = &patch.name {
        let name = clean_name(name)?;
        conn.execute(
            "UPDATE units SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
    }
    if let Some(cover) = patch.cover_item_id {
        if let Some(cover) = cover
            && !members(conn, id)?.contains(&cover)
        {
            return Err(CoreError::Invalid(
                "表紙にできるのはユニットのメンバーだけです".into(),
            ));
        }
        conn.execute(
            "UPDATE units SET cover_item_id = ?2 WHERE id = ?1",
            params![id, cover],
        )?;
    }
    if let Some(mode) = patch.order_mode {
        if mode == OrderMode::Manual && row.kind == UnitKind::Folder {
            return Err(CoreError::Invalid(
                "フォルダ群のユニットは手動では並べられません".into(),
            ));
        }
        conn.execute(
            "UPDATE units SET order_mode = ?2 WHERE id = ?1",
            params![id, mode.as_str()],
        )?;
    }
    if let Some(reverse) = patch.order_reverse {
        conn.execute(
            "UPDATE units SET order_reverse = ?2 WHERE id = ?1",
            params![id, i64::from(reverse)],
        )?;
    }
    if let Some(shuffle) = patch.inner_shuffle {
        conn.execute(
            "UPDATE units SET inner_shuffle = ?2 WHERE id = ?1",
            params![id, i64::from(shuffle)],
        )?;
    }
    if let Some(duration) = patch.image_duration_ms {
        if duration.is_some_and(|d| d <= 0) {
            return Err(CoreError::Invalid(
                "画像の表示時間は 1 秒以上にしてください".into(),
            ));
        }
        conn.execute(
            "UPDATE units SET image_duration_ms = ?2 WHERE id = ?1",
            params![id, duration],
        )?;
    }
    if let Some(rating) = patch.rating {
        if !(0..=5).contains(&rating) {
            return Err(CoreError::Invalid(format!("評価は 0〜5 です（{rating}）")));
        }
        conn.execute(
            "UPDATE units SET rating = ?2 WHERE id = ?1",
            params![id, rating],
        )?;
    }
    if let Some(note) = &patch.note {
        let note = note.as_deref().map(str::trim).filter(|n| !n.is_empty());
        conn.execute(
            "UPDATE units SET note = ?2 WHERE id = ?1",
            params![id, note],
        )?;
    }
    touch(conn, id, now)
}

fn touch(conn: &Connection, id: i64, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE units SET updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

fn require_set(conn: &Connection, id: i64) -> Result<UnitRow> {
    let row = load_row(conn, id)?;
    if row.kind != UnitKind::Set {
        return Err(CoreError::Invalid(
            "フォルダ群のユニットのメンバーはフォルダで決まります".into(),
        ));
    }
    Ok(row)
}

/// set ユニットの最後にアイテムを加える（すでにいるものはそのまま）。
pub fn add_items(conn: &Connection, id: i64, item_ids: &[i64], now: i64) -> Result<()> {
    require_set(conn, id)?;
    let ids = dedup_keep_order(item_ids);
    ensure_items(conn, &ids)?;
    let mut pos: f64 = conn.query_row(
        "SELECT coalesce(max(pos), 0) FROM unit_items WHERE unit_id = ?1",
        [id],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO unit_items (unit_id, item_id, pos) VALUES (?1, ?2, ?3)",
    )?;
    for item in ids {
        pos += 1.0;
        stmt.execute(params![id, item, pos])?;
    }
    conn.execute(
        "UPDATE units SET cover_item_id = (SELECT item_id FROM unit_items WHERE unit_id = ?1 ORDER BY pos LIMIT 1)
         WHERE id = ?1 AND cover_item_id IS NULL",
        [id],
    )?;
    touch(conn, id, now)
}

/// set ユニットからアイテムを外す（表紙を外したら表紙は先頭のメンバーに戻る）。
pub fn remove_items(conn: &Connection, id: i64, item_ids: &[i64], now: i64) -> Result<()> {
    require_set(conn, id)?;
    let mut stmt =
        conn.prepare_cached("DELETE FROM unit_items WHERE unit_id = ?1 AND item_id = ?2")?;
    for item in item_ids {
        stmt.execute([id, *item])?;
    }
    conn.execute(
        "UPDATE units SET cover_item_id = NULL WHERE id = ?1
         AND cover_item_id NOT IN (SELECT item_id FROM unit_items WHERE unit_id = ?1)",
        [id],
    )?;
    touch(conn, id, now)
}

/// set ユニットのメンバーを `ordered`（見えている並び）の順に並べ直し、並び順を手動にする。
/// 逆順のユニットは逆にして記録する（逆順のまま、見えている並びが `ordered` になる）。
/// `ordered` にないメンバー（ゴミ箱のものなど）は、元の順のまま後ろに回す。
pub fn reorder(conn: &Connection, id: i64, ordered: &[i64], now: i64) -> Result<()> {
    let row = require_set(conn, id)?;
    let def = load_def(conn, &row)?;
    let mut current = def.items.clone();
    current.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let existing: HashSet<i64> = current.iter().map(|(i, _)| *i).collect();
    let mut order: Vec<i64> = dedup_keep_order(ordered)
        .into_iter()
        .filter(|i| existing.contains(i))
        .collect();
    if row.order_reverse {
        order.reverse();
    }
    let placed: HashSet<i64> = order.iter().copied().collect();
    order.extend(
        current
            .iter()
            .map(|(i, _)| *i)
            .filter(|i| !placed.contains(i)),
    );
    let mut stmt =
        conn.prepare_cached("UPDATE unit_items SET pos = ?3 WHERE unit_id = ?1 AND item_id = ?2")?;
    for (i, item) in order.iter().enumerate() {
        stmt.execute(params![id, item, i as f64 + 1.0])?;
    }
    conn.execute("UPDATE units SET order_mode = 'manual' WHERE id = ?1", [id])?;
    touch(conn, id, now)
}

/// folder ユニットの参照フォルダを置き換える（渡した順が並び順）。
pub fn set_folders(conn: &Connection, id: i64, folders: &[(i64, bool)], now: i64) -> Result<()> {
    let row = load_row(conn, id)?;
    if row.kind != UnitKind::Folder {
        return Err(CoreError::Invalid(
            "参照フォルダを持てるのはフォルダ群のユニットだけです".into(),
        ));
    }
    ensure_folders(conn, folders)?;
    write_folders(conn, id, folders)?;
    touch(conn, id, now)
}

/// ユニットを解除する（ユニットを消すだけで、アイテムは残る）。
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    load_row(conn, id)?;
    conn.execute("DELETE FROM units WHERE id = ?1", [id])?;
    Ok(())
}

/// ユニットを選ぶ一覧（名前に `filter` を含むもの、更新の新しい順）。
pub fn list(
    conn: &Connection,
    filter: &str,
    kind: Option<UnitKind>,
    limit: usize,
) -> Result<Vec<UnitRef>> {
    let filter = nfc(filter.trim()).to_lowercase();
    let mut stmt = conn.prepare_cached(
        "SELECT id, name, kind FROM units WHERE ?1 IS NULL OR kind = ?1 ORDER BY updated_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([kind.map(UnitKind::as_str)], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, kind) = row?;
        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
            continue;
        }
        let Some(kind) = UnitKind::parse(&kind) else {
            continue;
        };
        out.push(UnitRef { id, name, kind });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// アイテムが入っているユニット（set のメンバー、または参照フォルダに入っている folder ユニット）。
pub fn units_of_item(conn: &Connection, item_id: i64) -> Result<Vec<UnitRef>> {
    let mut out: Vec<UnitRef> = Vec::new();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT u.id, u.name FROM unit_items m JOIN units u ON u.id = m.unit_id WHERE m.item_id = ?1",
        )?;
        let rows = stmt.query_map([item_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in rows {
            let (id, name) = row?;
            out.push(UnitRef {
                id,
                name,
                kind: UnitKind::Set,
            });
        }
    }
    let item_folders: Vec<i64> = {
        let mut stmt =
            conn.prepare_cached("SELECT folder_id FROM item_folders WHERE item_id = ?1")?;
        let rows = stmt.query_map([item_id], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    if !item_folders.is_empty() {
        let parents: HashMap<i64, Option<i64>> = {
            let mut stmt = conn.prepare_cached("SELECT id, parent_id FROM folders")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        // 直接入っているフォルダと、その祖先（recursive の参照なら祖先でも含まれる）
        let direct: HashSet<i64> = item_folders.iter().copied().collect();
        let mut ancestors: HashSet<i64> = HashSet::new();
        for f in &item_folders {
            let mut current = Some(*f);
            while let Some(id) = current {
                if !ancestors.insert(id) {
                    break;
                }
                current = parents.get(&id).copied().flatten();
            }
        }
        let mut stmt = conn.prepare_cached(
            "SELECT u.id, u.name, f.folder_id, f.recursive FROM unit_folders f JOIN units u ON u.id = f.unit_id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        for row in rows {
            let (id, name, folder, recursive) = row?;
            let hit = direct.contains(&folder) || (recursive && ancestors.contains(&folder));
            if hit && !out.iter().any(|u| u.id == id) {
                out.push(UnitRef {
                    id,
                    name,
                    kind: UnitKind::Folder,
                });
            }
        }
    }
    out.sort_by(|a, b| crate::text::natural_cmp(&a.name, &b.name).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

/// グリッドで set ユニットをまとめるための索引。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetIndex {
    /// アイテム → 入っている set ユニット（ゴミ箱のアイテムは含めない）。
    pub by_item: HashMap<i64, Vec<i64>>,
    /// ユニット → 表紙の縦横比×1000。
    pub cover_aspect: HashMap<i64, u16>,
}

/// set ユニットの所属と表紙の縦横比をまとめて読む。
pub fn set_index(conn: &Connection) -> Result<SetIndex> {
    let mut index = SetIndex::default();
    let mut stmt = conn.prepare_cached(
        "SELECT m.unit_id, m.item_id, i.width, i.height, u.cover_item_id
         FROM unit_items m JOIN items i ON i.id = m.item_id JOIN units u ON u.id = m.unit_id
         WHERE i.trashed_at IS NULL ORDER BY m.unit_id, m.pos, m.item_id",
    )?;
    let mut rows = stmt.query([])?;
    // ユニットごとに「表紙」か「先頭のメンバー」の縦横比
    let mut first: HashMap<i64, u16> = HashMap::new();
    while let Some(row) = rows.next()? {
        let unit: i64 = row.get(0)?;
        let item: i64 = row.get(1)?;
        let aspect = aspect_code(row.get(2)?, row.get(3)?);
        let cover: Option<i64> = row.get(4)?;
        index.by_item.entry(item).or_default().push(unit);
        first.entry(unit).or_insert(aspect);
        if cover == Some(item) {
            index.cover_aspect.insert(unit, aspect);
        }
    }
    for (unit, aspect) in first {
        index.cover_aspect.entry(unit).or_insert(aspect);
    }
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn item(conn: &Connection, id: i64, title: &str, kind: &str) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, duration_ms, title, original_name, imported_at, updated_at)
             VALUES (?1, ?2, 'jpg', ?4, 1, 1600, 900, ?5, ?3, ?3, ?1, 0)",
            params![id, vec![id as u8; 32], title, kind, if kind == "image" { None } else { Some(1000) }],
        )
        .unwrap();
    }

    fn items(conn: &Connection) {
        for (id, title) in [(1, "p1"), (2, "p2"), (3, "p10"), (4, "x")] {
            item(conn, id, title, "image");
        }
        item(conn, 5, "song", "audio");
    }

    #[test]
    fn creates_set_units_in_selection_order() {
        let conn = test_conn();
        items(&conn);
        let unit = create_set(&conn, " 鎌倉 ", &[3, 1, 3, 2], 10).unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![3, 1, 2]);
        let summary = &summaries(&conn, &[unit]).unwrap()[0];
        assert_eq!(summary.name, "鎌倉");
        assert_eq!(summary.cover.as_ref().unwrap().item_id, 3);
        assert_eq!(summary.stats.count, 3);
        assert_eq!(summary.stats.duration_ms, 3 * DEFAULT_IMAGE_DURATION_MS);

        assert!(matches!(
            create_set(&conn, "x", &[], 0),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            create_set(&conn, "x", &[99], 0),
            Err(CoreError::NotFound(_))
        ));
        assert!(matches!(
            create_set(&conn, " ", &[1], 0),
            Err(CoreError::Invalid(_))
        ));
    }

    #[test]
    fn edits_members_order_and_cover() {
        let conn = test_conn();
        items(&conn);
        let unit = create_set(&conn, "u", &[1, 2], 0).unwrap();
        add_items(&conn, unit, &[5, 1, 4], 1).unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![1, 2, 5, 4]);

        // 名前順にしてから手で並べ替えると manual に戻る
        update(
            &conn,
            unit,
            &UnitPatch {
                order_mode: Some(OrderMode::Name),
                ..UnitPatch::default()
            },
            2,
        )
        .unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![1, 2, 5, 4]);
        reorder(&conn, unit, &[4, 1], 3).unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![4, 1, 2, 5]);
        assert_eq!(detail(&conn, unit).unwrap().order_mode, OrderMode::Manual);

        update(
            &conn,
            unit,
            &UnitPatch {
                cover_item_id: Some(Some(5)),
                ..UnitPatch::default()
            },
            4,
        )
        .unwrap();
        assert_eq!(
            summaries(&conn, &[unit]).unwrap()[0]
                .cover
                .as_ref()
                .unwrap()
                .item_id,
            5
        );
        assert!(
            update(
                &conn,
                unit,
                &UnitPatch {
                    cover_item_id: Some(Some(3)),
                    ..UnitPatch::default()
                },
                4
            )
            .is_err()
        );

        // 表紙を外すと先頭のメンバーが表紙になる
        remove_items(&conn, unit, &[5], 5).unwrap();
        assert_eq!(
            summaries(&conn, &[unit]).unwrap()[0]
                .cover
                .as_ref()
                .unwrap()
                .item_id,
            4
        );

        // ゴミ箱のメンバーは出さず、表紙も次に移る
        conn.execute("UPDATE items SET trashed_at = 1 WHERE id = 4", [])
            .unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![1, 2]);
        assert_eq!(
            summaries(&conn, &[unit]).unwrap()[0]
                .cover
                .as_ref()
                .unwrap()
                .item_id,
            1
        );
        // 並べ替えに出てこないメンバー（ゴミ箱）は後ろに残る
        reorder(&conn, unit, &[2, 1], 6).unwrap();
        conn.execute("UPDATE items SET trashed_at = NULL WHERE id = 4", [])
            .unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![2, 1, 4]);
    }

    #[test]
    fn updates_settings_with_validation() {
        let conn = test_conn();
        items(&conn);
        let unit = create_set(&conn, "u", &[1, 5], 0).unwrap();
        update(
            &conn,
            unit,
            &UnitPatch {
                name: Some("アルバム".into()),
                inner_shuffle: Some(true),
                image_duration_ms: Some(Some(15_000)),
                rating: Some(4),
                note: Some(Some("  ".into())),
                ..UnitPatch::default()
            },
            1,
        )
        .unwrap();
        let d = detail(&conn, unit).unwrap();
        assert_eq!(d.summary.name, "アルバム");
        assert!(d.inner_shuffle);
        assert_eq!(d.image_duration_ms, Some(15_000));
        assert_eq!((d.summary.own_rating, d.summary.stats.rating), (4, 4));
        assert_eq!(d.note, None);
        assert_eq!(d.summary.stats.duration_ms, 15_000 + 1000);
        assert_eq!(
            d.summary.stats.kinds,
            vec![MediaKind::Image, MediaKind::Audio]
        );

        let bad = |patch: UnitPatch| update(&conn, unit, &patch, 2).is_err();
        assert!(bad(UnitPatch {
            rating: Some(6),
            ..UnitPatch::default()
        }));
        assert!(bad(UnitPatch {
            image_duration_ms: Some(Some(0)),
            ..UnitPatch::default()
        }));
        assert!(bad(UnitPatch {
            name: Some(String::new()),
            ..UnitPatch::default()
        }));
    }

    #[test]
    fn folder_units_follow_folders() {
        let conn = test_conn();
        items(&conn);
        let photos = folders::create(&conn, None, "写真", 0).unwrap();
        let trip = folders::create(&conn, Some(photos), "旅行", 0).unwrap();
        folders::set_items(&conn, &[3, 1], &[photos], &[]).unwrap();
        folders::set_items(&conn, &[2], &[trip], &[]).unwrap();

        let unit = create_folder(&conn, "写真", &[(photos, true)], 0).unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![1, 2, 3]);
        assert_eq!(detail(&conn, unit).unwrap().folders[0].path, vec!["写真"]);
        set_folders(&conn, unit, &[(trip, false), (photos, false)], 1).unwrap();
        assert_eq!(members(&conn, unit).unwrap(), vec![2, 1, 3]);

        // フォルダ群は手動の並び・メンバーの直接の追加ができない
        assert!(
            update(
                &conn,
                unit,
                &UnitPatch {
                    order_mode: Some(OrderMode::Manual),
                    ..UnitPatch::default()
                },
                2
            )
            .is_err()
        );
        assert!(add_items(&conn, unit, &[4], 2).is_err());
        assert!(set_folders(&conn, unit, &[], 2).is_err());

        // 1 つのアイテムが set と folder の両方のユニットに入る
        let set = create_set(&conn, "選んだもの", &[2, 4], 3).unwrap();
        let of_two: Vec<(i64, UnitKind)> = units_of_item(&conn, 2)
            .unwrap()
            .into_iter()
            .map(|u| (u.id, u.kind))
            .collect();
        assert_eq!(of_two, vec![(unit, UnitKind::Folder), (set, UnitKind::Set)]);
        // recursive の参照なら、子フォルダのアイテムも入る
        set_folders(&conn, unit, &[(photos, true)], 4).unwrap();
        assert!(
            units_of_item(&conn, 2)
                .unwrap()
                .iter()
                .any(|u| u.id == unit)
        );
        assert!(
            units_of_item(&conn, 4)
                .unwrap()
                .iter()
                .all(|u| u.id != unit)
        );

        delete(&conn, unit).unwrap();
        assert!(matches!(members(&conn, unit), Err(CoreError::NotFound(_))));
        assert_eq!(members(&conn, set).unwrap(), vec![2, 4]);
    }

    #[test]
    fn reverse_flips_members_but_keeps_the_cover_and_manual_moves() {
        let conn = test_conn();
        items(&conn);
        let reverse = |on: bool| UnitPatch {
            order_reverse: Some(on),
            ..UnitPatch::default()
        };
        let cover = |unit: i64| {
            summaries(&conn, &[unit]).unwrap()[0]
                .cover
                .as_ref()
                .unwrap()
                .item_id
        };

        // フォルダ群（表紙を選んでいない）：表紙は逆にする前の先頭のまま
        let photos = folders::create(&conn, None, "写真", 0).unwrap();
        folders::set_items(&conn, &[3, 1, 2], &[photos], &[]).unwrap();
        let folder = create_folder(&conn, "写真", &[(photos, false)], 0).unwrap();
        update(&conn, folder, &reverse(true), 1).unwrap();
        assert_eq!(members(&conn, folder).unwrap(), vec![3, 2, 1]);
        assert!(detail(&conn, folder).unwrap().order_reverse);
        assert_eq!(cover(folder), 1);

        // ファイル群：見えている並びで並べ替えると、逆順のままその並びになる
        let set = create_set(&conn, "u", &[1, 2, 3], 0).unwrap();
        update(&conn, set, &reverse(true), 1).unwrap();
        assert_eq!(members(&conn, set).unwrap(), vec![3, 2, 1]);
        reorder(&conn, set, &[1, 3, 2], 2).unwrap();
        assert_eq!(members(&conn, set).unwrap(), vec![1, 3, 2]);
        assert!(detail(&conn, set).unwrap().order_reverse);
        // 加えたものは逆にする前の最後、つまり見えている並びの先頭に出る
        add_items(&conn, set, &[4], 3).unwrap();
        assert_eq!(members(&conn, set).unwrap(), vec![4, 1, 3, 2]);
        // 戻すと元の向き
        update(&conn, set, &reverse(false), 4).unwrap();
        assert_eq!(members(&conn, set).unwrap(), vec![2, 3, 1, 4]);
        assert_eq!(cover(set), 1);
    }

    #[test]
    fn lists_units_and_builds_the_set_index() {
        let conn = test_conn();
        items(&conn);
        let a = create_set(&conn, "鎌倉 2026", &[1, 2], 1).unwrap();
        let b = create_set(&conn, "京都", &[2, 3], 2).unwrap();
        update(
            &conn,
            b,
            &UnitPatch {
                cover_item_id: Some(Some(3)),
                ..UnitPatch::default()
            },
            3,
        )
        .unwrap();
        conn.execute(
            "UPDATE items SET width = 900, height = 1600 WHERE id = 3",
            [],
        )
        .unwrap();
        let names: Vec<String> = list(&conn, "", None, 10)
            .unwrap()
            .into_iter()
            .map(|u| u.name)
            .collect();
        assert_eq!(names, vec!["京都", "鎌倉 2026"]);
        assert_eq!(
            list(&conn, "鎌倉", Some(UnitKind::Set), 10).unwrap()[0].id,
            a
        );
        assert!(
            list(&conn, "", Some(UnitKind::Folder), 10)
                .unwrap()
                .is_empty()
        );

        conn.execute("UPDATE items SET trashed_at = 1 WHERE id = 1", [])
            .unwrap();
        let index = set_index(&conn).unwrap();
        assert_eq!(index.by_item.get(&2), Some(&vec![a, b]));
        assert_eq!(index.by_item.get(&1), None);
        assert_eq!(index.cover_aspect.get(&a), Some(&1778)); // 表紙（1）がゴミ箱なので 2
        assert_eq!(index.cover_aspect.get(&b), Some(&563));
    }

    /// 表紙もアイテムの概要（items.rs の SUMMARY）と同じく、サムネイルの作り直しの回数・解析の進み具合・透過・ドット絵の
    /// 実効値を持つ（作り直したサムネイルを古いキャッシュから出さない、表紙にも市松模様と最近傍を当てる）。
    #[test]
    fn cover_carries_the_thumb_rev_and_the_tile_facts() {
        use crate::analysis::facts::{FACTS_V, MediaFacts, PIXEL_RULE};
        use crate::analysis::store::{ItemWrite, Write, write};
        use crate::asset::{Change, SettingsPatch, set_settings};

        let conn = test_conn();
        items(&conn);
        let ready = ItemWrite {
            id: 1,
            hash: [1; 32],
            write: Write::Ready {
                results: Vec::new(),
                keep: None,
                facts: Box::new(MediaFacts {
                    facts_v: FACTS_V,
                    alpha: Some(2),
                    pixel_art: Some(true),
                    pixel_rule: Some(PIXEL_RULE),
                    frame_count: Some(1),
                    ..MediaFacts::default()
                }),
                stamp: None,
            },
        };
        assert_eq!(write(&conn, &[ready], 0).unwrap().written, 1);
        conn.execute(
            "UPDATE items SET thumb_state = 'stale', thumb_rev = 2 WHERE id = 1",
            [],
        )
        .unwrap();
        let unit = create_set(&conn, "u", &[1, 2], 10).unwrap();
        let cover = |conn: &Connection| summaries(conn, &[unit]).unwrap()[0].cover.clone().unwrap();
        let item = |conn: &Connection| crate::items::summaries(conn, &[1]).unwrap().remove(0);

        let c = cover(&conn);
        assert_eq!(
            (c.item_id, c.thumb_state.as_str(), c.thumb_rev),
            (1, "stale", 2)
        );
        assert_eq!(
            (c.analysis_state, c.alpha, c.pixel_art),
            (AnalysisState::Ready, Some(Alpha::Partial), true)
        );
        // 設定が自動の判定に勝つ（アイテムの概要と同じ値）
        let set = |value: Change<bool>| {
            let patch = SettingsPatch {
                pixel_art: Some(value),
                ..SettingsPatch::default()
            };
            assert_eq!(set_settings(&conn, &[1], &patch, 0).unwrap(), 1);
        };
        set(Change::Set(false));
        let (c, i) = (cover(&conn), item(&conn));
        assert!(!c.pixel_art);
        assert_eq!(
            (c.thumb_rev, c.analysis_state, c.alpha, c.pixel_art),
            (i.thumb_rev, i.analysis_state, i.alpha, i.pixel_art)
        );
        set(Change::Clear);
        assert!(cover(&conn).pixel_art);
        // 未解析の表紙（2）は透過も判定もない
        let other = create_set(&conn, "v", &[2], 10).unwrap();
        let c = summaries(&conn, &[other]).unwrap()[0]
            .cover
            .clone()
            .unwrap();
        assert_eq!(
            (c.analysis_state, c.alpha, c.pixel_art, c.thumb_rev),
            (AnalysisState::Pending, None, false, 0)
        );
        // 表紙の問い合わせは主キーと台帳の索引で引く
        let plan = crate::db::query_plan(&conn, &COVER_SQL.replace("?1", "1"));
        assert!(!plan.contains("SCAN"), "{plan}");
    }
}
