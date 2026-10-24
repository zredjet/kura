//! 一覧の取得（docs/data-model.md §4「ID 先取り」）。
//!
//! - `query_ids`：条件に合う ID・縦横比・日付のグループを並び順で返す（全件分。詳細は持たない）。
//! - `summaries`：見えている分の詳細（最大 200 件）。
//! - `detail`：インスペクタ用（タグ・フォルダ・ユニットを含む）。
//! - `update`・`trash`・`restore`・`empty_trash`：編集とゴミ箱。

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::asset::{self, Alpha, AnalysisState};
use crate::import::MediaKind;
use crate::library::hex;
use crate::query::{DbResolver, Expr, QueryContext, compile, compile_unanalyzed};
use crate::text::{natural_cmp, nfc};
use crate::unit::{self, LIST_FLAG, SetIndex};
use crate::{CoreError, Result, folders, random, smart};

pub use crate::tags::TagRef;
pub use crate::unit::UnitRef;

/// 一度に詳細を取れる件数。
pub const MAX_SUMMARIES: usize = 200;

const DAY_MS: i64 = 86_400_000;

/// 「最近の取り込み」に入れる日数（docs/decisions.md）。
pub const RECENT_DAYS: i64 = 7;

/// 「最近の取り込み」の起点（この時刻以降の追加）。
pub fn recent_since(now: i64) -> i64 {
    now - RECENT_DAYS * DAY_MS
}

/// 並べる基準。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortKey {
    /// 追加日（日付の見出しあり）。
    #[default]
    Imported,
    /// 撮影日（日付の見出しあり。不明は最後）。
    Captured,
    /// 名前の自然順。
    Name,
    Rating,
    /// 長さ（不明は最後）。
    Duration,
    Size,
}

impl SortKey {
    /// DB・JSON での名前（`playlists.sort_key`）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::Captured => "captured",
            Self::Name => "name",
            Self::Rating => "rating",
            Self::Duration => "duration",
            Self::Size => "size",
        }
    }

    /// 名前から。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "imported" => Some(Self::Imported),
            "captured" => Some(Self::Captured),
            "name" => Some(Self::Name),
            "rating" => Some(Self::Rating),
            "duration" => Some(Self::Duration),
            "size" => Some(Self::Size),
            _ => None,
        }
    }
}

/// 並び順。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemSort {
    pub key: SortKey,
    /// 大きい・新しい順か。
    pub desc: bool,
}

impl Default for ItemSort {
    fn default() -> Self {
        Self {
            key: SortKey::Imported,
            desc: true,
        }
    }
}

/// 一覧の種類（サイドバーの項目）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ItemView {
    /// すべて（ゴミ箱以外）。
    #[default]
    All,
    /// `since` 以降に追加したもの（最近の取り込み）。
    Recent { since: i64 },
    /// どのフォルダにも入っていないもの。
    Unsorted,
    /// タグが 1 つもないもの。
    Untagged,
    /// すべてをシードで決まるランダムな順に（見出しなし）。
    Random { seed: u64 },
    /// ゴミ箱（入れた日の新しい順）。
    Trash,
    /// そのフォルダと子孫フォルダにあるもの。
    Folder { id: i64 },
    /// ユニットのメンバー（ユニットの並び順。見出しなし）。
    Unit { id: i64 },
    /// スマートフォルダ（保存した検索）。
    Smart { id: i64 },
}

/// 一覧の条件。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemQuery {
    pub view: ItemView,
    /// set ユニットを 1 件にまとめる（「ユニットをまとめて表示」）。ゴミ箱とユニットの中では使わない。
    pub collapse_units: bool,
    /// 検索の条件（docs/search-query.md）。一覧の種類の条件に AND で重ねる。
    pub filter: Option<Expr>,
    /// 並び順（ゴミ箱・ランダム・ユニットの中では使わない）。
    pub sort: ItemSort,
}

/// 日付の見出しのグループ（`start` 番目から始まる、`day` 日のアイテム）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayGroup {
    pub start: u32,
    /// 1970-01-01 からの日数（指定した時差での日付）。
    pub day: i32,
}

/// `query_ids` の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdList {
    pub ids: Vec<u32>,
    /// 縦横比×1000（音楽と不明は 1000）。
    pub aspects: Vec<u16>,
    pub groups: Vec<DayGroup>,
}

impl IdList {
    /// フロントへ送る生バイト（すべてリトルエンディアン）。
    /// `[件数 u32][id u32 × N][縦横比×1000 u16 × N][グループ数 u32][開始位置 u32 × G][日 i32 × G]`
    pub fn encode(&self) -> Vec<u8> {
        let n = self.ids.len();
        let g = self.groups.len();
        let mut out = Vec::with_capacity(8 + n * 6 + g * 8);
        out.extend_from_slice(&(n as u32).to_le_bytes());
        for id in &self.ids {
            out.extend_from_slice(&id.to_le_bytes());
        }
        for aspect in &self.aspects {
            out.extend_from_slice(&aspect.to_le_bytes());
        }
        out.extend_from_slice(&(g as u32).to_le_bytes());
        for group in &self.groups {
            out.extend_from_slice(&group.start.to_le_bytes());
        }
        for group in &self.groups {
            out.extend_from_slice(&group.day.to_le_bytes());
        }
        out
    }
}

/// 縦横比×1000 を u16 にする（不明は 1000）。
pub fn aspect_code(width: Option<i64>, height: Option<i64>) -> u16 {
    match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => {
            let ratio = (w as f64 / h as f64 * 1000.0).round();
            ratio.clamp(1.0, f64::from(u16::MAX)) as u16
        }
        _ => 1000,
    }
}

/// 一覧の 1 行（日付の見出しに使う時刻付き。時刻が不明なら None）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ListRow {
    id: u32,
    aspect: u16,
    at: Option<i64>,
}

/// 撮影日で並べたときの、撮影日のないもののグループの日（フロントは「撮影日なし」と出す）。
pub const NO_DATE_DAY: i32 = i32::MIN;

/// 一覧の種類の条件（`items i` に対する WHERE）。
struct Scope {
    sql: String,
    params: Vec<Value>,
}

fn scope(conn: &Connection, view: ItemView, ctx: &QueryContext) -> Result<Scope> {
    let alive = "i.trashed_at IS NULL".to_string();
    Ok(match view {
        ItemView::All | ItemView::Random { .. } | ItemView::Unit { .. } => Scope {
            sql: alive,
            params: Vec::new(),
        },
        ItemView::Recent { since } => Scope {
            sql: format!("{alive} AND i.imported_at >= ?"),
            params: vec![Value::Integer(since)],
        },
        ItemView::Unsorted => Scope {
            sql: format!(
                "{alive} AND NOT EXISTS (SELECT 1 FROM item_folders m WHERE m.item_id = i.id)"
            ),
            params: Vec::new(),
        },
        ItemView::Untagged => Scope {
            sql: format!(
                "{alive} AND NOT EXISTS (SELECT 1 FROM item_tags m WHERE m.item_id = i.id)"
            ),
            params: Vec::new(),
        },
        ItemView::Trash => Scope {
            sql: "i.trashed_at IS NOT NULL".to_string(),
            params: Vec::new(),
        },
        ItemView::Folder { id } => {
            let ids = folders::descendants(conn, id)?;
            if ids.is_empty() {
                return Err(CoreError::NotFound(format!("フォルダ {id}")));
            }
            let marks = vec!["?"; ids.len()].join(", ");
            Scope {
                sql: format!(
                    "{alive} AND i.id IN (SELECT m.item_id FROM item_folders m WHERE m.folder_id IN ({marks}))"
                ),
                params: ids.into_iter().map(Value::Integer).collect(),
            }
        }
        ItemView::Smart { id } => {
            let folder = smart::get(conn, id)?;
            let cond = compile(&smart::expr_of(&folder)?, &DbResolver { conn }, ctx)?;
            Scope {
                sql: format!("{alive} AND ({})", cond.sql),
                params: cond.params,
            }
        }
    })
}

/// 一覧の種類と検索の条件を合わせた WHERE。
fn where_clause(conn: &Connection, query: &ItemQuery, ctx: &QueryContext) -> Result<Scope> {
    let mut scope = scope(conn, query.view, ctx)?;
    if let Some(filter) = &query.filter {
        let cond = compile(filter, &DbResolver { conn }, ctx)?;
        scope.sql = format!("{} AND ({})", scope.sql, cond.sql);
        scope.params.extend(cond.params);
    }
    Ok(scope)
}

/// 条件に合う ID・縦横比・日付のグループを並び順で返す。日付の見出しは `ctx` の時差で区切る。
pub fn query_ids(conn: &Connection, query: &ItemQuery, ctx: &QueryContext) -> Result<IdList> {
    let scope = where_clause(conn, query, ctx)?;
    let (mut rows, grouped) = match query.view {
        ItemView::Unit { id } => {
            let members = unit::members(conn, id)?;
            let matching: HashSet<i64> = if query.filter.is_some() {
                matching_ids(conn, &scope)?
            } else {
                members.iter().copied().collect()
            };
            let kept: Vec<i64> = members
                .into_iter()
                .filter(|m| matching.contains(m))
                .collect();
            (unit_rows(conn, &kept)?, false)
        }
        ItemView::Random { seed } => {
            let mut rows = read_rows(conn, &scope, "i.imported_at", "i.id")?;
            random::shuffle(&mut rows, &mut random::rng(seed));
            (rows, false)
        }
        ItemView::Trash => (
            read_rows(conn, &scope, "i.trashed_at", "i.trashed_at DESC, i.id DESC")?,
            true,
        ),
        _ => sorted_rows(conn, &scope, query.sort)?,
    };
    let collapsible = !matches!(query.view, ItemView::Trash | ItemView::Unit { .. });
    if query.collapse_units && collapsible {
        rows = collapse(rows, &unit::set_index(conn)?);
    }
    Ok(build_list(&rows, grouped, ctx.utc_offset_minutes))
}

