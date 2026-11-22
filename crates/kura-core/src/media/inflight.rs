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
//!
//! 取り込み・ダウンロード・キャプチャでメディア情報を読む probe（ImageIO・AVFoundation・CoreText）にも、同じ 2 段の印を付ける
//! （[`ProbeBatch`]。M11 の検証 S2）。ファイルはまだアイテムでないので、取り込み元のパス（取り込みの再開・ダウンロードの
//! やり直しで同じになる）で覚え、ジョブごとに印を持つ（取り込みとダウンロードが同時に読むことがある）。コピー・ハッシュ・
//! 配置と probe を 1 つの並列の段で続けて行えるように、印はバッチの前に 1 回だけ書く。次に開いたときに（[`recover`]）、
//! まとめての印のファイルは [`PROBE_SUSPECTS_KEY`]（1 件ずつ読み直す）に、1 件だけの印のファイルは [`PROBE_CRASHED_KEY`]
//! （もう読まない。入れるアイテムはサムネイルを作れなかったものにする）にする。読んだ結果を DB に書くトランザクションで、
//! 印と覚えを片付ける（[`settle_probes`]）。

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::{MediaBackend, MediaInfo};
use crate::Result;
use crate::db::Db;
use crate::import::MediaKind;
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

/// 開くとき（ジョブを動かす前に）：前回落ちたときの印を片付ける（サムネイルと、取り込みなどの probe の印
/// [`recover_probes`]）。
pub fn recover(tx: &Connection) -> Result<Recovered> {
    recover_probes(tx)?;
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

// ---- 取り込み・ダウンロード・キャプチャの probe の印（M11 の検証 S2） ----

/// settings のキー：メディア情報を読んでいる途中のファイル（ジョブごとの印 [`ProbeMark`] の並び）。
pub const PROBE_INFLIGHT_KEY: &str = "probe.inflight";
/// settings のキー：前回落ちたときにまとめて読んでいたファイル（取り込み元のパス。1 件ずつ読み直す）。
pub const PROBE_SUSPECTS_KEY: &str = "probe.suspects";
/// settings のキー：1 件だけで読んでいて落ちたファイル（取り込み元のパス。もう読まない）。
pub const PROBE_CRASHED_KEY: &str = "probe.crashed";
/// 疑わしいもの・落ちたものとして覚えておく数の上限（入れずに終わったジョブの分が残り続けないように、古いものから捨てる）。
const MAX_PROBE_MEMO: usize = 4_096;

/// 1 つのジョブが読んでいる途中のファイル。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeMark {
    pub job: i64,
    /// 1 件だけを読んでいる（落ちたらこのファイルが原因）。
    pub solo: bool,
    /// 取り込み元のパス（[`probe_key`]）。
    pub paths: Vec<String>,
}

/// probe の印で覚えるファイルの名前（取り込み元のパス。文字にできない部分は置き換える。同じパスなら同じ名前）。
pub fn probe_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// probe の読み方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeWay {
    /// まとめて（並べて）読む。
    Together,
    /// 前回まとめて読んでいて落ちたので、あとで 1 件だけの印を付けて読む。
    Alone,
    /// 前回 1 件だけで読んでいて落ちたので、読まない。
    Skip,
}

fn str_list(conn: &Connection, key: &str) -> Result<Vec<String>> {
    Ok(settings::get(conn, key)?.unwrap_or_default())
}

/// 覚えの並びを書く（空なら消す。多すぎれば古いものから捨てる）。
fn set_str_list(tx: &Connection, key: &str, mut list: Vec<String>) -> Result<()> {
    if list.is_empty() {
        let mut stmt = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
        stmt.execute([key])?;
        return Ok(());
    }
    if list.len() > MAX_PROBE_MEMO {
        list.drain(..list.len() - MAX_PROBE_MEMO);
    }
    settings::set(tx, key, &list)
}

fn marks(conn: &Connection) -> Result<Vec<ProbeMark>> {
    Ok(settings::get(conn, PROBE_INFLIGHT_KEY)?.unwrap_or_default())
}

fn set_marks(tx: &Connection, marks: &[ProbeMark]) -> Result<()> {
    if marks.is_empty() {
        let mut stmt = tx.prepare_cached("DELETE FROM settings WHERE key = ?1")?;
        stmt.execute([PROBE_INFLIGHT_KEY])?;
        Ok(())
    } else {
        settings::set(tx, PROBE_INFLIGHT_KEY, &marks)
    }
}

/// 読む前に、ジョブ `job` の印を付ける（前の印と置き換える。ほかのジョブの印は残す）。
pub fn mark_probe(tx: &Connection, job: i64, paths: &[String], solo: bool) -> Result<()> {
    let mut all = marks(tx)?;
    all.retain(|m| m.job != job);
    all.push(ProbeMark {
        job,
        solo,
        paths: paths.to_vec(),
    });
    set_marks(tx, &all)
}

