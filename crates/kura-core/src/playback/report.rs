//! 再生済みの判定と記録（docs/playlists.md §6.4）。
//!
//! フロントはスロットを離れるとき（終わった・次へ・前へ・閉じた）に、そのスロットで実際に再生・表示した
//! 時間を送る。ここで「再生済み」かを判定し、そうなら `play_events` に記録して回数と日時を更新する。

use rusqlite::{Connection, OptionalExtension, params};

use crate::import::MediaKind;
use crate::{CoreError, Result};

/// 音楽・動画を「再生済み」とする再生時間（4 分）。
pub const PLAYED_MEDIA_MS: i64 = 4 * 60 * 1000;
/// 画像を「再生済み」とする表示時間（次へ進んだときに 2 秒以上）。
pub const PLAYED_IMAGE_MS: i64 = 2000;

/// 1 スロット分の報告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayReport {
    pub item_id: i64,
    /// ユニットとして再生したスロットならそのユニット。
    pub unit_id: Option<i64>,
    /// ユニットの最初のスロットか（ユニットの再生回数はそのときだけ増やす）。
    pub unit_start: bool,
    pub playlist_id: Option<i64>,
    /// 実際に再生・表示した時間（シークで飛ばした分は含まない）。
    pub played_ms: i64,
    /// 最後まで再生・表示したか。
    pub completed: bool,
    /// 再生要素が報告した長さ（DB に長さがないときに使う）。
    pub duration_ms: Option<i64>,
}

/// 再生済みか（純粋関数）。音楽・動画は再生時間の 50% 以上か 4 分以上、
/// 画像は表示時間を最後まで表示したか、2 秒以上表示してから次へ進んだか。
/// フォント・3D はキューに入らないので、報告が来ても再生済みにしない。
pub fn judge(kind: MediaKind, played_ms: i64, duration_ms: Option<i64>, completed: bool) -> bool {
    match kind {
        MediaKind::Image => completed || played_ms >= PLAYED_IMAGE_MS,
        MediaKind::Video | MediaKind::Audio => {
            played_ms >= PLAYED_MEDIA_MS || duration_ms.is_some_and(|d| d > 0 && played_ms * 2 >= d)
        }
        MediaKind::Font | MediaKind::Model => false,
    }
}

