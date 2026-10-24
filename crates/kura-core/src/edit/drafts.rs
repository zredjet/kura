//! 下書き（`edit_drafts`。docs/editing.md §12.12・§14.1 の S17、docs/data-model.md §4「保存と版（M12）」）。
//!
//! 下書きの形式は作らない：エディタの頭の文書（内容アドレスの部品。`edit::store`）と、その文書の土台（開いたときの今と頭の
//! 版。保存の [`Base`]）だけを持つ。アイテムごとに 1 つ。アプリが落ちても、開き直すと同じ文書（同じ根）に戻せる。
//!
//! - 開くとき（[`open`]）、土台が今と同じで、土台を待っている保存のジョブ（`after_job`）が動いていなければ `fresh`。偽なら、
//!   下書きを置いたあとに別の保存でアイテムが変わったので、その土台で保存すると Conflict になる（利用者に選ばせる）。
//! - エディタが保存を積んだあとも編集を続けて置いた下書きは、`after_job` にそのジョブを書く（土台はそのジョブの結果）。
//!   保存のジョブのトランザクション 2 が [`settle_after_save`] で、保存した文書と同じ下書きを消し、`after_job` が自分の
//!   下書きは土台を新しい頭に付け替える。ジョブがもうコミットしていれば、置くときに付け替える（[`save`]）。
//! - アイテムを消すと下書きも消える（CASCADE）。部品の行は残る（段 3 の GC が片付ける）。

use kura_engine::doc_parts::{DocParts, split};
use kura_engine::{Digest, Graph};
use rusqlite::{Connection, OptionalExtension, params};

use super::save::Base;
use super::{EngineRuntime, from_engine, store, versions};
use crate::db::Db;
use crate::time::now_ms;
use crate::{CoreError, Result};

/// 下書きの行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub item_id: i64,
    /// 文書の根の部品の行と、そのダイジェスト。
    pub doc_id: i64,
    pub doc: Digest,
    /// 文書の土台（開いたときの今と頭の版）。
    pub base: Base,
    /// 土台を待っている保存のジョブ（そのジョブがコミットすると、土台がその版に付け替わる）。
    pub after_job: Option<i64>,
    pub updated_at: i64,
}

/// 開いた下書き。
#[derive(Debug, Clone, PartialEq)]
pub struct OpenedDraft {
    /// 文書（`Engine::load` を通したもの。古いスキーマは移してある）。
    pub graph: Graph,
    /// 置いたときの根のダイジェスト。
    pub doc: Digest,
    pub base: Base,
    /// 土台が今と同じで、待っている保存のジョブがない（このまま保存できる）。
    pub fresh: bool,
    pub after_job: Option<i64>,
    pub updated_at: i64,
}

/// 下書きを置く（アイテムごとに 1 つ。前のものは置き換える）。文書を部品に分けて DB に置き、根のダイジェストを返す。
/// `after_job` は、エディタが積んだ保存のジョブ（`base` はそのジョブの土台）。そのジョブがもうコミットしていれば、置くときに
/// 付け替える（保存した文書と同じなら下書きを置かない）。アイテムがなければ NotFound。
pub fn save(
    db: &Db,
    item_id: i64,
    graph: &Graph,
    base: Base,
    after_job: Option<i64>,
) -> Result<Digest> {
    let parts = split(graph).map_err(|e| from_engine("下書きの文書を部品に分けられません", e))?;
    let root = parts.root;
    db.write(move |tx| put_draft(tx, item_id, &parts, base, after_job, now_ms()))?;
    Ok(root)
}

/// [`save`] の DB の側（呼び手のトランザクションの中で）。
fn put_draft(
    tx: &Connection,
    item_id: i64,
    parts: &DocParts,
    base: Base,
    after_job: Option<i64>,
    now: i64,
) -> Result<()> {
    if versions::head(tx, item_id)?.is_none() {
        return Err(CoreError::NotFound(format!("アイテム {item_id}")));
    }
    let doc_id = store::put_doc(tx, parts, now)?;
    let (mut base, mut after_job) = (base, after_job);
    if let Some(job) = after_job
        && let Some(v) = versions::by_job(tx, job)?
    {
        // 待っていた保存がもうコミットした（settle_after_save より後に置いた）：同じ文書なら保存していない変更はないので
        // 置かない。違えば、その版を土台にする（別名で保存なら版は新しいアイテムのもので、このアイテムの今は変わらない
        // ので土台はそのまま）
        if saved_doc_of(tx, &v)? == Some(doc_id) {
            tx.prepare_cached("DELETE FROM edit_drafts WHERE item_id = ?1")?
                .execute([item_id])?;
            return Ok(());
        }
        if v.item_id == item_id {
            base = Base {
                hash: v.output_hash,
                version: v.version,
            };
        }
        after_job = None;
    }
    tx.prepare_cached(
        "INSERT INTO edit_drafts (item_id, doc_id, base_hash, base_version, after_job, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(item_id) DO UPDATE SET doc_id = excluded.doc_id, base_hash = excluded.base_hash,
                base_version = excluded.base_version, after_job = excluded.after_job,
                updated_at = excluded.updated_at",
    )?
    .execute(params![
        item_id,
        doc_id,
        &base.hash[..],
        base.version,
        after_job,
        now
    ])?;
    Ok(())
}

