//! 一覧の取得（docs/data-model.md §4「ID 先取り」）。
//!
//! - `query_ids`：条件に合う ID・縦横比・日付のグループを並び順で返す（全件分。詳細は持たない）。
//! - `summaries`：見えている分の詳細（最大 200 件）。
//! - `detail`：インスペクタ用（タグ・フォルダ・ユニットを含む）。

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, Row, params_from_iter};

use crate::library::hex;
use crate::{CoreError, Result};

/// 一度に詳細を取れる件数。
pub const MAX_SUMMARIES: usize = 200;

const DAY_MS: i64 = 86_400_000;

/// 並び順。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ItemSort {
    /// 追加日の新しい順（既定）。
    #[default]
    ImportedDesc,
}

/// 一覧の条件（検索言語は M4）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemQuery {
    /// ゴミ箱の中を出すか。
    pub trashed: bool,
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

/// 条件に合う ID・縦横比・日付のグループを並び順で返す。`utc_offset_minutes` は日付の区切りに使う時差。
pub fn query_ids(
    conn: &Connection,
    query: &ItemQuery,
    sort: ItemSort,
    utc_offset_minutes: i32,
) -> Result<IdList> {
    // 一覧は全件を並び順で読む（意図した全件の走査。idx_items_imported の順に読む）
    let sql = match (query.trashed, sort) {
        (false, ItemSort::ImportedDesc) => {
            "SELECT id, width, height, imported_at FROM items
             WHERE trashed_at IS NULL ORDER BY imported_at DESC, id DESC"
        }
        (true, ItemSort::ImportedDesc) => {
            "SELECT id, width, height, trashed_at FROM items
             WHERE trashed_at IS NOT NULL ORDER BY trashed_at DESC, id DESC"
        }
    };
    let offset_ms = i64::from(utc_offset_minutes) * 60_000;
    let mut stmt = conn.prepare_cached(sql)?;
    let mut rows = stmt.query([])?;
    let mut list = IdList::default();
    let mut current_day: Option<i32> = None;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let width: Option<i64> = row.get(1)?;
        let height: Option<i64> = row.get(2)?;
        let at: i64 = row.get(3)?;
        let day = i32::try_from((at + offset_ms).div_euclid(DAY_MS)).unwrap_or(0);
        if current_day != Some(day) {
            list.groups.push(DayGroup {
                start: list.ids.len() as u32,
                day,
            });
            current_day = Some(day);
        }
        list.ids.push(
            u32::try_from(id)
                .map_err(|_| CoreError::Invalid(format!("ID が大きすぎます: {id}")))?,
        );
        list.aspects.push(aspect_code(width, height));
    }
    Ok(list)
}

/// 一覧の 1 件（グリッドのタイルに出す分）。
#[derive(Debug, Clone, PartialEq)]
pub struct ItemSummary {
    pub id: i64,
    pub hash: String,
    pub ext: String,
    pub kind: String,
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
}

const SUMMARY_COLUMNS: &str = "id, hash, ext, kind, width, height, duration_ms, size_bytes, title, \
                               rating, source_site, imported_at, captured_at, thumb_state";

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<ItemSummary> {
    let hash: Vec<u8> = row.get(1)?;
    Ok(ItemSummary {
        id: row.get(0)?,
        hash: hex(&hash),
        ext: row.get(2)?,
        kind: row.get(3)?,
        width: row.get(4)?,
        height: row.get(5)?,
        duration_ms: row.get(6)?,
        size_bytes: row.get(7)?,
        title: row.get(8)?,
        rating: row.get(9)?,
        source_site: row.get(10)?,
        imported_at: row.get(11)?,
        captured_at: row.get(12)?,
        thumb_state: row.get(13)?,
    })
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
    let marks = vec!["?"; ids.len()].join(", ");
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SUMMARY_COLUMNS} FROM items WHERE id IN ({marks})"
    ))?;
    let rows = stmt.query_map(params_from_iter(ids.iter()), summary_from_row)?;
    let mut by_id: HashMap<i64, ItemSummary> = HashMap::with_capacity(ids.len());
    for row in rows {
        let item = row?;
        by_id.insert(item.id, item);
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// タグ（名前空間付き）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRef {
    pub id: i64,
    pub namespace: String,
    pub name: String,
}

/// 所属する仮想フォルダ（いちばん上からのパス）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRef {
    pub id: i64,
    pub path: Vec<String>,
}

/// 所属するユニット。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitRef {
    pub id: i64,
    pub name: String,
    pub kind: String,
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
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {SUMMARY_COLUMNS}, mime, codec_info, original_name, original_path, source_url, note, trashed_at
         FROM items WHERE id = ?1"
    ))?;
    let found = stmt
        .query_row([id], |row| {
            Ok((
                summary_from_row(row)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, String>(16)?,
                row.get::<_, Option<String>>(17)?,
                row.get::<_, Option<String>>(18)?,
                row.get::<_, Option<String>>(19)?,
                row.get::<_, Option<i64>>(20)?,
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
        "SELECT t.id, t.namespace, t.name FROM item_tags m JOIN tags t ON t.id = m.tag_id
         WHERE m.item_id = ?1 ORDER BY t.namespace = '', t.namespace, t.name",
    )?;
    let tags = tag_stmt
        .query_map([id], |row| {
            Ok(TagRef {
                id: row.get(0)?,
                namespace: row.get(1)?,
                name: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

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

    let mut unit_stmt = conn.prepare_cached(
        "SELECT u.id, u.name, u.kind FROM unit_items m JOIN units u ON u.id = m.unit_id
         WHERE m.item_id = ?1 ORDER BY u.name",
    )?;
    let units = unit_stmt
        .query_map([id], |row| {
            Ok(UnitRef {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};

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

        let list = query_ids(&conn, &ItemQuery::default(), ItemSort::ImportedDesc, 0).unwrap();
        assert_eq!(list.ids, vec![3, 2, 1]);
        assert_eq!(list.aspects, vec![1000, 1000, 1500]);
        assert_eq!(
            list.groups,
            vec![DayGroup { start: 0, day: 2 }, DayGroup { start: 1, day: 1 }]
        );

        // 時差 -1 時間なら、どれも 1 日ずつ前の日になる（3 は 1 日目、1・2 は 0 日目）
        let shifted = query_ids(&conn, &ItemQuery::default(), ItemSort::ImportedDesc, -60).unwrap();
        assert_eq!(
            shifted.groups,
            vec![DayGroup { start: 0, day: 1 }, DayGroup { start: 1, day: 0 }]
        );

        let trash = query_ids(
            &conn,
            &ItemQuery { trashed: true },
            ItemSort::ImportedDesc,
            0,
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
            "SELECT id, width, height, imported_at FROM items
             WHERE trashed_at IS NULL ORDER BY imported_at DESC, id DESC",
        );
        assert!(plan.contains("idx_items_imported"), "{plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}