fn matching_ids(conn: &Connection, scope: &Scope) -> Result<HashSet<i64>> {
    let mut stmt = conn.prepare(&format!("SELECT i.id FROM items i WHERE {}", scope.sql))?;
    let rows = stmt.query_map(params_from_iter(scope.params.iter()), |r| r.get(0))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

/// 並び順で読む。見出しを付けるか（追加日・撮影日のときだけ）も返す。
fn sorted_rows(conn: &Connection, scope: &Scope, sort: ItemSort) -> Result<(Vec<ListRow>, bool)> {
    let dir = if sort.desc { "DESC" } else { "ASC" };
    Ok(match sort.key {
        // 一覧は全件を並び順で読む（意図した全件の走査。追加日なら idx_items_imported の順に読む）
        SortKey::Imported => (
            read_rows(
                conn,
                scope,
                "i.imported_at",
                &format!("i.imported_at {dir}, i.id {dir}"),
            )?,
            true,
        ),
        SortKey::Captured => (
            read_rows(
                conn,
                scope,
                "i.captured_at",
                &format!("i.captured_at IS NULL, i.captured_at {dir}, i.id {dir}"),
            )?,
            true,
        ),
        SortKey::Rating => (
            read_rows(
                conn,
                scope,
                "NULL",
                &format!("i.rating {dir}, i.imported_at DESC, i.id DESC"),
            )?,
            false,
        ),
        SortKey::Duration => (
            read_rows(
                conn,
                scope,
                "NULL",
                &format!("i.duration_ms IS NULL, i.duration_ms {dir}, i.id {dir}"),
            )?,
            false,
        ),
        SortKey::Size => (
            read_rows(
                conn,
                scope,
                "NULL",
                &format!("i.size_bytes {dir}, i.id {dir}"),
            )?,
            false,
        ),
        SortKey::Name => {
            // 自然順（「2」<「10」）は SQL ではできないので読んでから並べる
            let mut stmt = conn.prepare(&format!(
                "SELECT i.id, i.width, i.height, i.title FROM items i WHERE {}",
                scope.sql
            ))?;
            let mut rows = stmt.query(params_from_iter(scope.params.iter()))?;
            let mut named: Vec<(String, ListRow)> = Vec::new();
            while let Some(row) = rows.next()? {
                named.push((
                    row.get(3)?,
                    ListRow {
                        id: to_u32(row.get(0)?)?,
                        aspect: aspect_code(row.get(1)?, row.get(2)?),
                        at: None,
                    },
                ));
            }
            named.sort_by(|(a, ra), (b, rb)| natural_cmp(a, b).then_with(|| ra.id.cmp(&rb.id)));
            if sort.desc {
                named.reverse();
            }
            (named.into_iter().map(|(_, r)| r).collect(), false)
        }
    })
}

fn to_u32(id: i64) -> Result<u32> {
    u32::try_from(id)
        .ok()
        .filter(|id| id & LIST_FLAG == 0)
        .ok_or_else(|| CoreError::Invalid(format!("ID が大きすぎます: {id}")))
}

/// WHERE と並び順で読む。`at` は見出しに使う時刻の列（なければ `NULL`）。
fn read_rows(conn: &Connection, scope: &Scope, at: &str, order: &str) -> Result<Vec<ListRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT i.id, i.width, i.height, {at} FROM items i WHERE {} ORDER BY {order}",
        scope.sql
    ))?;
    let mut rows = stmt.query(params_from_iter(scope.params.iter()))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(ListRow {
            id: to_u32(row.get(0)?)?,
            aspect: aspect_code(row.get(1)?, row.get(2)?),
            at: row.get(3)?,
        });
    }
    Ok(out)
}

fn unit_rows(conn: &Connection, members: &[i64]) -> Result<Vec<ListRow>> {
    let mut stmt =
        conn.prepare_cached("SELECT id, width, height, imported_at FROM items WHERE id = ?1")?;
    let mut out = Vec::with_capacity(members.len());
    for id in members {
        let row = stmt
            .query_row([id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .optional()?;
        if let Some((id, w, h, at)) = row {
            out.push(ListRow {
                id: to_u32(id)?,
                aspect: aspect_code(w, h),
                at: Some(at),
            });
        }
    }
    Ok(out)
}

/// 種類ごとの件数（⌘K パレットの「一致 186 項目 — 画像 142 · 動画 44」）。
pub use crate::import::KindCounts;

/// 一覧と同じ条件で、種類ごとに数える（フォント・3D も分けて数える）。
pub fn count_kinds(conn: &Connection, query: &ItemQuery, ctx: &QueryContext) -> Result<KindCounts> {
    let scope = where_clause(conn, query, ctx)?;
    let mut counts = KindCounts::default();
    let mut add = |kind: MediaKind, n: i64| counts.add(kind, u64::try_from(n).unwrap_or(0));
    if let ItemView::Unit { id } = query.view {
        let members: HashSet<i64> = unit::members(conn, id)?.into_iter().collect();
        let mut stmt = conn.prepare(&format!(
            "SELECT i.id, i.kind FROM items i WHERE {}",
            scope.sql
        ))?;
        let rows = stmt.query_map(params_from_iter(scope.params.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, MediaKind>(1)?))
        })?;
        for row in rows {
            let (id, kind) = row?;
            if members.contains(&id) {
                add(kind, 1);
            }
        }
        return Ok(counts);
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT i.kind, count(*) FROM items i WHERE {} GROUP BY i.kind",
        scope.sql
    ))?;
    let rows = stmt.query_map(params_from_iter(scope.params.iter()), |r| {
        Ok((r.get::<_, MediaKind>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (kind, n) = row?;
        add(kind, n);
    }
    Ok(counts)
}

/// 検索の条件が解析の事実（透過・ループ・ラウドネスなど）を使うとき、一覧の中で解析がまだのために一致が
/// 決まっていないアイテムの数（⌘K パレットの「未解析 N 件」。design.md §3.1）。事実を使わなければ None。
pub fn count_unanalyzed(
    conn: &Connection,
    query: &ItemQuery,
    ctx: &QueryContext,
) -> Result<Option<u64>> {
    let Some(filter) = &query.filter else {
        return Ok(None);
    };
    let Some(cond) = compile_unanalyzed(filter, &DbResolver { conn }, ctx)? else {
        return Ok(None);
    };
    let mut scope = scope(conn, query.view, ctx)?;
    scope.sql = format!("{} AND ({})", scope.sql, cond.sql);
    scope.params.extend(cond.params);
    if let ItemView::Unit { id } = query.view {
        let members: HashSet<i64> = unit::members(conn, id)?.into_iter().collect();
        let matching = matching_ids(conn, &scope)?;
        return Ok(Some(matching.intersection(&members).count() as u64));
    }
    let n: i64 = conn
        .prepare(&format!("SELECT count(*) FROM items i WHERE {}", scope.sql))?
        .query_row(params_from_iter(scope.params.iter()), |r| r.get(0))?;
    Ok(Some(u64::try_from(n).unwrap_or(0)))
}

/// set ユニットに入っているアイテムを、最初に現れた位置の 1 件のユニット（`LIST_FLAG | unit_id`）にまとめる。
fn collapse(rows: Vec<ListRow>, index: &SetIndex) -> Vec<ListRow> {
    let mut placed: HashSet<i64> = HashSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        match index.by_item.get(&i64::from(row.id)) {
            None => out.push(row),
            Some(units) => {
                for unit in units {
                    let Ok(id) = u32::try_from(*unit) else {
                        continue;
                    };
                    if id & LIST_FLAG != 0 || !placed.insert(*unit) {
                        continue;
                    }
                    out.push(ListRow {
                        id: LIST_FLAG | id,
                        aspect: index.cover_aspect.get(unit).copied().unwrap_or(1000),
                        at: row.at,
                    });
                }
            }
        }
    }
    out
}

fn build_list(rows: &[ListRow], grouped: bool, utc_offset_minutes: i32) -> IdList {
    let offset_ms = i64::from(utc_offset_minutes) * 60_000;
    let mut list = IdList {
        ids: Vec::with_capacity(rows.len()),
        aspects: Vec::with_capacity(rows.len()),
        groups: Vec::new(),
    };
    let mut current_day: Option<i32> = None;
    for row in rows {
        if grouped {
            let day = row.at.map_or(NO_DATE_DAY, |at| {
                i32::try_from((at + offset_ms).div_euclid(DAY_MS)).unwrap_or(0)
            });
            if current_day != Some(day) {
                list.groups.push(DayGroup {
                    start: list.ids.len() as u32,
                    day,
                });
                current_day = Some(day);
            }
        }
        list.ids.push(row.id);
        list.aspects.push(row.aspect);
    }
    list
}

/// サイドバーの「ライブラリ」の件数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryCounts {
    pub all: i64,
    pub recent: i64,
    pub unsorted: i64,
    pub untagged: i64,
    pub trash: i64,
    /// キャプチャで取り込んだもの（ゴミ箱を除く）。
    pub captures: i64,
}

/// サイドバーの件数（`recent_since` 以降の追加を「最近の取り込み」と数える）。
pub fn counts(conn: &Connection, recent_since: i64) -> Result<LibraryCounts> {
    let mut stmt = conn.prepare_cached(
        "SELECT
           (SELECT count(*) FROM items WHERE trashed_at IS NULL),
           (SELECT count(*) FROM items WHERE trashed_at IS NULL AND imported_at >= ?1),
           (SELECT count(*) FROM items WHERE trashed_at IS NULL
              AND NOT EXISTS (SELECT 1 FROM item_folders m WHERE m.item_id = items.id)),
           (SELECT count(*) FROM items WHERE trashed_at IS NULL
              AND NOT EXISTS (SELECT 1 FROM item_tags m WHERE m.item_id = items.id)),
           (SELECT count(*) FROM items WHERE trashed_at IS NOT NULL),
           (SELECT count(*) FROM items WHERE trashed_at IS NULL AND source_site = 'capture')",
    )?;
    Ok(stmt.query_row([recent_since], |row| {
        Ok(LibraryCounts {
            all: row.get(0)?,
            recent: row.get(1)?,
            unsorted: row.get(2)?,
            untagged: row.get(3)?,
            trash: row.get(4)?,
            captures: row.get(5)?,
        })
    })?)
}

