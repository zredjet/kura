//! 再生キュー（docs/playlists.md §5.1 の Slot 列）。
//!
//! キューは `playlist::resolve` と `playlist::build_queue`（選択から作るその場のキューも、エントリの列にして同じ道を通る。
//! フォント・3D は `resolve` が除く）で作る。ここはスロットの型と、キューの長さだけを持つ。

use rusqlite::{Connection, OptionalExtension};

use crate::Result;

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

    fn slot(item_id: i64, image_duration_ms: Option<i64>) -> Slot {
        Slot {
            item_id,
            entry_id: None,
            unit_key: None,
            index_in_unit: 0,
            unit_len: 1,
            image_duration_ms,
            start_ms: None,
            end_ms: None,
        }
    }

    #[test]
    fn total_is_image_time_plus_known_durations() {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, duration_ms)
               VALUES (1, x'01', 'jpg', 'image', 1, 'a', 'a', 0, 0, NULL),
                      (2, x'02', 'mkv', 'video', 1, 'b', 'b', 0, 0, 60000),
                      (3, x'03', 'flac', 'audio', 1, 'c', 'c', 0, 0, NULL);",
        )
        .unwrap();
        // 画像は表示時間、動画は長さ、長さの不明な音楽と見つからないアイテムは 0
        let slots = [
            slot(1, Some(8_000)),
            slot(2, None),
            slot(3, None),
            slot(99, None),
            slot(1, Some(15_000)),
        ];
        assert_eq!(total_ms(&conn, &slots).unwrap(), 8_000 + 60_000 + 15_000);
        assert!(slots[0].unit_key.is_none() && !slots[0].is_unit_start());
        let start = Slot {
            unit_key: Some(UnitKey::Unit(1)),
            ..slot(1, None)
        };
        assert!(start.is_unit_start());
        assert!(
            !Slot {
                index_in_unit: 1,
                ..start
            }
            .is_unit_start()
        );
    }
}
