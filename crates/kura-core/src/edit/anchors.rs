//! アンカーの行（`edit_anchors`。再現を約束しないデコーダで読んだ元を、保存のジョブが可逆形式で残したもの。docs/editing.md
//! §9.4、docs/data-model.md §3）。行は「元のファイル × 読み方の設定」ごとに 1 つで、ファイルは台帳 `blobs` の行を指す。
//!
//! 保存のジョブ（`edit::save`）が最初の保存で作り（[`insert`]）、以後の保存・作り直しはアンカーから読む（[`find`]）。ゴミ箱を
//! 空にするときは、どの版の入力からも参照されなくなった行を片付ける（[`sweep_unreferenced`]）。

use kura_engine::Digest;
use kura_engine::codec::DecodeStamp;
use kura_engine::digest::{Hasher, canonical};
use kura_engine::doc::DecodeOptions;
use rusqlite::{Connection, OptionalExtension, params};

use super::from_engine;
use crate::library::ledger;
use crate::{CoreError, Result};

/// 読み方の設定のダイジェストのドメイン。
const OPTIONS_DOMAIN: &str = "kura-decode-options-v1";

/// 読み方の設定（`DecodeOptions`）のダイジェスト（正規の JSON の、ドメイン付きの BLAKE3。アンカーの行の鍵の片方）。
pub fn options_digest(opts: &DecodeOptions) -> Result<[u8; 32]> {
    let json = canonical(opts).map_err(|e| from_engine("読み方の設定を正規の形にできません", e))?;
    Ok(Hasher::new(OPTIONS_DOMAIN).bytes_with_len(&json).finish().0)
}

/// アンカーの行と、そのファイルの台帳の行。
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub id: i64,
    /// 元のファイルの中身。
    pub source_hash: [u8; 32],
    /// 元を読んだ記録（アンカーを読み直した値のダイジェストはこの `value_digest` と同じ）。
    pub decoded: DecodeStamp,
    /// アンカーのファイルの台帳の行。
    pub blob_id: i64,
    /// アンカーのファイルの中身・拡張子・大きさ（`files/<hash>.<ext>`）。
    pub hash: [u8; 32],
    pub ext: String,
    pub size_bytes: i64,
    pub created_at: i64,
}

/// 元 `source` × 読み方の設定 `options_digest` のアンカー（なければ None。`UNIQUE(source_hash, options_digest)` の索引で引く）。
pub fn find(
    conn: &Connection,
    source: &[u8; 32],
    options_digest: &[u8; 32],
) -> Result<Option<Anchor>> {
    let mut stmt = conn.prepare_cached(
        "SELECT a.id, a.decoded, a.blob_id, b.hash, b.ext, b.size_bytes, a.created_at
           FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id
          WHERE a.source_hash = ?1 AND a.options_digest = ?2",
    )?;
    let row = stmt
        .query_row(params![&source[..], &options_digest[..]], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .optional()?;
    let Some((id, decoded, blob_id, hash, ext, size_bytes, created_at)) = row else {
        return Ok(None);
    };
    let decoded: DecodeStamp = serde_json::from_str(&decoded).map_err(|e| {
        CoreError::Invalid(format!("アンカー {id} の読み取りの記録を読めません: {e}"))
    })?;
    let hash = <[u8; 32]>::try_from(hash.as_slice()).map_err(|_| {
        CoreError::Invalid(format!(
            "アンカー {id} の台帳の行のハッシュが 32 バイトではありません"
        ))
    })?;
    Ok(Some(Anchor {
        id,
        source_hash: *source,
        decoded,
        blob_id,
        hash,
        ext,
        size_bytes,
        created_at,
    }))
}

/// 足すアンカー（ファイルは置いてある）。
pub(crate) struct NewAnchor<'a> {
    pub source: [u8; 32],
    pub options: &'a DecodeOptions,
    pub decoded: &'a DecodeStamp,
    /// アンカーのファイルの中身・拡張子・大きさ。
    pub hash: [u8; 32],
    pub ext: &'a str,
    pub size: u64,
    pub now: i64,
}