/// 一覧の 1 件（グリッドのタイルに出す分）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSummary {
    pub id: i64,
    pub hash: String,
    pub ext: String,
    pub kind: MediaKind,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub size_bytes: i64,
    pub title: String,
    pub rating: i64,
    pub source_site: Option<String>,
    pub imported_at: i64,
    pub captured_at: Option<i64>,
    pub thumb_state: String,
    /// 再生用プロキシの状態（unknown / not_needed / pending / ready / failed）。
    pub proxy_state: String,
    pub last_played_at: Option<i64>,
    /// サムネイルを作り直した回数（URL の `?r=`。WebKit のメモリのキャッシュから古い絵を出さないため）。
    pub thumb_rev: i64,
    /// 解析の進み具合（`items.analysis_state`）。
    pub analysis_state: AnalysisState,
    /// 解析で分かった透過（`media_facts.alpha`。未解析・画像でなければ None）。
    pub alpha: Option<Alpha>,
    /// ドット絵として表示するか（実効値。[`asset::effective`]）。
    pub pixel_art: bool,
    /// ドット絵の設定（`asset_settings.pixel_art`。None は自動）。右クリックのメニューのチェックに使う。
    pub pixel_art_setting: Option<bool>,
    /// アニメとして回せるか（実効値）。
    pub animated: bool,
    /// コマの数（実効値。格子があれば帯で使う数）。
    pub frame_count: Option<u32>,
    /// 解析で分かったループ点があるか（音声）。
    pub has_loop: bool,
}

/// 概要の列。素材の表示（市松模様・ドット絵・アニメとループの印）のために、中身ごとの解析の事実（`media_facts`）と
/// アイテムごとの設定（`asset_settings`）を [`SUMMARY_FROM`] の LEFT JOIN で引く（docs/data-model.md §4）。
const SUMMARY_COLUMNS: &str = "i.id, i.hash, i.ext, i.kind, i.width, i.height, i.duration_ms, i.size_bytes, i.title, \
                               i.rating, i.source_site, i.imported_at, i.captured_at, i.thumb_state, i.proxy_state, \
                               i.last_played_at, i.thumb_rev, i.analysis_state, f.alpha, f.pixel_art, f.frame_count, \
                               f.has_loop, s.pixel_art, s.grid_json";
/// `SUMMARY_COLUMNS` の列数（続けて読む列の位置に使う）。
const SUMMARY_LEN: usize = 24;
/// 概要の表：アイテム（主キー）→ 事実（台帳の行の ID `items.blob_id` で、事実の主キー）、設定（item_id の主キー）。
const SUMMARY_FROM: &str = "items i
     LEFT JOIN media_facts f ON f.blob_id = i.blob_id
     LEFT JOIN asset_settings s ON s.item_id = i.id";

/// 0 以上で u32 に収まる整数（ほかは None）。
fn u32_of(v: Option<i64>) -> Option<u32> {
    v.and_then(|v| u32::try_from(v).ok())
}

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<ItemSummary> {
    let hash: Vec<u8> = row.get(1)?;
    let kind: MediaKind = row.get(3)?;
    let width: Option<i64> = row.get(4)?;
    let height: Option<i64> = row.get(5)?;
    let pixel_art_setting = row.get::<_, Option<i64>>(22)?.map(|v| v != 0);
    let grid = row
        .get::<_, Option<String>>(23)?
        .as_deref()
        .and_then(asset::grid_from_json);
    let effective = asset::effective(
        kind,
        (u32_of(width), u32_of(height)),
        asset::EffectiveInputs {
            auto_pixel_art: row.get::<_, Option<i64>>(19)?.map(|v| v != 0),
            frame_count: u32_of(row.get(20)?),
            pixel_art_setting,
            grid: grid.as_ref(),
        },
    );
    Ok(ItemSummary {
        id: row.get(0)?,
        hash: hex(&hash),
        ext: row.get(2)?,
        kind,
        width,
        height,
        duration_ms: row.get(6)?,
        size_bytes: row.get(7)?,
        title: row.get(8)?,
        rating: row.get(9)?,
        source_site: row.get(10)?,
        imported_at: row.get(11)?,
        captured_at: row.get(12)?,
        thumb_state: row.get(13)?,
        proxy_state: row.get(14)?,
        last_played_at: row.get(15)?,
        thumb_rev: row.get(16)?,
        analysis_state: AnalysisState::parse(&row.get::<_, String>(17)?),
        alpha: row.get::<_, Option<i64>>(18)?.and_then(Alpha::from_code),
        pixel_art: effective.pixel_art,
        pixel_art_setting,
        animated: effective.animated,
        frame_count: effective.frame_count,
        has_loop: row.get::<_, Option<i64>>(21)?.is_some_and(|v| v != 0),
    })
}

/// `summaries` の SQL（`n` 個の `?`）。
fn summaries_sql(n: usize) -> String {
    let marks = vec!["?"; n].join(", ");
    format!("SELECT {SUMMARY_COLUMNS} FROM {SUMMARY_FROM} WHERE i.id IN ({marks})")
}

/// `detail` の SQL（概要の列のあとにインスペクタの列）。
fn detail_sql() -> String {
    format!(
        "SELECT {SUMMARY_COLUMNS}, i.mime, i.codec_info, i.original_name, i.original_path, i.source_url, i.note,
                i.trashed_at
         FROM {SUMMARY_FROM} WHERE i.id = ?1"
    )
}