/// 判定して、再生済みなら記録する。記録したら true。
pub fn record(tx: &Connection, report: &PlayReport, now: i64) -> Result<bool> {
    let row: Option<(MediaKind, Option<i64>)> = tx
        .query_row(
            "SELECT kind, duration_ms FROM items WHERE id = ?1",
            [report.item_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((kind, duration_ms)) = row else {
        return Err(CoreError::NotFound(format!("アイテム {}", report.item_id)));
    };
    let duration_ms = duration_ms.or(report.duration_ms);
    if !judge(kind, report.played_ms.max(0), duration_ms, report.completed) {
        return Ok(false);
    }
    // ユニット・プレイリストが消えていたら、つながりは付けずに記録する
    let exists = |table: &str, id: Option<i64>| -> Result<Option<i64>> {
        let Some(id) = id else { return Ok(None) };
        let sql = format!("SELECT EXISTS (SELECT 1 FROM {table} WHERE id = ?1)");
        let found: bool = tx.prepare_cached(&sql)?.query_row([id], |r| r.get(0))?;
        Ok(found.then_some(id))
    };
    let unit_id = exists("units", report.unit_id)?;
    let playlist_id = exists("playlists", report.playlist_id)?;
    tx.prepare_cached(
        "INSERT INTO play_events (item_id, unit_id, playlist_id, played_at, played_ms, completed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![
        report.item_id,
        unit_id,
        playlist_id,
        now,
        report.played_ms.max(0),
        report.completed
    ])?;
    tx.prepare_cached(
        "UPDATE items SET play_count = play_count + 1, last_played_at = ?2 WHERE id = ?1",
    )?
    .execute(params![report.item_id, now])?;
    if let Some(unit_id) = unit_id {
        tx.prepare_cached(
            "UPDATE units SET play_count = play_count + ?2, last_played_at = ?3 WHERE id = ?1",
        )?
        .execute(params![unit_id, i64::from(report.unit_start), now])?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;

    #[test]
    fn judges_media_by_half_or_four_minutes() {
        // 3 分の曲：1 分 30 秒で再生済み
        assert!(!judge(MediaKind::Audio, 89_999, Some(180_000), false));
        assert!(judge(MediaKind::Audio, 90_000, Some(180_000), false));
        // 1 時間の動画：4 分で再生済み
        assert!(!judge(MediaKind::Video, 239_999, Some(3_600_000), false));
        assert!(judge(MediaKind::Video, 240_000, Some(3_600_000), false));
        // 最後までシークして終わっただけでは再生済みにしない
        assert!(!judge(MediaKind::Video, 5_000, Some(3_600_000), true));
        // 長さがわからなければ 4 分
        assert!(!judge(MediaKind::Audio, 200_000, None, true));
        assert!(judge(MediaKind::Audio, 240_000, None, false));
    }

    #[test]
    fn judges_images_by_completion_or_two_seconds() {
        assert!(judge(MediaKind::Image, 8_000, None, true));
        // 表示時間を 1 秒にしていても、最後まで表示すれば再生済み
        assert!(judge(MediaKind::Image, 1_000, None, true));
        assert!(judge(MediaKind::Image, 2_000, None, false));
        assert!(!judge(MediaKind::Image, 1_999, None, false));
    }

    /// フォント・3D はキューに入らないので、報告が来ても再生済みにしない。
    #[test]
    fn fonts_and_models_are_never_played() {
        for kind in [MediaKind::Font, MediaKind::Model] {
            assert!(!judge(kind, 240_000, Some(1_000), true), "{kind:?}");
        }
    }

    fn library() -> Connection {
        let conn = test_conn();
        conn.execute_batch(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, duration_ms)
               VALUES (1, x'01', 'flac', 'audio', 1, 'a', 'a', 0, 0, 180000),
                      (2, x'02', 'jpg', 'image', 1, 'b', 'b', 0, 0, NULL),
                      (3, x'03', 'jpg', 'image', 1, 'c', 'c', 0, 0, NULL);
             INSERT INTO units (id, kind, name, order_mode, created_at, updated_at) VALUES (1, 'set', '鎌倉', 'manual', 0, 0);
             INSERT INTO unit_items (unit_id, item_id, pos) VALUES (1, 2, 1), (1, 3, 2);",
        )
        .unwrap();
        conn
    }

    fn counts(conn: &Connection, table: &str, id: i64) -> (i64, Option<i64>) {
        conn.query_row(
            &format!("SELECT play_count, last_played_at FROM {table} WHERE id = ?1"),
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    fn report(item_id: i64, played_ms: i64) -> PlayReport {
        PlayReport {
            item_id,
            unit_id: None,
            unit_start: false,
            playlist_id: None,
            played_ms,
            completed: false,
            duration_ms: None,
        }
    }

    #[test]
    fn records_played_items_only() {
        let conn = library();
        assert!(!record(&conn, &report(1, 30_000), 100).unwrap());
        assert_eq!(counts(&conn, "items", 1), (0, None));
        assert!(record(&conn, &report(1, 95_000), 200).unwrap());
        assert_eq!(counts(&conn, "items", 1), (1, Some(200)));
        let event: (i64, Option<i64>, i64, i64, bool) = conn
            .query_row(
                "SELECT item_id, unit_id, played_at, played_ms, completed FROM play_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(event, (1, None, 200, 95_000, false));
        assert!(matches!(
            record(&conn, &report(99, 95_000), 300),
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn unit_count_grows_only_from_its_first_slot() {
        let conn = library();
        let slot = |item_id, unit_start| PlayReport {
            unit_id: Some(1),
            unit_start,
            completed: true,
            ..report(item_id, 8_000)
        };
        assert!(record(&conn, &slot(2, true), 100).unwrap());
        assert!(record(&conn, &slot(3, false), 200).unwrap());
        assert_eq!(counts(&conn, "units", 1), (1, Some(200)));
        // 途中のスロットから再生しても回数は増えず、日時だけ更新
        assert!(record(&conn, &slot(3, false), 300).unwrap());
        assert_eq!(counts(&conn, "units", 1), (1, Some(300)));
        assert!(record(&conn, &slot(2, true), 400).unwrap());
        assert_eq!(counts(&conn, "units", 1), (2, Some(400)));
        assert_eq!(counts(&conn, "items", 3), (2, Some(300)));
        // 消えたユニットはつながりを付けずに記録する
        conn.execute("DELETE FROM units WHERE id = 1", []).unwrap();
        assert!(record(&conn, &slot(2, true), 500).unwrap());
        let unit: Option<i64> = conn
            .query_row(
                "SELECT unit_id FROM play_events ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unit, None);
    }
}