/// 1 回分（バッチ）の probe：読み方を決め、まとめて読むものの印を付けてある。
pub struct ProbeBatch {
    job: i64,
    suspects: BTreeSet<String>,
    crashed: BTreeSet<String>,
}

impl ProbeBatch {
    /// バッチの前に：ジョブ `job` が読むかもしれないファイル `paths`（[`probe_key`]）のうち、まとめて読むものの印を付けて
    /// コミットする（DB の読み 1 回と書き 1 回）。印のあとで、コピー・ハッシュ・配置と probe を並べて行ってよい。
    pub fn begin(db: &Db, job: i64, paths: Vec<String>) -> Result<Self> {
        let (suspects, crashed) = db.read(|c| {
            Ok((
                str_list(c, PROBE_SUSPECTS_KEY)?
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                str_list(c, PROBE_CRASHED_KEY)?
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
            ))
        })?;
        let together: Vec<String> = paths
            .into_iter()
            .filter(|p| !suspects.contains(p) && !crashed.contains(p))
            .collect();
        if !together.is_empty() {
            db.write(move |tx| mark_probe(tx, job, &together, false))?;
        }
        Ok(Self {
            job,
            suspects,
            crashed,
        })
    }

    /// ファイルの読み方。
    pub fn way(&self, key: &str) -> ProbeWay {
        if self.crashed.contains(key) {
            ProbeWay::Skip
        } else if self.suspects.contains(key) {
            ProbeWay::Alone
        } else {
            ProbeWay::Together
        }
    }

    /// 並べて読む段で：まとめて読むものなら読む。1 件ずつ読むもの・読まないものは None（あとで [`Self::probe_alone`]）。
    pub fn probe_together(
        &self,
        key: &str,
        backend: &dyn MediaBackend,
        path: &Path,
        kind: MediaKind,
    ) -> Option<MediaInfo> {
        match self.way(key) {
            ProbeWay::Together => backend.probe(path, kind).ok(),
            ProbeWay::Alone | ProbeWay::Skip => None,
        }
    }

    /// 並べて読む段のあとで：前回まとめて読んでいて落ちたファイルを、1 件だけの印を付けてから読む。
    pub fn probe_alone(
        &self,
        db: &Db,
        key: &str,
        backend: &dyn MediaBackend,
        path: &Path,
        kind: MediaKind,
    ) -> Result<Option<MediaInfo>> {
        let (job, one) = (self.job, vec![key.to_string()]);
        db.write(move |tx| mark_probe(tx, job, &one, true))?;
        Ok(backend.probe(path, kind).ok())
    }
}

/// 読んだ結果を DB に書くトランザクションで：ジョブ `job` の印を消し、入れたファイル `done`（[`probe_key`]）を疑わしいもの・
/// 落ちたものの覚えから外す（落ちたものは、入れるアイテムをサムネイルを作れなかったものにしてある）。
pub fn settle_probes(tx: &Connection, job: i64, done: &[String]) -> Result<()> {
    let mut all = marks(tx)?;
    let before = all.len();
    all.retain(|m| m.job != job);
    if all.len() != before {
        set_marks(tx, &all)?;
    }
    if done.is_empty() {
        return Ok(());
    }
    let done: BTreeSet<&String> = done.iter().collect();
    for key in [PROBE_SUSPECTS_KEY, PROBE_CRASHED_KEY] {
        let list = str_list(tx, key)?;
        if list.iter().any(|p| done.contains(p)) {
            set_str_list(
                tx,
                key,
                list.into_iter().filter(|p| !done.contains(p)).collect(),
            )?;
        }
    }
    Ok(())
}

/// 前回落ちたときの probe の印を片付ける（開くとき、ジョブを動かす前に）：まとめての印のファイルは疑わしいものに、
/// 1 件だけの印のファイルは落ちたものにする。(疑わしいものに足した数, 落ちたものに足した数) を返す。
pub fn recover_probes(tx: &Connection) -> Result<(usize, usize)> {
    let all = marks(tx)?;
    if all.is_empty() {
        return Ok((0, 0));
    }
    let mut suspects = str_list(tx, PROBE_SUSPECTS_KEY)?;
    let mut crashed = str_list(tx, PROBE_CRASHED_KEY)?;
    let (mut new_suspects, mut new_crashed) = (0, 0);
    for m in &all {
        for p in &m.paths {
            if m.solo {
                suspects.retain(|s| s != p);
                if !crashed.contains(p) {
                    crashed.push(p.clone());
                    new_crashed += 1;
                }
            } else if !suspects.contains(p) && !crashed.contains(p) {
                suspects.push(p.clone());
                new_suspects += 1;
            }
        }
    }
    if new_crashed > 0 {
        tracing::warn!(
            count = new_crashed,
            "前回、メディア情報を読んでいる途中でアプリが終わったファイルは、もう読まずに入れます"
        );
    }
    if new_suspects > 0 {
        tracing::warn!(
            count = new_suspects,
            "前回、メディア情報をまとめて読んでいる途中でアプリが終わりました。そのとき読んでいたものを 1 件ずつ読み直します"
        );
    }
    set_str_list(tx, PROBE_SUSPECTS_KEY, suspects)?;
    set_str_list(tx, PROBE_CRASHED_KEY, crashed)?;
    set_marks(tx, &[])?;
    Ok((new_suspects, new_crashed))
}