/// 見えている分の詳細を、渡した ID の順で返す（見つからない ID は飛ばす）。
pub fn summaries(conn: &Connection, ids: &[i64]) -> Result<Vec<ItemSummary>> {
    if ids.len() > MAX_SUMMARIES {
        return Err(CoreError::Invalid(format!(
            "一度に取れるのは {MAX_SUMMARIES} 件までです（{} 件）",
            ids.len()
        )));
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(&summaries_sql(ids.len()))?;
    let rows = stmt.query_map(params_from_iter(ids.iter()), summary_from_row)?;
    let mut by_id: HashMap<i64, ItemSummary> = HashMap::with_capacity(ids.len());
    for row in rows {
        let item = row?;
        by_id.insert(item.id, item);
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// 所属する仮想フォルダ（いちばん上からのパス）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRef {
    pub id: i64,
    pub path: Vec<String>,
}

/// インスペクタ用の詳細。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemDetail {
    pub summary: ItemSummary,
    pub mime: Option<String>,
    pub codec_info: Option<String>,
    pub original_name: String,
    pub original_path: Option<String>,
    pub source_url: Option<String>,
    pub note: Option<String>,
    pub trashed_at: Option<i64>,
    pub tags: Vec<TagRef>,
    pub folders: Vec<FolderRef>,
    pub units: Vec<UnitRef>,
}

/// 1 件の詳細。
pub fn detail(conn: &Connection, id: i64) -> Result<ItemDetail> {
    let mut stmt = conn.prepare_cached(&detail_sql())?;
    let found = stmt
        .query_row([id], |row| {
            Ok((
                summary_from_row(row)?,
                row.get::<_, Option<String>>(SUMMARY_LEN)?,
                row.get::<_, Option<String>>(SUMMARY_LEN + 1)?,
                row.get::<_, String>(SUMMARY_LEN + 2)?,
                row.get::<_, Option<String>>(SUMMARY_LEN + 3)?,
                row.get::<_, Option<String>>(SUMMARY_LEN + 4)?,
                row.get::<_, Option<String>>(SUMMARY_LEN + 5)?,
                row.get::<_, Option<i64>>(SUMMARY_LEN + 6)?,
            ))
        })
        .optional()?;
    let Some((
        summary,
        mime,
        codec_info,
        original_name,
        original_path,
        source_url,
        note,
        trashed_at,
    )) = found
    else {
        return Err(CoreError::NotFound(format!("アイテム {id}")));
    };

    let mut tag_stmt = conn.prepare_cached(
        "SELECT t.id, t.namespace, t.name, t.color FROM item_tags m JOIN tags t ON t.id = m.tag_id
         WHERE m.item_id = ?1",
    )?;
    let mut tags = tag_stmt
        .query_map([id], |row| {
            Ok(TagRef {
                id: row.get(0)?,
                namespace: row.get(1)?,
                name: row.get(2)?,
                color: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    tags.sort_by(crate::tags::display_order);

    let mut folder_stmt = conn.prepare_cached(
        "WITH RECURSIVE chain(start_id, id, parent_id, name, depth) AS (
           SELECT f.id, f.id, f.parent_id, f.name, 0 FROM item_folders m JOIN folders f ON f.id = m.folder_id
           WHERE m.item_id = ?1
           UNION ALL
           SELECT c.start_id, p.id, p.parent_id, p.name, c.depth + 1 FROM chain c JOIN folders p ON p.id = c.parent_id
         )
         SELECT start_id, name FROM chain ORDER BY start_id, depth DESC",
    )?;
    let mut folders: Vec<FolderRef> = Vec::new();
    let rows = folder_stmt.query_map([id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (start, name) = row?;
        match folders.last_mut() {
            Some(last) if last.id == start => last.path.push(name),
            _ => folders.push(FolderRef {
                id: start,
                path: vec![name],
            }),
        }
    }

    let units = unit::units_of_item(conn, id)?;

    Ok(ItemDetail {
        summary,
        mime,
        codec_info,
        original_name,
        original_path,
        source_url,
        note,
        trashed_at,
        tags,
        folders,
        units,
    })
}

/// 複数選択のインスペクタに出す集計。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionInfo {
    /// 見つかったアイテムの数。
    pub count: usize,
    /// 全件の評価が同じならその値。
    pub rating: Option<i64>,
    /// 全件のメモが同じならその値（メモなしは `Some(None)`）。
    pub note: Option<Option<String>>,
    /// 付いているタグと、付いている件数（多い順）。
    pub tags: Vec<(TagRef, usize)>,
    /// 入っているフォルダと、入っている件数（多い順）。
    pub folders: Vec<(FolderRef, usize)>,
}

/// 選んだアイテムの評価・メモ・タグ・フォルダをまとめる。
pub fn selection_info(conn: &Connection, ids: &[i64]) -> Result<SelectionInfo> {
    let mut info = SelectionInfo::default();
    let mut tag_counts: HashMap<i64, usize> = HashMap::new();
    let mut folder_counts: HashMap<i64, usize> = HashMap::new();
    let mut first = true;
    let mut item_stmt = conn.prepare_cached("SELECT rating, note FROM items WHERE id = ?1")?;
    let mut tag_stmt = conn.prepare_cached("SELECT tag_id FROM item_tags WHERE item_id = ?1")?;
    let mut folder_stmt =
        conn.prepare_cached("SELECT folder_id FROM item_folders WHERE item_id = ?1")?;
    let mut seen = HashSet::new();
    for id in ids.iter().filter(|id| seen.insert(**id)) {
        let row: Option<(i64, Option<String>)> = item_stmt
            .query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((rating, note)) = row else {
            continue;
        };
        info.count += 1;
        if first {
            info.rating = Some(rating);
            info.note = Some(note);
            first = false;
        } else {
            if info.rating != Some(rating) {
                info.rating = None;
            }
            if info.note.as_ref().is_some_and(|n| *n != note) {
                info.note = None;
            }
        }
        for tag in tag_stmt.query_map([id], |r| r.get::<_, i64>(0))? {
            *tag_counts.entry(tag?).or_insert(0) += 1;
        }
        for folder in folder_stmt.query_map([id], |r| r.get::<_, i64>(0))? {
            *folder_counts.entry(folder?).or_insert(0) += 1;
        }
    }

    let mut tag_ref =
        conn.prepare_cached("SELECT id, namespace, name, color FROM tags WHERE id = ?1")?;
    for (id, n) in tag_counts {
        let tag = tag_ref
            .query_row([id], |r| {
                Ok(TagRef {
                    id: r.get(0)?,
                    namespace: r.get(1)?,
                    name: r.get(2)?,
                    color: r.get(3)?,
                })
            })
            .optional()?;
        if let Some(tag) = tag {
            info.tags.push((tag, n));
        }
    }
    info.tags
        .sort_by(|(a, na), (b, nb)| nb.cmp(na).then_with(|| crate::tags::display_order(a, b)));

    let tree = crate::folders::tree(conn)?;
    info.folders = folder_counts
        .into_iter()
        .map(|(id, n)| {
            (
                FolderRef {
                    id,
                    path: crate::folders::path_of(&tree, id),
                },
                n,
            )
        })
        .collect();
    info.folders
        .sort_by(|(a, na), (b, nb)| nb.cmp(na).then_with(|| a.path.cmp(&b.path)));
    Ok(info)
}

/// 変更する項目（None は変えない。`note` の `Some(None)` はメモを消す）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemPatch {
    /// タイトル（1 件だけのときに変えられる）。
    pub title: Option<String>,
    pub rating: Option<i64>,
    pub note: Option<Option<String>>,
}

/// タイトル・評価・メモを変える（評価とメモは全件に同じ値を入れる）。
pub fn update(conn: &Connection, ids: &[i64], patch: &ItemPatch, now: i64) -> Result<()> {
    if let Some(title) = &patch.title {
        if ids.len() != 1 {
            return Err(CoreError::Invalid(
                "タイトルは 1 件ずつ変えてください".into(),
            ));
        }
        let title = nfc(title.trim());
        if title.is_empty() {
            return Err(CoreError::Invalid("タイトルを入力してください".into()));
        }
        let mut stmt =
            conn.prepare_cached("UPDATE items SET title = ?2, updated_at = ?3 WHERE id = ?1")?;
        stmt.execute(params![ids[0], title, now])?;
    }
    if let Some(rating) = patch.rating {
        if !(0..=5).contains(&rating) {
            return Err(CoreError::Invalid(format!("評価は 0〜5 です（{rating}）")));
        }
        let mut stmt =
            conn.prepare_cached("UPDATE items SET rating = ?2, updated_at = ?3 WHERE id = ?1")?;
        for id in ids {
            stmt.execute(params![id, rating, now])?;
        }
    }
    if let Some(note) = &patch.note {
        let note = note.as_deref().map(str::trim).filter(|n| !n.is_empty());
        let mut stmt =
            conn.prepare_cached("UPDATE items SET note = ?2, updated_at = ?3 WHERE id = ?1")?;
        for id in ids {
            stmt.execute(params![id, note, now])?;
        }
    }
    Ok(())
}

/// ゴミ箱に入れる。入れた件数を返す（すでに入っているものは数えない）。
pub fn trash(conn: &Connection, ids: &[i64], now: i64) -> Result<usize> {
    let mut stmt = conn.prepare_cached(
        "UPDATE items SET trashed_at = ?2, updated_at = ?2 WHERE id = ?1 AND trashed_at IS NULL",
    )?;
    let mut n = 0;
    for id in ids {
        n += stmt.execute(params![id, now])?;
    }
    Ok(n)
}

/// ゴミ箱から戻す。戻した件数を返す。
pub fn restore(conn: &Connection, ids: &[i64], now: i64) -> Result<usize> {
    let mut stmt = conn.prepare_cached(
        "UPDATE items SET trashed_at = NULL, updated_at = ?2 WHERE id = ?1 AND trashed_at IS NOT NULL",
    )?;
    let mut n = 0;
    for id in ids {
        n += stmt.execute(params![id, now])?;
    }
    Ok(n)
}

/// ゴミ箱を空にするときに消したアイテム。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedItem {
    pub hash: [u8; 32],
    pub ext: String,
    pub size_bytes: i64,
}

/// ゴミ箱を空にした DB の側の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrashDeleted {
    /// 消したアイテム。
    pub items: Vec<RemovedItem>,
    /// 使われなくなって解放した台帳の行（このファイルと派生のファイルを、コミットのあとで消す）。
    pub released: Vec<crate::library::ledger::ReleasedBlob>,
    /// 消した文書の部品の数（消したアイテムの版・下書きの文書のうち、ほかから辿れないもの）。
    pub parts: u64,
}

/// ゴミ箱を空にする前に決めること（トランザクション 1）：消すアイテムと、消しうる中身（アイテムの今・版が持つファイル・
/// 版の入力が使うアンカーのファイル）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrashPlan {
    /// 消すアイテム（このあとゴミ箱に入れられたものは、次に空にするまで残す）。
    pub items: Vec<i64>,
    /// 消しうる中身の (hash, 拡張子)。'delete' で予告する（解放されなかったものは、予告だけ消える）。
    pub blobs: Vec<([u8; 32], String)>,
}

