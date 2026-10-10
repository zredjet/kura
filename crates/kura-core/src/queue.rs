//! 再生キュー（docs/playlists.md §5.1 の Slot 列）。
//!
//! M5 はライブラリの選択をそのままキューにする（`from_selection`）。プレイリストからキューを作る
//! `resolve` / `build_queue`（シャッフルを含む）は M6 でここに足す。

use rusqlite::{Connection, OptionalExtension};

use crate::unit::{self, DEFAULT_IMAGE_DURATION_MS, LIST_FLAG};
use crate::{CoreError, Result};

/// スロットがどのまとまりから来たか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnitKey {
    Unit(i64),
    /// フォルダのエントリ（M6）。
    Folder {
        folder_id: i64,
        recursive: bool,
    },
}

/// キューの 1 つ分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub item_id: i64,
    /// プレイリストのエントリ（M6。選択から作ったキューでは None）。
    pub entry_id: Option<i64>,
    pub unit_key: Option<UnitKey>,
    /// まとまりの中の位置（0 から）と、まとまりの件数。単体のアイテムは 0 と 1。
    pub index_in_unit: u32,
    pub unit_len: u32,
    /// 画像の表示時間（音楽・動画は None）。
    pub image_duration_ms: Option<i64>,
    /// トリム（M6）。
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
}

impl Slot {
    /// まとまりの最初のスロットか。
    pub fn is_unit_start(&self) -> bool {
        self.unit_key.is_some() && self.index_in_unit == 0
    }
}

/// 選択（アイテムの ID と、まとめて表示したユニットの `LIST_FLAG | unit_id`）を、並び順のまま
/// キューにする。ユニットはメンバー順に展開する。ゴミ箱のもの・見つからないものは除く。
/// 画像の表示時間はユニットの設定 → `default_image_ms`（docs/playlists.md §5.3）。
pub fn from_selection(
    conn: &Connection,
    entries: &[i64],
    default_image_ms: Option<i64>,
) -> Result<Vec<Slot>> {
    let default_image_ms = default_image_ms.unwrap_or(DEFAULT_IMAGE_DURATION_MS);
    let flag = i64::from(LIST_FLAG);
    let mut kind_of =
        conn.prepare_cached("SELECT kind FROM items WHERE id = ?1 AND trashed_at IS NULL")?;
    let mut unit_image_ms =
        conn.prepare_cached("SELECT image_duration_ms FROM units WHERE id = ?1")?;
    let mut slots = Vec::with_capacity(entries.len());
    for &entry in entries {
        if entry & flag == 0 {
            let kind: Option<String> = kind_of.query_row([entry], |r| r.get(0)).optional()?;
            if let Some(kind) = kind {
                slots.push(Slot {
                    item_id: entry,
                    entry_id: None,
                    unit_key: None,
                    index_in_unit: 0,
                    unit_len: 1,
                    image_duration_ms: (kind == "image").then_some(default_image_ms),
                    start_ms: None,
                    end_ms: None,
                });
            }
            continue;
        }
        let unit_id = entry & !flag;
        let members = match unit::members(conn, unit_id) {
            Ok(members) => members,
            Err(CoreError::NotFound(_)) => continue,
            Err(err) => return Err(err),
        };
        let image_ms: Option<i64> = unit_image_ms
            .query_row([unit_id], |r| r.get(0))
            .optional()?
            .flatten();
        let mut kinds = Vec::with_capacity(members.len());
        for &item_id in &members {
            let kind: Option<String> = kind_of.query_row([item_id], |r| r.get(0)).optional()?;
            if let Some(kind) = kind {
                kinds.push((item_id, kind));
            }
        }
        let len = u32::try_from(kinds.len()).unwrap_or(u32::MAX);
        for (index, (item_id, kind)) in kinds.into_iter().enumerate() {
            slots.push(Slot {
                item_id,
                entry_id: None,
                unit_key: Some(UnitKey::Unit(unit_id)),
                index_in_unit: u32::try_from(index).unwrap_or(u32::MAX),
                unit_len: len,
                image_duration_ms: (kind == "image")
                    .then_some(image_ms.unwrap_or(default_image_ms)),
                start_ms: None,
                end_ms: None,
            });
        }
    }
    Ok(slots)
}