/// アイテムの下書き（なければ None）。
pub fn get(conn: &Connection, item_id: i64) -> Result<Option<Draft>> {
    let mut stmt = conn.prepare_cached(
        "SELECT d.doc_id, o.digest, d.base_hash, d.base_version, d.after_job, d.updated_at
           FROM edit_drafts d JOIN edit_objects o ON o.id = d.doc_id
          WHERE d.item_id = ?1",
    )?;
    let row = stmt
        .query_row([item_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })
        .optional()?;
    let Some((doc_id, doc, base_hash, base_version, after_job, updated_at)) = row else {
        return Ok(None);
    };
    let bytes32 = |v: Vec<u8>, what: &str| {
        <[u8; 32]>::try_from(v.as_slice()).map_err(|_| {
            CoreError::Invalid(format!(
                "アイテム {item_id} の下書きの{what}が 32 バイトではありません"
            ))
        })
    };
    Ok(Some(Draft {
        item_id,
        doc_id,
        doc: Digest(bytes32(doc, "文書")?),
        base: Base {
            hash: bytes32(base_hash, "土台")?,
            version: base_version,
        },
        after_job,
        updated_at,
    }))
}

/// 下書きがこのまま保存できるか：アイテムがゴミ箱になく、土台が今（`items.hash`）と頭の版に等しく、待っている保存のジョブ
/// （`after_job`）が動いていない（待ち・実行中・一時停止でない）。
pub fn is_fresh(conn: &Connection, draft: &Draft) -> Result<bool> {
    let Some(head) = versions::head(conn, draft.item_id)? else {
        return Ok(false);
    };
    if head.trashed || head.hash != draft.base.hash || head.version != draft.base.version {
        return Ok(false);
    }
    let Some(job) = draft.after_job else {
        return Ok(true);
    };
    let live: Option<bool> = conn
        .prepare_cached("SELECT state IN ('queued', 'running', 'paused') FROM jobs WHERE id = ?1")?
        .query_row([job], |r| r.get(0))
        .optional()?;
    Ok(!live.unwrap_or(false))
}

/// 下書きを開く（なければ None）：部品をつないで、エディタのプール（`runtime.preview`）で `Engine::load` に通す。
pub fn open(db: &Db, runtime: &EngineRuntime, item_id: i64) -> Result<Option<OpenedDraft>> {
    let Some((draft, bytes, fresh)) = db.read(|c| {
        let Some(draft) = get(c, item_id)? else {
            return Ok(None);
        };
        let bytes = store::load_doc_bytes(c, draft.doc_id)?;
        let fresh = is_fresh(c, &draft)?;
        Ok(Some((draft, bytes, fresh)))
    })?
    else {
        return Ok(None);
    };
    let graph = runtime
        .preview(|e| e.load(&bytes))
        .map_err(|e| from_engine("下書きの文書を読めません", e))?;
    Ok(Some(OpenedDraft {
        graph,
        doc: draft.doc,
        base: draft.base,
        fresh,
        after_job: draft.after_job,
        updated_at: draft.updated_at,
    }))
}

/// 下書きを捨てる（あったら真）。部品の行は残る（段 3 の GC）。
pub fn discard(db: &Db, item_id: i64) -> Result<bool> {
    let n = db.write(move |tx| {
        Ok(tx
            .prepare_cached("DELETE FROM edit_drafts WHERE item_id = ?1")?
            .execute([item_id])?)
    })?;
    Ok(n > 0)
}

/// 空の文書（版 0 と、版 0 に戻した版の文書）の根の行（DB になければ None。下書きが空の文書なら、その行がある）。
pub(crate) fn empty_doc_id(conn: &Connection) -> Result<Option<i64>> {
    let parts = split(&Graph::new_single())
        .map_err(|e| from_engine("空の文書を部品に分けられません", e))?;
    store::doc_id_by_digest(conn, &parts.root)
}

/// 版が保存した文書の根の行（文書のない版（版 0・版 0 に戻した版）は空の文書）。
fn saved_doc_of(conn: &Connection, v: &versions::Version) -> Result<Option<i64>> {
    match v.doc_id {
        Some(doc) => Ok(Some(doc)),
        None => empty_doc_id(conn),
    }
}

