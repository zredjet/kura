//! サムネイルを作っている途中の印（critic 8・決定の「OS のデコーダで落ちたとき」）。
//!
//! OS のデコーダ（ImageIO・CoreText・AVFoundation）とエンジンの Platform の段はアプリのプロセスの中で動くので（ADR-153）、
//! 壊れたファイルでプロセスごと落ちることがある。何もしないと、次の起動で同じアイテムをまた読んで落ち続ける。
//!
//! - 作る前に、作っているアイテムを settings の [`INFLIGHT_KEY`] に書く（1 回の取り出し 64 件をまとめて 1 回。並べて作るので、
//!   どれで落ちたかは分からない）。結果を書くトランザクションで消す。
//! - 次に開いたときに印が残っていれば（[`recover`]）、まとめての印のアイテムは「疑わしいもの」（[`SUSPECTS_KEY`]）にする。
//!   サムネイルのジョブは、疑わしいものを先に 1 件ずつ、1 件だけの印を付けて作る（ほかのアイテムを巻き込まない）。
//! - 1 件だけの印が残っていれば、それが落ちたアイテム：pending は failed に、stale は ready に戻して（作り直す前の絵を出したまま）
//!   先へ進む。同じファイルで落ちるのは多くても 2 回。

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::settings;
use crate::time::now_ms;

/// settings のキー：作っている途中のアイテム。
pub const INFLIGHT_KEY: &str = "thumbs.inflight";
/// settings のキー：前回落ちたときにまとめて作っていたアイテム（1 件ずつ作り直す）。
pub const SUSPECTS_KEY: &str = "thumbs.suspects";

/// 作っている途中の印。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inflight {
    /// 1 件だけを作っている（落ちたらこのアイテムが原因）。
    pub solo: bool,
    pub ids: Vec<i64>,
}

/// 作る前に印を付ける。
pub fn mark(tx: &Connection, ids: &[i64], solo: bool) -> Result<()> {
    settings::set(
        tx,
        INFLIGHT_KEY,
        &Inflight {
            solo,
            ids: ids.to_vec(),
        },
    )
}

/// 結果を書いたら印を消す（結果と同じトランザクションで）。
pub fn clear(tx: &Connection) -> Result<()> {
    let mut stmt = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
    stmt.execute([INFLIGHT_KEY])?;
    Ok(())
}

/// 疑わしいもの（1 件ずつ作るもの）。
pub fn suspects(conn: &Connection) -> Result<Vec<i64>> {
    Ok(settings::get(conn, SUSPECTS_KEY)?.unwrap_or_default())
}

/// 疑わしいものから `id` を外す（作り終えたか、もう作らないもの）。
pub fn drop_suspect(tx: &Connection, id: i64) -> Result<()> {
    let rest: Vec<i64> = suspects(tx)?.into_iter().filter(|s| *s != id).collect();
    if rest.is_empty() {
        let mut stmt = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
        stmt.execute([SUSPECTS_KEY])?;
        Ok(())
    } else {
        settings::set(tx, SUSPECTS_KEY, &rest)
    }
}

/// [`recover`] の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovered {
    /// 1 件だけで作っていて落ちたアイテム（pending は failed、stale は ready にした）。
    pub crashed: Vec<i64>,
    /// 1 件ずつ作り直す数（疑わしいものの数）。
    pub suspects: usize,
}