/// 前回このファイルを読んでいて落ちた（[`ProbeWay::Skip`]）アイテムを、サムネイルを作れなかったものにする（サムネイルの
/// ジョブも同じ OS のデコーダで読んで落ちないように。作り終えた絵は変えない）。
pub fn fail_crashed_thumb(tx: &Connection, item_id: i64) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "UPDATE items SET thumb_state = 'failed', updated_at = ?2
         WHERE id = ?1 AND thumb_state = 'pending'",
    )?;
    stmt.execute(rusqlite::params![item_id, now_ms()])?;
    Ok(())
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

    fn k(n: u8) -> String {
        format!("/src/{n}.otf")
    }

    fn ways(conn: &Connection, keys: &[String]) -> Vec<ProbeWay> {
        let suspects: BTreeSet<String> = str_list(conn, PROBE_SUSPECTS_KEY)
            .unwrap()
            .into_iter()
            .collect();
        let crashed: BTreeSet<String> = str_list(conn, PROBE_CRASHED_KEY)
            .unwrap()
            .into_iter()
            .collect();
        let batch = ProbeBatch {
            job: 0,
            suspects,
            crashed,
        };
        keys.iter().map(|key| batch.way(key)).collect()
    }

    /// 検証 S2：まとめての印は疑わしいもの（1 件ずつ読む）に、1 件だけの印は落ちたもの（もう読まない）にし、印を消す。
    /// 入れたら覚えから外す。ほかのジョブの印は残す。
    #[test]
    fn probe_marks_become_suspects_or_crashed() {
        let conn = test_conn();
        assert_eq!(recover_probes(&conn).unwrap(), (0, 0));
        mark_probe(&conn, 1, &[k(1), k(2)], false).unwrap();
        mark_probe(&conn, 2, &[k(3)], true).unwrap();
        // 同じジョブの印は置き換える
        mark_probe(&conn, 1, &[k(1), k(2)], false).unwrap();
        assert_eq!(marks(&conn).unwrap().len(), 2);
        assert_eq!(recover(&conn).unwrap(), Recovered::default());
        assert!(marks(&conn).unwrap().is_empty());
        assert_eq!(
            ways(&conn, &[k(1), k(2), k(3), k(4)]),
            vec![
                ProbeWay::Alone,
                ProbeWay::Alone,
                ProbeWay::Skip,
                ProbeWay::Together
            ]
        );
        // 疑わしいものを 1 件だけで読んでいて落ちたら、落ちたものに移す
        mark_probe(&conn, 5, &[k(1)], true).unwrap();
        mark_probe(&conn, 6, &[k(4)], false).unwrap();
        assert_eq!(recover_probes(&conn).unwrap(), (1, 1));
        assert_eq!(
            ways(&conn, &[k(1), k(2), k(4)]),
            vec![ProbeWay::Skip, ProbeWay::Alone, ProbeWay::Alone]
        );
        // 入れたら外す。印はそのジョブのものだけ消す
        mark_probe(&conn, 7, &[k(2)], true).unwrap();
        mark_probe(&conn, 8, &[k(9)], false).unwrap();
        settle_probes(&conn, 7, &[k(1), k(2), k(3)]).unwrap();
        assert_eq!(
            marks(&conn)
                .unwrap()
                .iter()
                .map(|m| m.job)
                .collect::<Vec<_>>(),
            vec![8]
        );
        assert_eq!(str_list(&conn, PROBE_SUSPECTS_KEY).unwrap(), vec![k(4)]);
        assert!(
            settings::get::<Vec<String>>(&conn, PROBE_CRASHED_KEY)
                .unwrap()
                .is_none()
        );
    }

    /// 覚えは上限の数まで（古いものから捨てる）。
    #[test]
    fn probe_memos_are_bounded() {
        let conn = test_conn();
        let many: Vec<String> = (0..MAX_PROBE_MEMO + 10).map(|i| format!("/{i}")).collect();
        set_str_list(&conn, PROBE_SUSPECTS_KEY, many.clone()).unwrap();
        let kept = str_list(&conn, PROBE_SUSPECTS_KEY).unwrap();
        assert_eq!(kept.len(), MAX_PROBE_MEMO);
        assert_eq!(kept.first(), many.get(10));
    }

    /// 落ちたもののサムネイルは、作っていなければ作れなかったものにする（作り終えた絵は変えない）。
    #[test]
    fn crashed_items_fail_their_pending_thumbnail() {
        let conn = test_conn();
        add(&conn, 1, "pending");
        add(&conn, 2, "ready");
        fail_crashed_thumb(&conn, 1).unwrap();
        fail_crashed_thumb(&conn, 2).unwrap();
        assert_eq!(state(&conn, 1), "failed");
        assert_eq!(state(&conn, 2), "ready");
    }
}