/// キュー全体の長さ（画像は表示時間、音楽・動画は長さ。長さのわからないものは 0）。
pub fn total_ms(conn: &Connection, slots: &[Slot]) -> Result<i64> {
    let mut duration =
        conn.prepare_cached("SELECT coalesce(duration_ms, 0) FROM items WHERE id = ?1")?;
    let mut total = 0i64;
    for slot in slots {
        total += match slot.image_duration_ms {
            Some(ms) => ms,
            None => duration
                .query_row([slot.item_id], |r| r.get::<_, i64>(0))
                .optional()?
                .unwrap_or(0),
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    fn library() -> Connection {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
               VALUES (1, x'01', 'jpg', 'image', 1, 'a', 'a', 0, 0),
                      (2, x'02', 'mkv', 'video', 1, 'b', 'b', 0, 0),
                      (3, x'03', 'flac', 'audio', 1, 'c', 'c', 0, 0),
                      (4, x'04', 'jpg', 'image', 1, 'd', 'd', 0, 0),
                      (5, x'05', 'jpg', 'image', 1, 'e', 'e', 0, 0),
                      (6, x'06', 'jpg', 'image', 1, 'f', 'f', 0, 0);
             UPDATE items SET trashed_at = 1 WHERE id = 6;
             INSERT INTO units (id, kind, name, order_mode, image_duration_ms, created_at, updated_at)
               VALUES (1, 'set', '鎌倉', 'manual', 15000, 0, 0);
             INSERT INTO unit_items (unit_id, item_id, pos) VALUES (1, 5, 1), (1, 6, 2), (1, 4, 3), (1, 3, 4);",
        )
        .unwrap();
        conn
    }

    /// （アイテム, ユニット, 位置, 件数, 画像の表示時間）
    type Row = (i64, Option<i64>, u32, u32, Option<i64>);

    fn summary(slots: &[Slot]) -> Vec<Row> {
        slots
            .iter()
            .map(|s| {
                (
                    s.item_id,
                    s.unit_key.map(|key| match key {
                        UnitKey::Unit(id) | UnitKey::Folder { folder_id: id, .. } => id,
                    }),
                    s.index_in_unit,
                    s.unit_len,
                    s.image_duration_ms,
                )
            })
            .collect()
    }

    #[test]
    fn keeps_selection_order_and_expands_units() {
        let conn = library();
        let unit = i64::from(LIST_FLAG) | 1;
        let slots = from_selection(&conn, &[2, unit, 1, 6, 99], None).unwrap();
        assert_eq!(
            summary(&slots),
            vec![
                (2, None, 0, 1, None),
                // ユニットはメンバー順。ゴミ箱のもの（6）は除き、表示時間はユニットの設定
                (5, Some(1), 0, 3, Some(15_000)),
                (4, Some(1), 1, 3, Some(15_000)),
                (3, Some(1), 2, 3, None),
                (1, None, 0, 1, Some(DEFAULT_IMAGE_DURATION_MS)),
            ]
        );
        assert!(slots[1].is_unit_start());
        assert!(!slots[2].is_unit_start());
        assert!(!slots[0].is_unit_start());
        // 3 の長さは不明（0）、画像は 15 秒 × 2 と 8 秒
        assert_eq!(
            total_ms(&conn, &slots).unwrap(),
            2 * 15_000 + DEFAULT_IMAGE_DURATION_MS
        );
        // 既定の表示時間はアプリの設定で変えられる
        let slots = from_selection(&conn, &[1], Some(5_000)).unwrap();
        assert_eq!(slots[0].image_duration_ms, Some(5_000));
        // 消えたユニットは飛ばす
        let gone = i64::from(LIST_FLAG) | 7;
        assert!(from_selection(&conn, &[gone], None).unwrap().is_empty());
    }

    /// 10 万件の選択からキューを作る時間（`cargo test --release -p kura-core queue -- --ignored --nocapture`）。
    #[test]
    #[ignore]
    fn bench_selection_of_100k_items() {
        let mut conn = test_conn();
        let tx = conn.transaction().unwrap();
        {
            let mut insert = tx
                .prepare(
                    "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, duration_ms)
                     VALUES (?1, ?2, 'jpg', ?3, 1, 't', 't', 0, 0, ?4)",
                )
                .unwrap();
            for id in 1..=100_000i64 {
                let kind = ["image", "video", "audio"][(id % 3) as usize];
                let duration = (kind != "image").then_some(60_000i64);
                insert
                    .execute(rusqlite::params![
                        id,
                        id.to_le_bytes().to_vec(),
                        kind,
                        duration
                    ])
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        let ids: Vec<i64> = (1..=100_000).collect();
        let started = std::time::Instant::now();
        let slots = from_selection(&conn, &ids, None).unwrap();
        let built = started.elapsed();
        let total = total_ms(&conn, &slots).unwrap();
        eprintln!(
            "10 万件：キュー {:?}、合計の長さ {:?}（{} ms）",
            built,
            started.elapsed() - built,
            total
        );
        assert_eq!(slots.len(), 100_000);
    }
}