/// 開くとき（ジョブを動かす前に）：前回落ちたときの印を片付ける。
pub fn recover(tx: &Connection) -> Result<Recovered> {
    let Some(inflight) = settings::get::<Inflight>(tx, INFLIGHT_KEY)? else {
        return Ok(Recovered {
            crashed: Vec::new(),
            suspects: suspects(tx)?.len(),
        });
    };
    let mut out = Recovered::default();
    if inflight.solo {
        let now = now_ms();
        let mut stmt = tx.prepare_cached(
            "UPDATE items SET
               thumb_state = CASE thumb_state WHEN 'stale' THEN 'ready' ELSE 'failed' END,
               updated_at = ?2
             WHERE id = ?1 AND thumb_state IN ('pending', 'stale')",
        )?;
        for &id in &inflight.ids {
            if stmt.execute(rusqlite::params![id, now])? > 0 {
                out.crashed.push(id);
            }
            drop_suspect(tx, id)?;
        }
        if !out.crashed.is_empty() {
            tracing::warn!(
                ids = ?out.crashed,
                "前回、サムネイルを作っている途中でアプリが終わったアイテムを、作れなかったものにします"
            );
        }
    } else {
        let mut all = suspects(tx)?;
        for id in inflight.ids {
            if !all.contains(&id) {
                all.push(id);
            }
        }
        if !all.is_empty() {
            tracing::warn!(
                count = all.len(),
                "前回、サムネイルを作っている途中でアプリが終わりました。そのとき作っていたものを 1 件ずつ作り直します"
            );
            settings::set(tx, SUSPECTS_KEY, &all)?;
        }
    }
    clear(tx)?;
    out.suspects = suspects(tx)?.len();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_conn;
    use rusqlite::params;

    fn add(conn: &Connection, id: i64, state: &str) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at, thumb_state)
             VALUES (?1, ?2, 'jpg', 'image', 1, 't', 't', 1, 1, ?3)",
            params![id, vec![id as u8; 32], state],
        )
        .unwrap();
    }

    fn state(conn: &Connection, id: i64) -> String {
        conn.query_row("SELECT thumb_state FROM items WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    /// 印がなければ何もしない。まとめての印は疑わしいもの（前からのものと合わせる）にして、アイテムは変えない。
    #[test]
    fn a_batch_mark_left_behind_becomes_suspects() {
        let conn = test_conn();
        for id in 1..=3 {
            add(&conn, id, "pending");
        }
        assert_eq!(recover(&conn).unwrap(), Recovered::default());
        settings::set(&conn, SUSPECTS_KEY, &vec![3i64]).unwrap();
        mark(&conn, &[1, 2, 3], false).unwrap();
        assert_eq!(
            recover(&conn).unwrap(),
            Recovered {
                crashed: vec![],
                suspects: 3
            }
        );
        assert_eq!(suspects(&conn).unwrap(), vec![3, 1, 2]);
        assert!(
            settings::get::<Inflight>(&conn, INFLIGHT_KEY)
                .unwrap()
                .is_none()
        );
        assert_eq!(state(&conn, 1), "pending");
        drop_suspect(&conn, 1).unwrap();
        assert_eq!(suspects(&conn).unwrap(), vec![3, 2]);
        drop_suspect(&conn, 3).unwrap();
        drop_suspect(&conn, 2).unwrap();
        assert!(suspects(&conn).unwrap().is_empty());
        assert!(
            settings::get::<Vec<i64>>(&conn, SUSPECTS_KEY)
                .unwrap()
                .is_none()
        );
    }

    /// 1 件だけの印が残っていれば、そのアイテムで落ちた：pending は failed に、stale は作り直す前の絵（ready）に戻す。
    /// 作り終えていたもの（ready）は変えない。疑わしいものからも外す。
    #[test]
    fn a_solo_mark_left_behind_fails_that_item() {
        let conn = test_conn();
        add(&conn, 1, "pending");
        add(&conn, 2, "stale");
        add(&conn, 3, "ready");
        settings::set(&conn, SUSPECTS_KEY, &vec![1i64, 4]).unwrap();
        mark(&conn, &[1], true).unwrap();
        assert_eq!(
            recover(&conn).unwrap(),
            Recovered {
                crashed: vec![1],
                suspects: 1
            }
        );
        assert_eq!(state(&conn, 1), "failed");
        assert_eq!(suspects(&conn).unwrap(), vec![4]);
        mark(&conn, &[2], true).unwrap();
        recover(&conn).unwrap();
        assert_eq!(state(&conn, 2), "ready");
        mark(&conn, &[3], true).unwrap();
        assert!(recover(&conn).unwrap().crashed.is_empty());
        assert_eq!(state(&conn, 3), "ready");
        // 片付けたら、次はもう何もしない
        assert!(recover(&conn).unwrap().crashed.is_empty());
    }
}