/// アンカーのファイルを台帳に載せ（`ledger::register`）、行を足して ID を返す（呼び手のトランザクションの中で）。同じ元 ×
/// 読み方の設定の行がすでにあれば、同じファイルを指すときだけその行を使い、別のファイルを指していれば Conflict（台帳の行を
/// 参照されないまま残さない。トランザクションごと戻し、呼び手が置いたファイルを片付ける）。
pub(crate) fn insert(tx: &Connection, a: &NewAnchor<'_>) -> Result<i64> {
    let blob = ledger::register(tx, &a.hash, a.ext, a.size, a.now)?;
    let options =
        canonical(a.options).map_err(|e| from_engine("読み方の設定を正規の形にできません", e))?;
    let options = String::from_utf8(options)
        .map_err(|e| CoreError::Invalid(format!("読み方の設定が UTF-8 ではありません: {e}")))?;
    let decoded =
        canonical(a.decoded).map_err(|e| from_engine("読み取りの記録を正規の形にできません", e))?;
    let decoded = String::from_utf8(decoded)
        .map_err(|e| CoreError::Invalid(format!("読み取りの記録が UTF-8 ではありません: {e}")))?;
    let digest = options_digest(a.options)?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO edit_anchors (source_hash, options_digest, options, decoded, value_digest, blob_id, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(source_hash, options_digest) DO NOTHING",
    )?;
    stmt.execute(params![
        &a.source[..],
        &digest[..],
        options,
        decoded,
        &a.decoded.value_digest.0[..],
        blob,
        a.now
    ])?;
    let mut stmt = tx.prepare_cached(
        "SELECT id, blob_id FROM edit_anchors WHERE source_hash = ?1 AND options_digest = ?2",
    )?;
    let (id, kept): (i64, i64) = stmt.query_row(params![&a.source[..], &digest[..]], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    if kept != blob {
        return Err(CoreError::Conflict(format!(
            "元 {} のアンカーが別のファイルで先にあります",
            Digest(a.source)
        )));
    }
    Ok(id)
}

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

    fn stamp(value: u8) -> DecodeStamp {
        DecodeStamp {
            codec: "fake-heic-dec/1".into(),
            tier: kura_engine::codec::Tier::Platform,
            reproducible: false,
            value_digest: Digest([value; 32]),
            env: None,
        }
    }

    /// 足したアンカーを元 × 読み方の設定で引ける。同じファイルなら同じ行、別のファイルなら Conflict（台帳の行は戻る）。
    #[test]
    fn insert_then_find_by_source_and_options() {
        let conn = test_conn();
        let opts = DecodeOptions::default();
        let od = options_digest(&opts).unwrap();
        assert_eq!(od, options_digest(&DecodeOptions::default()).unwrap());
        assert!(find(&conn, &[1; 32], &od).unwrap().is_none());
        let decoded = stamp(4);
        let new = |hash: [u8; 32]| NewAnchor {
            source: [1; 32],
            options: &opts,
            decoded: &decoded,
            hash,
            ext: "png",
            size: 9,
            now: 3,
        };
        let id = insert(&conn, &new([7; 32])).unwrap();
        let found = find(&conn, &[1; 32], &od).unwrap().unwrap();
        assert_eq!(found.id, id);
        assert_eq!(
            (found.hash, found.ext.as_str(), found.size_bytes),
            ([7; 32], "png", 9)
        );
        assert_eq!(found.decoded, decoded);
        assert_eq!(
            insert(&conn, &new([7; 32])).unwrap(),
            id,
            "同じファイルなら同じ行"
        );
        assert!(matches!(
            insert(&conn, &new([8; 32])),
            Err(CoreError::Conflict(_))
        ));
        assert!(find(&conn, &[2; 32], &od).unwrap().is_none());
        let plan = query_plan(
            &conn,
            "SELECT a.id, a.decoded, a.blob_id, b.hash, b.ext, b.size_bytes, a.created_at
               FROM edit_anchors a JOIN blobs b ON b.id = a.blob_id
              WHERE a.source_hash = x'01' AND a.options_digest = x'02'",
        );
        assert!(plan.contains("sqlite_autoindex_edit_anchors_1"), "{plan}");
        assert!(!plan.contains("SCAN"), "{plan}");
    }
}