/// 保存のジョブ `job` のトランザクション 2 で呼ぶ：アイテムの下書きが保存した文書（`saved_doc_id`）と同じで、どのジョブも
/// 待っていないか `job` を待っていたなら消す。`job` を待っていた下書き（保存を積んだあとも編集を続けたもの）は、土台を
/// 新しい頭 `new_base` に付け替える。ほかの下書き（別の保存の前に置いたもの）は残す（開くと fresh が偽）。
pub(crate) fn settle_after_save(
    tx: &Connection,
    item_id: i64,
    saved_doc_id: Option<i64>,
    job: i64,
    new_base: Base,
) -> Result<()> {
    if let Some(doc_id) = saved_doc_id {
        tx.prepare_cached(
            "DELETE FROM edit_drafts
              WHERE item_id = ?1 AND doc_id = ?2 AND (after_job IS NULL OR after_job = ?3)",
        )?
        .execute(params![item_id, doc_id, job])?;
    }
    tx.prepare_cached(
        "UPDATE edit_drafts SET base_hash = ?3, base_version = ?4, after_job = NULL
          WHERE item_id = ?1 AND after_job = ?2",
    )?
    .execute(params![item_id, job, &new_base.hash[..], new_base.version])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use crate::edit::testing::{insert_item, insert_version};
    use serde_json::json;

    fn doc(axis: &str) -> Graph {
        let mut g = Graph::new_single();
        g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
            .unwrap();
        g
    }

    fn put(conn: &Connection, g: &Graph) -> i64 {
        store::put_doc(conn, &split(g).unwrap(), 1).unwrap()
    }

    fn insert_draft(conn: &Connection, item: i64, doc_id: i64, base: Base, after: Option<i64>) {
        conn.execute(
            "INSERT INTO edit_drafts (item_id, doc_id, base_hash, base_version, after_job, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![item, doc_id, &base.hash[..], base.version, after],
        )
        .unwrap();
    }

    fn base(hash: [u8; 32], version: i64) -> Base {
        Base { hash, version }
    }

    /// 保存した文書と同じ下書き（待っていない・このジョブを待っていた）は消え、このジョブを待っていた別の文書の下書きは
    /// 土台が付け替わり、ほかのジョブを待つもの・別の文書の下書きは残る。
    #[test]
    fn settle_deletes_saved_and_rebases_waiting_drafts() {
        let conn = test_conn();
        let (a, b, c, d) = (
            insert_item(&conn, 1, [1; 32]),
            insert_item(&conn, 2, [2; 32]),
            insert_item(&conn, 3, [3; 32]),
            insert_item(&conn, 4, [4; 32]),
        );
        let (saved, other) = (put(&conn, &doc("vertical")), put(&conn, &doc("horizontal")));
        insert_draft(&conn, a, saved, base([1; 32], 0), None);
        insert_draft(&conn, b, saved, base([2; 32], 0), Some(7));
        insert_draft(&conn, c, other, base([3; 32], 0), Some(7));
        insert_draft(&conn, d, saved, base([4; 32], 0), Some(8));
        for item in [a, b, c, d] {
            settle_after_save(&conn, item, Some(saved), 7, base([9; 32], 1)).unwrap();
        }
        assert!(get(&conn, a).unwrap().is_none());
        assert!(get(&conn, b).unwrap().is_none());
        let moved = get(&conn, c).unwrap().unwrap();
        assert_eq!(
            (moved.base, moved.after_job),
            (base([9; 32], 1), None),
            "このジョブを待っていた下書きは新しい頭の上"
        );
        let kept = get(&conn, d).unwrap().unwrap();
        assert_eq!((kept.base, kept.after_job), (base([4; 32], 0), Some(8)));
        // 文書のない版（元に戻した版）では消さず、付け替えだけ
        insert_draft(&conn, a, saved, base([1; 32], 0), Some(5));
        settle_after_save(&conn, a, None, 5, base([1; 32], 2)).unwrap();
        assert_eq!(get(&conn, a).unwrap().unwrap().base, base([1; 32], 2));
    }

    /// fresh：土台が今と頭の版に等しく、ゴミ箱になく、待っている保存のジョブが動いていない。
    #[test]
    fn freshness_follows_the_head_and_the_waited_job() {
        let conn = test_conn();
        let a = insert_item(&conn, 1, [1; 32]);
        let g = put(&conn, &doc("vertical"));
        insert_draft(&conn, a, g, base([1; 32], 0), None);
        let draft = get(&conn, a).unwrap().unwrap();
        assert_eq!(draft.doc, split(&doc("vertical")).unwrap().root);
        assert!(is_fresh(&conn, &draft).unwrap());
        // 待っているジョブ：動いていれば偽、終わっていれば（失敗・中止でも）真、なくても真
        for (state, fresh) in [
            ("queued", false),
            ("running", false),
            ("paused", false),
            ("failed", true),
            ("canceled", true),
        ] {
            conn.execute("DELETE FROM jobs", []).unwrap();
            conn.execute(
                "INSERT INTO jobs (id, kind, state, input_json, created_at) VALUES (7, 'edit', ?1, '{}', 1)",
                [state],
            )
            .unwrap();
            let waiting = Draft {
                after_job: Some(7),
                ..draft.clone()
            };
            assert_eq!(is_fresh(&conn, &waiting).unwrap(), fresh, "{state}");
        }
        let gone = Draft {
            after_job: Some(99),
            ..draft.clone()
        };
        assert!(is_fresh(&conn, &gone).unwrap());
        // 版が足された・ゴミ箱に入れられた
        insert_version(&conn, a, 0, [1; 32], "hold", None);
        insert_version(&conn, a, 1, [1; 32], "hold", None);
        assert!(!is_fresh(&conn, &draft).unwrap());
        let moved = Draft {
            base: base([1; 32], 1),
            ..draft.clone()
        };
        assert!(is_fresh(&conn, &moved).unwrap());
        conn.execute("UPDATE items SET trashed_at = 5 WHERE id = ?1", [a])
            .unwrap();
        assert!(!is_fresh(&conn, &moved).unwrap());
    }

    /// 待っていた保存がもうコミットしていれば、置くときに付け替える：上書き・版に戻すならその版を土台に、別名で保存（版は
    /// 新しいアイテムのもの）なら土台はそのまま。どちらも保存した文書と同じなら置かない。ないアイテムは NotFound。
    #[test]
    fn saving_after_the_waited_job_committed_settles_at_once() {
        let conn = test_conn();
        let (a, b) = (
            insert_item(&conn, 1, [1; 32]),
            insert_item(&conn, 2, [2; 32]),
        );
        insert_version(&conn, a, 0, [5; 32], "hold", None);
        insert_version(&conn, a, 1, [1; 32], "hold", None);
        insert_version(&conn, b, 0, [5; 32], "hold", None);
        insert_version(&conn, b, 1, [2; 32], "hold", None);
        conn.execute(
            "UPDATE item_versions SET job_id = CASE item_id WHEN 1 THEN 7 ELSE 8 END WHERE version = 1",
            [],
        )
        .unwrap();
        let parts = |g: &Graph| split(g).unwrap();
        let editing = doc("vertical");
        // 上書き（ジョブ 7）のあと：版 1 が土台
        put_draft(&conn, a, &parts(&editing), base([5; 32], 0), Some(7), 2).unwrap();
        let d = get(&conn, a).unwrap().unwrap();
        assert_eq!((d.base, d.after_job), (base([1; 32], 1), None));
        // 別名で保存（ジョブ 8 の版は b のもの）のあと：a の土台はそのまま
        put_draft(&conn, a, &parts(&editing), base([5; 32], 0), Some(8), 3).unwrap();
        let d = get(&conn, a).unwrap().unwrap();
        assert_eq!((d.base, d.after_job), (base([5; 32], 0), None));
        // 保存した文書（テストの版の文書は空の文書）と同じなら置かない
        put_draft(
            &conn,
            a,
            &parts(&Graph::new_single()),
            base([5; 32], 0),
            Some(7),
            4,
        )
        .unwrap();
        assert!(get(&conn, a).unwrap().is_none());
        // まだコミットしていないジョブ：そのまま待つ
        put_draft(&conn, a, &parts(&editing), base([1; 32], 1), Some(9), 5).unwrap();
        assert_eq!(get(&conn, a).unwrap().unwrap().after_job, Some(9));
        assert!(matches!(
            put_draft(&conn, 99, &parts(&editing), base([1; 32], 1), None, 6),
            Err(CoreError::NotFound(_))
        ));
    }

    #[test]
    fn queries_use_their_indexes() {
        let conn = test_conn();
        let plan = query_plan(
            &conn,
            "SELECT d.doc_id, o.digest, d.base_hash, d.base_version, d.after_job, d.updated_at
               FROM edit_drafts d JOIN edit_objects o ON o.id = d.doc_id WHERE d.item_id = 1",
        );
        assert!(!plan.contains("SCAN"), "{plan}");
        for sql in [
            "DELETE FROM edit_drafts WHERE item_id = 1 AND doc_id = 2 AND (after_job IS NULL OR after_job = 3)",
            "UPDATE edit_drafts SET base_hash = x'00', base_version = 1, after_job = NULL WHERE item_id = 1 AND after_job = 3",
            "SELECT state IN ('queued', 'running', 'paused') FROM jobs WHERE id = 1",
        ] {
            let plan = query_plan(&conn, sql);
            assert!(!plan.contains("SCAN"), "{sql}: {plan}");
        }
    }
}
