//! アンカーの行（`edit_anchors`。再現を約束しないデコーダで読んだ元を、保存のジョブが可逆形式で残したもの。docs/editing.md
//! §9.4、docs/data-model.md §3）。行は「元のファイル × 読み方の設定」ごとに 1 つで、ファイルは台帳 `blobs` の行を指す。
//!
//! 段 2 の A では、ゴミ箱を空にするときの片付け（[`sweep_unreferenced`]）だけを置く。探す・入れる口は保存のジョブ（B）で足す。

use rusqlite::{Connection, OptionalExtension};

use crate::Result;

/// `ids` のアンカーのうち、どの版の入力からも参照されなくなった行を消し、そのファイルの台帳の行の hash を返す（呼び手の
/// トランザクションの中で。hash は `ledger::release` に渡す。台帳の行はほかから参照されていれば残る）。ゴミ箱を空にする処理が、
/// 版の行を消したあとに呼ぶ（M12 の利用者の決定 6）。
pub(crate) fn sweep_unreferenced(tx: &Connection, ids: &[i64]) -> Result<Vec<[u8; 32]>> {
    let mut delete = tx.prepare_cached(
        "DELETE FROM edit_anchors WHERE id = ?1
           AND NOT EXISTS (SELECT 1 FROM item_version_inputs i WHERE i.anchor_id = edit_anchors.id)
         RETURNING blob_id",
    )?;
    let mut hash_of = tx.prepare_cached("SELECT hash FROM blobs WHERE id = ?1")?;
    let mut out = Vec::new();
    for id in ids {
        let Some(blob_id) = delete
            .query_row([id], |row| row.get::<_, i64>(0))
            .optional()?
        else {
            continue;
        };
        let hash: Option<Vec<u8>> = hash_of.query_row([blob_id], |row| row.get(0)).optional()?;
        if let Some(hash) = hash.and_then(|h| <[u8; 32]>::try_from(h.as_slice()).ok()) {
            out.push(hash);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};
    use crate::edit::testing::{insert_anchor, insert_item, insert_version};

    #[test]
    fn sweeps_only_anchors_no_input_uses() {
        let conn = test_conn();
        let (a, b) = (
            insert_item(&conn, 1, [1; 32]),
            insert_item(&conn, 2, [2; 32]),
        );
        let used = insert_anchor(&conn, [1; 32], [11; 32]);
        let free = insert_anchor(&conn, [2; 32], [12; 32]);
        insert_version(&conn, a, 0, [1; 32], "hold", None);
        insert_version(&conn, a, 1, [3; 32], "hold", Some(used));
        insert_version(&conn, b, 0, [2; 32], "hold", None);
        let swept = sweep_unreferenced(&conn, &[used, free, 999]).unwrap();
        assert_eq!(swept, vec![[12; 32]]);
        let left: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM edit_anchors").unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(left, vec![used]);
        let plan = query_plan(
            &conn,
            "DELETE FROM edit_anchors WHERE id = 1
               AND NOT EXISTS (SELECT 1 FROM item_version_inputs i WHERE i.anchor_id = edit_anchors.id)",
        );
        assert!(plan.contains("idx_item_version_inputs_anchor"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
    }
}