/// ゴミ箱を空にする前に、消すアイテムと消しうる中身を集める（ゴミ箱の部分索引 `idx_items_trashed` からたどる。CROSS JOIN で
/// 結ぶ順を決める。統計のない小さな表では、版の表を走査する順を選ぶことがあるため）。
pub fn plan_empty_trash(conn: &Connection) -> Result<TrashPlan> {
    let items: Vec<i64> = {
        let mut stmt =
            conn.prepare_cached("SELECT id FROM items WHERE trashed_at IS NOT NULL ORDER BY id")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    let mut stmt = conn.prepare_cached(
        "SELECT b.hash, b.ext FROM items i CROSS JOIN blobs b ON b.hash = i.hash
          WHERE i.trashed_at IS NOT NULL
         UNION
         SELECT b.hash, b.ext FROM items i
           CROSS JOIN item_versions v ON v.item_id = i.id
           JOIN blobs b ON b.id = v.file_blob_id
          WHERE i.trashed_at IS NOT NULL
         UNION
         SELECT b.hash, b.ext FROM items i
           CROSS JOIN item_versions v ON v.item_id = i.id
           CROSS JOIN item_version_inputs n ON n.version_id = v.id
           JOIN edit_anchors a ON a.id = n.anchor_id
           JOIN blobs b ON b.id = a.blob_id
          WHERE i.trashed_at IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut blobs = Vec::new();
    for row in rows {
        let (hash, ext) = row?;
        if let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) {
            blobs.push((hash, ext));
        }
    }
    Ok(TrashPlan { items, blobs })
}

/// ゴミ箱のアイテム `ids`（[`plan_empty_trash`] で決めたもの。もうゴミ箱にないものは消さない）の行を消し、使われなくなった
/// 台帳の行を同じトランザクションで解放する（docs/data-model.md §4「保存と版（M12）」。中身ごとの事実・結果は CASCADE で
/// 消える）。編集したアイテム（M12）は、版の行（CASCADE で消える）が持つファイル（元・猶予中の版・照合が合わずに残した版）と、
/// 版の入力が使うアンカーも、ほかから参照されなければ解放し（利用者の決定 6：ゴミ箱そのものが猶予の役をするので、照合しない）、
/// 版・下書きの文書の部品のうち、ほかから辿れないものも消す。実ファイルと派生のファイルは `Library::empty_trash` がコミットの
/// あとで消す（'delete' の予告を先にコミットしてある）。
pub fn delete_trashed(conn: &Connection, ids: &[i64]) -> Result<TrashDeleted> {
    let mut items = Vec::with_capacity(ids.len());
    let mut hashes: Vec<[u8; 32]> = Vec::new();
    let mut anchors: Vec<i64> = Vec::new();
    let mut docs: Vec<i64> = Vec::new();
    let mut item_of = conn.prepare_cached(
        "SELECT hash, ext, size_bytes FROM items WHERE id = ?1 AND trashed_at IS NOT NULL",
    )?;
    let mut files_of = conn.prepare_cached(
        "SELECT b.hash FROM item_versions v JOIN blobs b ON b.id = v.file_blob_id WHERE v.item_id = ?1",
    )?;
    let mut anchors_of = conn.prepare_cached(
        "SELECT n.anchor_id FROM item_versions v CROSS JOIN item_version_inputs n ON n.version_id = v.id
          WHERE v.item_id = ?1 AND n.anchor_id IS NOT NULL",
    )?;
    let mut docs_of = conn.prepare_cached(
        "SELECT doc_id FROM item_versions WHERE item_id = ?1 AND doc_id IS NOT NULL
         UNION SELECT doc_id FROM edit_drafts WHERE item_id = ?1",
    )?;
    let mut delete = conn.prepare_cached("DELETE FROM items WHERE id = ?1")?;
    for &id in ids {
        let row: Option<(Vec<u8>, String, i64)> = item_of
            .query_row([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .optional()?;
        let Some((hash, ext, size_bytes)) = row else {
            continue;
        };
        let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
            continue;
        };
        for file in files_of.query_map([id], |row| row.get::<_, Vec<u8>>(0))? {
            if let Ok(file) = <[u8; 32]>::try_from(file?.as_slice()) {
                hashes.push(file);
            }
        }
        for anchor in anchors_of.query_map([id], |row| row.get::<_, i64>(0))? {
            anchors.push(anchor?);
        }
        for doc in docs_of.query_map([id], |row| row.get::<_, i64>(0))? {
            docs.push(doc?);
        }
        delete.execute([id])?;
        hashes.push(hash);
        items.push(RemovedItem {
            hash,
            ext,
            size_bytes,
        });
    }
    anchors.sort_unstable();
    anchors.dedup();
    hashes.extend(crate::edit::anchors::sweep_unreferenced(conn, &anchors)?);
    hashes.sort_unstable();
    hashes.dedup();
    let released = crate::library::ledger::release(conn, &hashes)?;
    docs.sort_unstable();
    docs.dedup();
    let live = crate::edit::save::gc_roots(
        conn,
        crate::time::now_ms().saturating_sub(crate::edit::gc::GRACE_MS),
    )?;
    let parts = crate::edit::gc::sweep_parts(conn, crate::edit::gc::Scope::Roots(&docs), &live)?;
    Ok(TrashDeleted {
        items,
        released,
        parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_len_matches_columns() {
        assert_eq!(SUMMARY_COLUMNS.split(',').count(), SUMMARY_LEN);
    }
    use crate::db::{query_plan, test_conn};

    fn ctx(utc_offset_minutes: i32) -> QueryContext {
        QueryContext {
            now_ms: 0,
            utc_offset_minutes,
        }
    }

    fn insert(conn: &Connection, id: i64, w: Option<i64>, h: Option<i64>, at: i64) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, title, original_name, imported_at, updated_at)
             VALUES (?1, ?2, 'jpg', 'image', 10, ?3, ?4, ?5, ?5, ?6, ?6)",
            rusqlite::params![id, vec![id as u8; 32], w, h, format!("t{id}"), at],
        )
        .unwrap();
    }

    #[test]
    fn aspect_codes() {
        assert_eq!(aspect_code(Some(1600), Some(900)), 1778);
        assert_eq!(aspect_code(Some(900), Some(1600)), 563);
        assert_eq!(aspect_code(None, Some(1)), 1000);
        assert_eq!(aspect_code(Some(0), Some(10)), 1000);
        assert_eq!(aspect_code(Some(1_000_000), Some(1)), u16::MAX);
    }

    #[test]
    fn ids_come_newest_first_with_day_groups() {
        let conn = test_conn();
        // 2 日分（UTC で 1 日目に 2 件、2 日目に 1 件）
        insert(&conn, 1, Some(1500), Some(1000), DAY_MS + 1000);
        insert(&conn, 2, Some(1000), Some(1000), DAY_MS + 2000);
        insert(&conn, 3, None, None, 2 * DAY_MS + 10);
        insert(&conn, 4, Some(1), Some(1), 3 * DAY_MS);
        conn.execute("UPDATE items SET trashed_at = 1 WHERE id = 4", [])
            .unwrap();

        let list = query_ids(&conn, &ItemQuery::default(), &ctx(0)).unwrap();
        assert_eq!(list.ids, vec![3, 2, 1]);
        assert_eq!(list.aspects, vec![1000, 1000, 1500]);
        assert_eq!(
            list.groups,
            vec![DayGroup { start: 0, day: 2 }, DayGroup { start: 1, day: 1 }]
        );

        // 時差 -1 時間なら、どれも 1 日ずつ前の日になる（3 は 1 日目、1・2 は 0 日目）
        let shifted = query_ids(&conn, &ItemQuery::default(), &ctx(-60)).unwrap();
        assert_eq!(
            shifted.groups,
            vec![DayGroup { start: 0, day: 1 }, DayGroup { start: 1, day: 0 }]
        );

        let trash = query_ids(
            &conn,
            &ItemQuery {
                view: ItemView::Trash,
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap();
        assert_eq!(trash.ids, vec![4]);
    }

    #[test]
    fn encodes_little_endian_layout() {
        let list = IdList {
            ids: vec![7, 9],
            aspects: vec![1500, 1000],
            groups: vec![DayGroup { start: 0, day: -1 }],
        };
        let bytes = list.encode();
        assert_eq!(bytes.len(), 4 + 8 + 4 + 4 + 4 + 4);
        assert_eq!(&bytes[0..4], &2u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &7u32.to_le_bytes());
        assert_eq!(&bytes[12..14], &1500u16.to_le_bytes());
        assert_eq!(&bytes[16..20], &1u32.to_le_bytes());
        assert_eq!(&bytes[24..28], &(-1i32).to_le_bytes());
    }

    #[test]
    fn summaries_follow_requested_order_and_limit() {
        let conn = test_conn();
        for id in 1..=3 {
            insert(&conn, id, Some(10), Some(10), id * 1000);
        }
        let items = summaries(&conn, &[3, 99, 1]).unwrap();
        assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![3, 1]);
        assert_eq!(items[0].hash, hex(&[3u8; 32]));
        assert!(summaries(&conn, &vec![1; MAX_SUMMARIES + 1]).is_err());
    }

    /// タイルの詳細（`summaries`・`detail`）は、事実を台帳の行の ID（`items.blob_id`）から、設定をアイテムの主キーから引く。
    /// 200 件の IN でも items・media_facts・asset_settings のどれも全件走査しない（台帳 blobs は通らない）。
    #[test]
    fn summaries_join_facts_and_settings_without_scanning() {
        let conn = test_conn();
        for n in [1, MAX_SUMMARIES] {
            let marks = vec!["1"; n].join(", ");
            let sql = summaries_sql(n).replace(&vec!["?"; n].join(", "), &marks);
            let plan = query_plan(&conn, &sql);
            assert!(!plan.contains("SCAN"), "{n} 件:\n{plan}");
            assert!(!plan.contains("blobs"), "{n} 件:\n{plan}");
            for (table, how) in [("f", "INTEGER PRIMARY KEY"), ("s", "INTEGER PRIMARY KEY")] {
                assert!(
                    plan.contains(&format!("SEARCH {table} USING")) && plan.contains(how),
                    "{table} を {how} で引く:\n{plan}"
                );
            }
        }
        let plan = query_plan(&conn, &detail_sql().replace("?1", "1"));
        assert!(!plan.contains("SCAN"), "{plan}");
    }

    /// 解析の事実（解析の書き手 `analysis::store::write` で入れる）と設定（素材の欄の書き手 `asset::set_settings`）を
    /// 引き、実効値は素材の欄（`asset::get`）と同じ規則（`asset::effective_pixel_art`）で決まる。設定 3 通り ×
    /// 自動 4 通り（未解析・材料が足りない・いいえ・はい）のすべてで照らし、`detail` の概要も同じ値。
    #[test]
    fn summaries_follow_the_effective_values_of_the_asset_panel() {
        use crate::analysis::facts::{FACTS_V, MediaFacts, PIXEL_RULE};
        use crate::analysis::store::{ItemWrite, Write, write};
        use crate::asset::{
            Change, GridSetting, SettingsPatch, TRISTATE, effective_pixel_art, set_settings,
        };

        let conn = test_conn();
        let put = |id: i64, kind: &str, ext: &str, size: Option<(i64, i64)>| {
            conn.execute(
                "INSERT INTO items (id, hash, ext, kind, size_bytes, width, height, title, original_name,
                                    imported_at, updated_at, analysis_state)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, 't', 't', 0, 0, 'pending')",
                params![
                    id,
                    vec![u8::try_from(id).unwrap(); 32],
                    ext,
                    kind,
                    size.map(|s| s.0),
                    size.map(|s| s.1)
                ],
            )
            .unwrap();
        };
        let facts = |id: i64, facts: MediaFacts| {
            let ready = ItemWrite {
                id,
                hash: [u8::try_from(id).unwrap(); 32],
                write: Write::Ready {
                    results: Vec::new(),
                    keep: None,
                    facts: Box::new(MediaFacts {
                        facts_v: FACTS_V,
                        ..facts
                    }),
                    stamp: None,
                },
            };
            assert_eq!(write(&conn, &[ready], 0).unwrap().written, 1);
        };
        let patch = |ids: &[i64], patch: SettingsPatch| {
            let n = u32::try_from(ids.len()).unwrap();
            assert_eq!(set_settings(&conn, ids, &patch, 0).unwrap(), n);
        };

        // ドット絵：設定 3 通り × 自動 4 通り（None は未解析、Some(None) は解析済みで材料が足りない）
        let autos: [Option<Option<bool>>; 4] =
            [None, Some(None), Some(Some(false)), Some(Some(true))];
        let mut cases = Vec::new();
        let mut id = 0_i64;
        for setting in TRISTATE {
            for auto in autos {
                id += 1;
                put(id, "image", "png", Some((64, 64)));
                if let Some(auto) = auto {
                    facts(
                        id,
                        MediaFacts {
                            alpha: Some(1),
                            pixel_art: auto,
                            pixel_rule: Some(PIXEL_RULE),
                            frame_count: Some(1),
                            ..MediaFacts::default()
                        },
                    );
                }
                if let Some(value) = setting {
                    patch(
                        &[id],
                        SettingsPatch {
                            pixel_art: Some(Change::Set(value)),
                            ..SettingsPatch::default()
                        },
                    );
                }
                cases.push((id, setting, auto));
            }
        }
        let ids: Vec<i64> = cases.iter().map(|c| c.0).collect();
        let rows = summaries(&conn, &ids).unwrap();
        assert_eq!(rows.len(), cases.len());
        for (row, (id, setting, auto)) in rows.iter().zip(&cases) {
            let want = effective_pixel_art(*setting, auto.flatten());
            assert_eq!(row.pixel_art, want, "{id}: 設定 {setting:?}・自動 {auto:?}");
            assert_eq!(row.pixel_art_setting, *setting, "{id}");
            let panel = crate::asset::get(&conn, *id).unwrap();
            assert_eq!(row.pixel_art, panel.effective.pixel_art, "{id}");
            assert_eq!(row.animated, panel.effective.animated, "{id}");
            assert_eq!(row.frame_count, panel.effective.frame_count, "{id}");
            let analyzed = auto.is_some();
            assert_eq!(row.alpha, analyzed.then_some(Alpha::Binary), "{id}");
            assert_eq!(
                row.analysis_state,
                if analyzed {
                    AnalysisState::Ready
                } else {
                    AnalysisState::Pending
                },
                "{id}"
            );
            assert_eq!(&detail(&conn, *id).unwrap().summary, row, "{id}");
        }

        // アニメ：解析のコマの数、格子（帯で使う数）。半透明。サムネイルの作り直しの回数
        put(40, "image", "gif", Some((32, 32)));
        facts(
            40,
            MediaFacts {
                alpha: Some(2),
                frame_count: Some(8),
                ..MediaFacts::default()
            },
        );
        put(41, "image", "png", Some((64, 32)));
        facts(
            41,
            MediaFacts {
                alpha: Some(0),
                frame_count: Some(1),
                ..MediaFacts::default()
            },
        );
        patch(
            &[41],
            SettingsPatch {
                grid: Some(Change::Set(GridSetting {
                    cell_w: 16,
                    cell_h: 16,
                    offset_x: 0,
                    offset_y: 0,
                    gap_x: 0,
                    gap_y: 0,
                    count: Some(5),
                })),
                ..SettingsPatch::default()
            },
        );
        conn.execute(
            "UPDATE items SET thumb_state = 'stale', thumb_rev = 3 WHERE id = 40",
            [],
        )
        .unwrap();
        // 音声：ループ点。動画：解析しない
        put(42, "audio", "ogg", None);
        facts(
            42,
            MediaFacts {
                has_loop: Some(true),
                loop_start: Some(10),
                loop_end: Some(20),
                ..MediaFacts::default()
            },
        );
        put(43, "audio", "wav", None);
        facts(
            43,
            MediaFacts {
                has_loop: Some(false),
                ..MediaFacts::default()
            },
        );
        put(44, "video", "mp4", Some((16, 9)));
        conn.execute(
            "UPDATE items SET analysis_state = 'not_needed' WHERE id = 44",
            [],
        )
        .unwrap();

        let rows = summaries(&conn, &[40, 41, 42, 43, 44]).unwrap();
        let [gif, sheet, looped, plain, video] = rows.as_slice() else {
            panic!("5 件: {rows:?}");
        };
        assert_eq!(
            (gif.alpha, gif.animated, gif.frame_count),
            (Some(Alpha::Partial), true, Some(8))
        );
        assert_eq!((gif.thumb_state.as_str(), gif.thumb_rev), ("stale", 3));
        assert_eq!(
            (sheet.alpha, sheet.animated, sheet.frame_count),
            (Some(Alpha::None), true, Some(5)),
            "格子のコマの数"
        );
        assert_eq!(
            (looped.has_loop, looped.alpha, looped.animated),
            (true, None, false)
        );
        assert!(!plain.has_loop);
        assert_eq!(
            (
                video.analysis_state,
                video.alpha,
                video.pixel_art,
                video.has_loop
            ),
            (AnalysisState::NotNeeded, None, false, false)
        );
        for row in &rows {
            let panel = crate::asset::get(&conn, row.id).unwrap();
            assert_eq!(
                (row.pixel_art, row.animated, row.frame_count),
                (
                    panel.effective.pixel_art,
                    panel.effective.animated,
                    panel.effective.frame_count
                ),
                "{}",
                row.id
            );
            assert_eq!(&detail(&conn, row.id).unwrap().summary, row);
        }
    }

    #[test]
    fn detail_includes_tags_folders_and_units() {
        let conn = test_conn();
        insert(&conn, 1, Some(10), Some(10), 0);
        conn.execute_batch(
            "INSERT INTO tags (id, namespace, name, created_at) VALUES (1, '', '夕景', 0), (2, '作者', 'もりの', 0);
             INSERT INTO item_tags (tag_id, item_id) VALUES (1, 1), (2, 1);
             INSERT INTO folders (id, parent_id, name, pos, created_at) VALUES (1, NULL, '写真', 1, 0), (2, 1, '2026 旅行', 1, 0);
             INSERT INTO item_folders (folder_id, item_id) VALUES (2, 1);
             INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (5, 'set', '鎌倉', 'manual', 0, 0);
             INSERT INTO unit_items (unit_id, item_id, pos) VALUES (5, 1, 1);",
        )
        .unwrap();
        let d = detail(&conn, 1).unwrap();
        assert_eq!(
            d.tags
                .iter()
                .map(|t| (t.namespace.as_str(), t.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("作者", "もりの"), ("", "夕景")]
        );
        assert_eq!(
            d.folders,
            vec![FolderRef {
                id: 2,
                path: vec!["写真".into(), "2026 旅行".into()]
            }]
        );
        assert_eq!(d.units[0].name, "鎌倉");
        assert!(matches!(detail(&conn, 42), Err(CoreError::NotFound(_))));
    }

    #[test]
    fn list_reads_in_index_order() {
        let conn = test_conn();
        let plan = query_plan(
            &conn,
            "SELECT i.id, i.width, i.height, i.imported_at FROM items i
             WHERE i.trashed_at IS NULL ORDER BY i.imported_at DESC, i.id DESC",
        );
        assert!(plan.contains("idx_items_imported"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }

    fn ids(conn: &Connection, view: ItemView, collapse: bool) -> Vec<u32> {
        query_ids(
            conn,
            &ItemQuery {
                view,
                collapse_units: collapse,
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap()
        .ids
    }

    /// 1〜6 を 1 日ずつずらして追加。1・2 は写真/旅行、3 は写真。1 にタグ。
    fn library() -> Connection {
        let conn = test_conn();
        for id in 1..=6 {
            insert(&conn, id, Some(1600), Some(900), id * DAY_MS);
        }
        let photos = crate::folders::create(&conn, None, "写真", 0).unwrap();
        let trip = crate::folders::create(&conn, Some(photos), "旅行", 0).unwrap();
        crate::folders::set_items(&conn, &[1, 2], &[trip], &[]).unwrap();
        crate::folders::set_items(&conn, &[3], &[photos], &[]).unwrap();
        crate::tags::apply(&conn, &[1], &["夕景".into()], &[], 0).unwrap();
        conn
    }

    #[test]
    fn views_filter_and_order_items() {
        let conn = library();
        assert_eq!(ids(&conn, ItemView::All, false), vec![6, 5, 4, 3, 2, 1]);
        assert_eq!(
            ids(&conn, ItemView::Recent { since: 5 * DAY_MS }, false),
            vec![6, 5]
        );
        assert_eq!(ids(&conn, ItemView::Unsorted, false), vec![6, 5, 4]);
        assert_eq!(ids(&conn, ItemView::Untagged, false), vec![6, 5, 4, 3, 2]);
        // フォルダは子孫フォルダの分も含める
        assert_eq!(ids(&conn, ItemView::Folder { id: 1 }, false), vec![3, 2, 1]);
        assert_eq!(ids(&conn, ItemView::Folder { id: 2 }, false), vec![2, 1]);
        assert!(
            query_ids(
                &conn,
                &ItemQuery {
                    view: ItemView::Folder { id: 99 },
                    ..ItemQuery::default()
                },
                &ctx(0)
            )
            .is_err()
        );

        trash(&conn, &[2], 10 * DAY_MS).unwrap();
        assert_eq!(ids(&conn, ItemView::Folder { id: 1 }, false), vec![3, 1]);
        assert_eq!(ids(&conn, ItemView::Trash, false), vec![2]);
    }

    #[test]
    fn random_view_is_a_seeded_permutation_without_headings() {
        let conn = library();
        let list = |seed| {
            query_ids(
                &conn,
                &ItemQuery {
                    view: ItemView::Random { seed },
                    ..ItemQuery::default()
                },
                &ctx(0),
            )
            .unwrap()
        };
        let a = list(7);
        assert_eq!(a, list(7));
        assert!(a.groups.is_empty());
        let mut sorted = a.ids.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3, 4, 5, 6]);
        assert!((0..20).any(|seed| list(seed).ids != a.ids));
    }

    #[test]
    fn unit_view_follows_unit_order() {
        let conn = library();
        let unit = crate::unit::create_set(&conn, "u", &[4, 1, 6], 0).unwrap();
        let list = query_ids(
            &conn,
            &ItemQuery {
                view: ItemView::Unit { id: unit },
                collapse_units: true,
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap();
        assert_eq!(list.ids, vec![4, 1, 6]);
        assert!(list.groups.is_empty());
    }

    #[test]
    fn collapses_set_units_at_their_first_member() {
        let conn = library();
        let a = crate::unit::create_set(&conn, "a", &[2, 4], 0).unwrap() as u32;
        let b = crate::unit::create_set(&conn, "b", &[4, 1], 0).unwrap() as u32;
        // 6 5 [4→a,b] 3 (2→a は済み) (1→b は済み)
        assert_eq!(
            ids(&conn, ItemView::All, true),
            vec![6, 5, LIST_FLAG | a, LIST_FLAG | b, 3]
        );
        // 絞り込んだ一覧では、残ったメンバーの位置に出る
        assert_eq!(
            ids(&conn, ItemView::Folder { id: 2 }, true),
            vec![LIST_FLAG | a, LIST_FLAG | b]
        );
        // ゴミ箱のメンバーではまとめない
        trash(&conn, &[4], 10 * DAY_MS).unwrap();
        assert_eq!(
            ids(&conn, ItemView::All, true),
            vec![6, 5, 3, LIST_FLAG | a, LIST_FLAG | b]
        );
        // 見出しはまとめた後の並びで付く
        let list = query_ids(
            &conn,
            &ItemQuery {
                view: ItemView::All,
                collapse_units: true,
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap();
        assert_eq!(list.groups.len(), 5); // 6・5・3・2（a）・1（b）の日
        assert_eq!(list.aspects[3], 1778);
    }

    #[test]
    fn counts_each_sidebar_view() {
        let conn = library();
        trash(&conn, &[6], 0).unwrap();
        assert_eq!(
            counts(&conn, 4 * DAY_MS).unwrap(),
            LibraryCounts {
                all: 5,
                recent: 2,
                unsorted: 2,
                untagged: 4,
                trash: 1,
                captures: 0
            }
        );
    }

    #[test]
    fn edits_titles_ratings_and_notes() {
        let conn = library();
        update(
            &conn,
            &[1],
            &ItemPatch {
                title: Some(" 夕方の海 ".into()),
                ..ItemPatch::default()
            },
            9,
        )
        .unwrap();
        update(
            &conn,
            &[1, 2],
            &ItemPatch {
                rating: Some(4),
                note: Some(Some("構図".into())),
                ..ItemPatch::default()
            },
            9,
        )
        .unwrap();
        let d = detail(&conn, 2).unwrap();
        assert_eq!((d.summary.rating, d.note.as_deref()), (4, Some("構図")));
        assert_eq!(detail(&conn, 1).unwrap().summary.title, "夕方の海");
        update(
            &conn,
            &[2],
            &ItemPatch {
                note: Some(Some(" ".into())),
                ..ItemPatch::default()
            },
            9,
        )
        .unwrap();
        assert_eq!(detail(&conn, 2).unwrap().note, None);

        let bad = |ids: &[i64], patch: ItemPatch| update(&conn, ids, &patch, 9).is_err();
        assert!(bad(
            &[1, 2],
            ItemPatch {
                title: Some("x".into()),
                ..ItemPatch::default()
            }
        ));
        assert!(bad(
            &[1],
            ItemPatch {
                title: Some(" ".into()),
                ..ItemPatch::default()
            }
        ));
        assert!(bad(
            &[1],
            ItemPatch {
                rating: Some(9),
                ..ItemPatch::default()
            }
        ));
    }

    #[test]
    fn trash_restore_and_delete_rows() {
        let conn = library();
        let unit = crate::unit::create_set(&conn, "u", &[1, 2], 0).unwrap();
        assert_eq!(trash(&conn, &[1, 2, 1], 5).unwrap(), 2);
        assert_eq!(trash(&conn, &[1], 6).unwrap(), 0);
        assert_eq!(restore(&conn, &[2], 7).unwrap(), 1);
        assert_eq!(detail(&conn, 1).unwrap().trashed_at, Some(5));

        let plan = plan_empty_trash(&conn).unwrap();
        assert_eq!(plan.items, vec![1]);
        assert_eq!(plan.blobs, vec![([1u8; 32], "jpg".to_string())]);
        // 決めたあとにゴミ箱から戻したものは消さない
        trash(&conn, &[2], 8).unwrap();
        restore(&conn, &[2], 9).unwrap();
        let removed = delete_trashed(&conn, &[1, 2]).unwrap();
        assert_eq!(removed.items.len(), 1);
        assert_eq!(removed.items[0].hash, [1u8; 32]);
        // 使われなくなった台帳の行も同じトランザクションで解放する
        assert_eq!(removed.released.len(), 1);
        assert_eq!(removed.released[0].hash, [1u8; 32]);
        let ledger: i64 = conn
            .query_row(
                "SELECT count(*) FROM blobs WHERE hash = ?1",
                [&[1u8; 32][..]],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ledger, 0);
        assert!(matches!(detail(&conn, 1), Err(CoreError::NotFound(_))));
        // 所属（フォルダ・タグ・ユニット）も消え、ユニットには残りがいる
        let left: i64 = conn
            .query_row("SELECT (SELECT count(*) FROM item_folders WHERE item_id = 1) + (SELECT count(*) FROM item_tags WHERE item_id = 1)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
        assert_eq!(crate::unit::members(&conn, unit).unwrap(), vec![2]);
    }

    /// 編集したアイテムでゴミ箱を空にすると、版が持つファイル（猶予中の版）とアンカーの台帳の行も解放し（決定 6）、版の文書の
    /// 部品も片付ける。ほかのアイテムの版が持つ同じ中身（別名で保存した B の版 0 が持つ A の元）と、B の版の文書は残し、参照
    /// されない台帳の行は 0。予告する中身は、消しうるもの（A の今・版のファイル・アンカー）すべて。
    #[test]
    fn emptying_the_trash_releases_versions_and_anchors_of_edited_items() {
        use crate::edit::testing::{insert_anchor, insert_item, insert_version};
        let conn = test_conn();
        let (a, b) = (
            insert_item(&conn, 1, [1; 32]),
            insert_item(&conn, 2, [2; 32]),
        );
        let mine = insert_anchor(&conn, [10; 32], [20; 32]);
        let theirs = insert_anchor(&conn, [10; 32], [21; 32]);
        // A：元 [10]・猶予中の版 1 [11]・今の版 2 [1]（版 1 と 2 の入力がアンカーを使う）
        insert_version(&conn, a, 0, [10; 32], "hold", None);
        insert_version(&conn, a, 1, [11; 32], "waiting", Some(mine));
        insert_version(&conn, a, 2, [1; 32], "hold", Some(mine));
        // B：A から別名で保存（版 0 は A の元 [10]、版 1 は今 [2]、別のアンカー）
        insert_version(&conn, b, 0, [10; 32], "hold", None);
        insert_version(&conn, b, 1, [2; 32], "hold", Some(theirs));
        trash(&conn, &[a], 5).unwrap();
        let plan = plan_empty_trash(&conn).unwrap();
        assert_eq!(plan.items, vec![a]);
        let mut planned: Vec<[u8; 32]> = plan.blobs.iter().map(|(h, _)| *h).collect();
        planned.sort_unstable();
        assert_eq!(planned, vec![[1; 32], [10; 32], [11; 32], [20; 32]]);
        let docs_before: i64 = conn
            .query_row("SELECT count(*) FROM edit_objects", [], |r| r.get(0))
            .unwrap();
        let removed = delete_trashed(&conn, &plan.items).unwrap();
        assert_eq!(removed.items.len(), 1);
        // A の版 1・2 の文書は空の文書で、B の版 1 の文書と同じ部品なので残る
        assert_eq!(removed.parts, 0);
        assert_eq!(
            conn.query_row("SELECT count(*) FROM edit_objects", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            docs_before
        );
        let released: Vec<[u8; 32]> = removed.released.iter().map(|r| r.hash).collect();
        assert_eq!(released, vec![[1; 32], [11; 32], [20; 32]]);
        assert_eq!(
            removed
                .released
                .iter()
                .map(|r| r.ext.as_str())
                .collect::<Vec<_>>(),
            ["png", "png", "wav"]
        );
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT count(*) FROM item_versions WHERE item_id = 1"),
            0
        );
        assert_eq!(
            count("SELECT count(*) FROM item_versions WHERE item_id = 2"),
            2
        );
        assert_eq!(count("SELECT count(*) FROM edit_anchors"), 1);
        assert_eq!(crate::library::ledger::unreferenced(&conn).unwrap(), 0);
        // 残った台帳の行：B の今 [2]・A の元 [10]（B の版 0 が持つ）・B のアンカー [21]
        let mut left: Vec<Vec<u8>> = {
            let mut stmt = conn.prepare("SELECT hash FROM blobs").unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        left.sort();
        assert_eq!(left, vec![vec![2; 32], vec![10; 32], vec![21; 32]]);
        assert!(crate::db::violations(&conn).unwrap().is_empty());
        assert!(
            crate::edit::versions::check_invariants(&conn)
                .unwrap()
                .is_empty()
        );
        // 予告する中身を集める問い合わせは、ゴミ箱の部分索引からたどる（items・版を走査しない）
        let plan = query_plan(
            &conn,
            "SELECT b.hash, b.ext FROM items i CROSS JOIN blobs b ON b.hash = i.hash
              WHERE i.trashed_at IS NOT NULL
             UNION
             SELECT b.hash, b.ext FROM items i
               CROSS JOIN item_versions v ON v.item_id = i.id
               JOIN blobs b ON b.id = v.file_blob_id
              WHERE i.trashed_at IS NOT NULL
             UNION
             SELECT b.hash, b.ext FROM items i
               CROSS JOIN item_versions v ON v.item_id = i.id
               CROSS JOIN item_version_inputs n ON n.version_id = v.id
               JOIN edit_anchors a ON a.id = n.anchor_id
               JOIN blobs b ON b.id = a.blob_id
              WHERE i.trashed_at IS NOT NULL",
        );
        assert!(plan.contains("idx_items_trashed"), "{plan}");
        assert!(
            !plan.contains("SCAN i") && !plan.contains("SCAN v") && !plan.contains("SCAN b"),
            "{plan}"
        );
        // アイテムごとの問い合わせは主キー・版の一意の索引・下書きの主キーで引く
        for sql in [
            "SELECT hash, ext, size_bytes FROM items WHERE id = 1 AND trashed_at IS NOT NULL",
            "SELECT b.hash FROM item_versions v JOIN blobs b ON b.id = v.file_blob_id WHERE v.item_id = 1",
            "SELECT n.anchor_id FROM item_versions v CROSS JOIN item_version_inputs n ON n.version_id = v.id
              WHERE v.item_id = 1 AND n.anchor_id IS NOT NULL",
            "SELECT doc_id FROM item_versions WHERE item_id = 1 AND doc_id IS NOT NULL
             UNION SELECT doc_id FROM edit_drafts WHERE item_id = 1",
        ] {
            let plan = query_plan(&conn, sql);
            assert!(!plan.contains("SCAN"), "{sql}\n{plan}");
        }
    }

    #[test]
    fn detail_lists_folder_units_too() {
        let conn = library();
        let unit = crate::unit::create_folder(&conn, "写真", &[(1, true)], 0).unwrap();
        let d = detail(&conn, 2).unwrap();
        assert_eq!(d.units.iter().map(|u| u.id).collect::<Vec<_>>(), vec![unit]);
        assert_eq!(d.tags.len(), 0);
        assert_eq!(detail(&conn, 1).unwrap().tags[0].name, "夕景");
    }

    #[test]
    fn filtered_views_use_indexes() {
        let conn = test_conn();
        for sql in [
            "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND NOT EXISTS (SELECT 1 FROM item_folders m WHERE m.item_id = i.id) ORDER BY i.imported_at DESC, i.id DESC",
            "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND NOT EXISTS (SELECT 1 FROM item_tags m WHERE m.item_id = i.id) ORDER BY i.imported_at DESC, i.id DESC",
            "SELECT i.id FROM items i WHERE i.trashed_at IS NULL AND i.imported_at >= 0 ORDER BY i.imported_at DESC, i.id DESC",
        ] {
            let plan = query_plan(&conn, sql);
            assert!(plan.contains("idx_items_imported"), "{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
            assert!(!plan.contains("SCAN m"), "{plan}");
        }
    }

    #[test]
    fn summarizes_a_selection() {
        let conn = library();
        update(
            &conn,
            &[1, 2],
            &ItemPatch {
                rating: Some(3),
                note: Some(Some("a".into())),
                ..ItemPatch::default()
            },
            0,
        )
        .unwrap();
        crate::tags::apply(&conn, &[2], &["夕景".into(), "海".into()], &[], 0).unwrap();
        let info = selection_info(&conn, &[1, 2, 2, 99]).unwrap();
        assert_eq!(info.count, 2);
        assert_eq!(info.rating, Some(3));
        assert_eq!(info.note, Some(Some("a".into())));
        let tags: Vec<(&str, usize)> = info
            .tags
            .iter()
            .map(|(t, n)| (t.name.as_str(), *n))
            .collect();
        assert_eq!(tags, vec![("夕景", 2), ("海", 1)]);
        assert_eq!(info.folders.len(), 1);
        assert_eq!(info.folders[0].0.path, vec!["写真", "旅行"]);
        assert_eq!(info.folders[0].1, 2);

        let mixed = selection_info(&conn, &[1, 3]).unwrap();
        assert_eq!((mixed.rating, mixed.note), (None, None));
        let none = selection_info(&conn, &[3, 4]).unwrap();
        assert_eq!((none.rating, none.note), (Some(0), Some(None)));
    }

    fn search(conn: &Connection, view: ItemView, filter: &str, sort: ItemSort) -> Vec<u32> {
        let filter = crate::query::parse(filter).unwrap().expr;
        query_ids(
            conn,
            &ItemQuery {
                view,
                filter: Some(filter),
                sort,
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap()
        .ids
    }

    #[test]
    fn filters_within_the_current_view() {
        let conn = library();
        conn.execute_batch(
            "UPDATE items SET kind = 'video', duration_ms = 1000 WHERE id IN (2, 5);
             UPDATE items SET rating = 4 WHERE id IN (1, 5, 6);",
        )
        .unwrap();
        let sort = ItemSort::default();
        assert_eq!(search(&conn, ItemView::All, "評価>=4", sort), vec![6, 5, 1]);
        // フォルダの中で絞る（写真 = 1, 2, 3）
        assert_eq!(
            search(&conn, ItemView::Folder { id: 1 }, "種類:動画", sort),
            vec![2]
        );
        assert_eq!(
            search(&conn, ItemView::Unsorted, "評価>=4", sort),
            vec![6, 5]
        );
        // ユニットの中で絞っても、ユニットの並び順のまま
        let unit = crate::unit::create_set(&conn, "u", &[5, 1, 2], 0).unwrap();
        assert_eq!(
            search(&conn, ItemView::Unit { id: unit }, "評価>=4", sort),
            vec![5, 1]
        );
        // 夕景は 1 にだけ付いている
        assert_eq!(
            search(&conn, ItemView::All, "-夕景 種類:画像", sort),
            vec![6, 4, 3]
        );
    }

    #[test]
    fn smart_folders_are_views() {
        let conn = library();
        conn.execute("UPDATE items SET rating = 5 WHERE id IN (2, 4)", [])
            .unwrap();
        let id = crate::smart::save(&conn, "高評価", "評価>=5", 0).unwrap();
        assert_eq!(ids(&conn, ItemView::Smart { id }, false), vec![4, 2]);
        // スマートフォルダの条件と検索の条件は AND
        assert_eq!(
            search(
                &conn,
                ItemView::Smart { id },
                "フォルダ:写真",
                ItemSort::default()
            ),
            vec![2]
        );
        assert!(matches!(
            query_ids(
                &conn,
                &ItemQuery {
                    view: ItemView::Smart { id: 99 },
                    ..ItemQuery::default()
                },
                &ctx(0)
            ),
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn sorts_by_each_key() {
        let conn = library();
        conn.execute_batch(
            "UPDATE items SET title = 'p' || ((7 - id) * 5) WHERE id <= 6;
             UPDATE blobs SET size_bytes = (SELECT i.id * 10 FROM items i WHERE i.hash = blobs.hash);
             UPDATE items SET rating = id % 3, size_bytes = id * 10;
             UPDATE items SET duration_ms = 100 * id WHERE id IN (2, 4);
             UPDATE items SET captured_at = imported_at WHERE id IN (1, 3);",
        )
        .unwrap();
        let sort = |key, desc| ItemSort { key, desc };
        // 名前：p5 < p10 < p15 …（自然順）。id 6 = p5、id 1 = p30
        assert_eq!(
            search(&conn, ItemView::All, "", sort(SortKey::Name, false)),
            vec![6, 5, 4, 3, 2, 1]
        );
        assert_eq!(
            search(&conn, ItemView::All, "", sort(SortKey::Name, true)),
            vec![1, 2, 3, 4, 5, 6]
        );
        assert_eq!(
            search(&conn, ItemView::All, "", sort(SortKey::Size, true)),
            vec![6, 5, 4, 3, 2, 1]
        );
        // 評価が同じなら追加日の新しい順
        assert_eq!(
            search(&conn, ItemView::All, "", sort(SortKey::Rating, true)),
            vec![5, 2, 4, 1, 6, 3]
        );
        // 長さ・撮影日が不明なものは後ろ
        assert_eq!(
            search(&conn, ItemView::All, "", sort(SortKey::Duration, true))[..2],
            [4, 2]
        );
        assert_eq!(
            search(&conn, ItemView::All, "", sort(SortKey::Imported, false)),
            vec![1, 2, 3, 4, 5, 6]
        );
        let list = query_ids(
            &conn,
            &ItemQuery {
                sort: sort(SortKey::Captured, true),
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap();
        assert_eq!(list.ids[..2], [3, 1]);
        assert_eq!(
            list.groups.last().map(|g| (g.start, g.day)),
            Some((2, NO_DATE_DAY))
        );
        // 名前・評価などの並びには見出しがない
        let named = query_ids(
            &conn,
            &ItemQuery {
                sort: sort(SortKey::Name, false),
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap();
        assert!(named.groups.is_empty());
    }

    #[test]
    fn counts_matches_by_kind() {
        let conn = library();
        conn.execute("UPDATE items SET kind = 'video' WHERE id IN (2, 3)", [])
            .unwrap();
        conn.execute("UPDATE items SET kind = 'audio' WHERE id = 6", [])
            .unwrap();
        let query = |view, text: &str| ItemQuery {
            view,
            filter: Some(crate::query::parse(text).unwrap().expr),
            ..ItemQuery::default()
        };
        let counts = count_kinds(&conn, &query(ItemView::All, ""), &ctx(0)).unwrap();
        assert_eq!(
            (counts.image, counts.video, counts.audio, counts.total()),
            (3, 2, 1, 6)
        );
        let counts = count_kinds(
            &conn,
            &query(ItemView::Folder { id: 1 }, "-種類:画像"),
            &ctx(0),
        )
        .unwrap();
        assert_eq!((counts.image, counts.video, counts.audio), (0, 2, 0));
        let unit = crate::unit::create_set(&conn, "u", &[2, 6, 1], 0).unwrap();
        let counts = count_kinds(
            &conn,
            &query(ItemView::Unit { id: unit }, "-種類:動画"),
            &ctx(0),
        )
        .unwrap();
        assert_eq!((counts.image, counts.video, counts.audio), (1, 0, 1));
    }

    /// フォント・3D を音楽に数えない（`_ =>` の枝で音楽に落ちていた退行を防ぐ）。
    #[test]
    fn kind_counts_include_font_and_model() {
        let conn = library();
        conn.execute("UPDATE items SET kind = 'font' WHERE id IN (2, 3)", [])
            .unwrap();
        conn.execute("UPDATE items SET kind = 'model' WHERE id = 6", [])
            .unwrap();
        let query = ItemQuery::default();
        let counts = count_kinds(&conn, &query, &ctx(0)).unwrap();
        let c = counts;
        assert_eq!(
            (c.image, c.video, c.audio, c.font, c.model),
            (3, 0, 0, 2, 1)
        );
        assert_eq!(counts.total(), 6);
        let unit = crate::unit::create_set(&conn, "u", &[2, 6, 1], 0).unwrap();
        let counts = count_kinds(
            &conn,
            &ItemQuery {
                view: ItemView::Unit { id: unit },
                ..ItemQuery::default()
            },
            &ctx(0),
        )
        .unwrap();
        let c = counts;
        assert_eq!(
            (c.image, c.video, c.audio, c.font, c.model),
            (1, 0, 0, 1, 1)
        );
        // 検索の語で種類を選べる
        let only = |text: &str| {
            count_kinds(
                &conn,
                &ItemQuery {
                    filter: Some(crate::query::parse(text).unwrap().expr),
                    ..ItemQuery::default()
                },
                &ctx(0),
            )
            .unwrap()
        };
        assert_eq!(only("種類:フォント").total(), 2);
        assert_eq!(only("種類:3D").model, 1);
        assert_eq!(only("-種類:フォント,3D").total(), 3);
    }
}
