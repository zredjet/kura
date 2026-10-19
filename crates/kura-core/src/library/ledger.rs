//! 実ファイルの台帳 `blobs`（0007。docs/data-model.md §3・§4）。`files/ab/cd/<hash>.<ext>` の 1 ファイルに 1 行。
//!
//! 容量・重複の見込み・整合性チェックは台帳から数える。台帳への登録は items のトリガが行い、
//! 台帳の行を消すのは `release` だけ（どの表からも参照されない行だけを消す。M12 の版・アンカーの行を消さないように）。

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension};

use super::layout::{content_rel_path, hex};
use crate::Result;

/// 台帳の行を参照する表と列（ON DELETE CASCADE で付いているもの＝中身ごとの事実と結果は除く）。
/// `blobs` を参照する表を足したら、ここにも足す（`release_sees_every_table_that_references_the_ledger` が見張る）。
const REFERRERS: &[(&str, &str, &str)] = &[("items", "hash", "hash")];

/// 解放した台帳の行（実ファイルと、サムネイルなどの派生のファイルを消すのに使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleasedBlob {
    pub hash: [u8; 32],
    pub ext: String,
    pub size_bytes: i64,
}

/// ライブラリのファイルの合計サイズ（ゴミ箱のものを含む。ファイルはゴミ箱を空にするまで残るので）。
pub fn total_size(conn: &Connection) -> Result<u64> {
    let mut stmt = conn.prepare_cached("SELECT coalesce(sum(size_bytes), 0) FROM blobs")?;
    let total: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(u64::try_from(total).unwrap_or(0))
}

/// 同じサイズの台帳の行の (ハッシュ, 拡張子)。重複の見込みに使う。
pub fn candidates_by_size(conn: &Connection, size: u64) -> Result<Vec<([u8; 32], String)>> {
    let Ok(size) = i64::try_from(size) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare_cached("SELECT hash, ext FROM blobs WHERE size_bytes = ?1")?;
    let rows = stmt.query_map([size], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (hash, ext) = row?;
        if let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) {
            out.push((hash, ext));
        }
    }
    Ok(out)
}

/// 台帳にある中身の拡張子（`files/<hash>.<ext>` の ext。なければ None）。
pub fn ext_of(conn: &Connection, hash: &[u8; 32]) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT ext FROM blobs WHERE hash = ?1")?;
    Ok(stmt.query_row([&hash[..]], |row| row.get(0)).optional()?)
}

/// `files/` にあるはずのファイル（`files/` からの相対パス `ab/cd/<hash>.<ext>`）。整合性チェックに使う。
pub fn expected_paths(conn: &Connection) -> Result<HashSet<String>> {
    // すべての行を確かめるので全件を読む（意図した全件の走査）
    let mut stmt = conn.prepare_cached("SELECT hash, ext FROM blobs")?;
    let rows = stmt.query_map([], |row| {
        let hash: Vec<u8> = row.get(0)?;
        let ext: String = row.get(1)?;
        Ok(content_rel_path(&hex(&hash), &ext))
    })?;
    Ok(rows.collect::<std::result::Result<HashSet<_>, _>>()?)
}

/// どの表からも参照されない台帳の行の数（ゴミ箱を空にする途中で落ちたなど。整合性チェックで数える）。
pub fn unreferenced(conn: &Connection) -> Result<u64> {
    let sql = format!(
        "SELECT count(*) FROM blobs WHERE {}",
        not_referenced("blobs")
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let n: i64 = stmt.query_row([], |row| row.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// `hashes` の台帳の行のうち、どの表からも参照されなくなったものを消し、消した行を返す（呼び手のトランザクションの中で）。
/// 中身ごとの事実と結果（media_facts・analysis_results）は CASCADE で消える。実ファイルはコミットのあとで呼び手が消す（ADR-46）。
pub fn release(conn: &Connection, hashes: &[[u8; 32]]) -> Result<Vec<ReleasedBlob>> {
    let sql = format!(
        "DELETE FROM blobs WHERE hash = ?1 AND {} RETURNING ext, size_bytes",
        not_referenced("blobs")
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut out = Vec::new();
    for hash in hashes {
        let row: Option<(String, i64)> = stmt
            .query_row([&hash[..]], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?;
        if let Some((ext, size_bytes)) = row {
            out.push(ReleasedBlob {
                hash: *hash,
                ext,
                size_bytes,
            });
        }
    }
    Ok(out)
}

/// 「`table` の行がどの表からも参照されていない」の条件（`REFERRERS` の定数だけから組む。値は埋め込まない）。
fn not_referenced(table: &str) -> String {
    REFERRERS
        .iter()
        .map(|(child, column, parent)| {
            format!("NOT EXISTS (SELECT 1 FROM {child} r WHERE r.{column} = {table}.{parent})")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{query_plan, test_conn};

    fn insert(conn: &Connection, id: i64, size: i64) {
        conn.execute(
            "INSERT INTO items (id, hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
             VALUES (?1, ?2, 'jpg', 'image', ?3, 't', 't', 0, 0)",
            rusqlite::params![id, vec![id as u8; 32], size],
        )
        .unwrap();
    }

    #[test]
    fn total_size_counts_every_file_including_the_trash() {
        let conn = test_conn();
        assert_eq!(total_size(&conn).unwrap(), 0);
        insert(&conn, 1, 10);
        insert(&conn, 2, 10);
        conn.execute("UPDATE items SET trashed_at = 5 WHERE id = 2", [])
            .unwrap();
        assert_eq!(total_size(&conn).unwrap(), 20);
        let plan = query_plan(&conn, "SELECT coalesce(sum(size_bytes), 0) FROM blobs");
        assert!(plan.contains("COVERING INDEX idx_blobs_size"), "{plan}");
    }

    #[test]
    fn candidates_by_size_from_ledger_uses_index() {
        let conn = test_conn();
        insert(&conn, 1, 42);
        insert(&conn, 2, 41);
        assert_eq!(
            candidates_by_size(&conn, 42).unwrap(),
            vec![([1u8; 32], "jpg".to_string())]
        );
        assert!(candidates_by_size(&conn, 43).unwrap().is_empty());
        assert!(candidates_by_size(&conn, u64::MAX).unwrap().is_empty());
        assert_eq!(ext_of(&conn, &[1u8; 32]).unwrap().as_deref(), Some("jpg"));
        assert_eq!(ext_of(&conn, &[9u8; 32]).unwrap(), None);
        let plan = query_plan(&conn, "SELECT hash, ext FROM blobs WHERE size_bytes = 42");
        assert!(plan.contains("idx_blobs_size"), "{plan}");
    }

    #[test]
    fn release_deletes_only_rows_nobody_references() {
        let conn = test_conn();
        insert(&conn, 1, 10);
        insert(&conn, 2, 20);
        conn.execute_batch(
            "INSERT INTO media_facts (blob_id, state, facts_v, analyzed_at) SELECT id, 'ok', 1, 1 FROM blobs;
             INSERT INTO analysis_results (blob_id, analysis, version, params, encoding, output_len, digest, created_at)
               SELECT id, 'image.alpha', 1, '{}', 'json', 0, x'00', 1 FROM blobs;",
        )
        .unwrap();
        assert_eq!(unreferenced(&conn).unwrap(), 0);
        conn.execute("DELETE FROM items WHERE id = 1", []).unwrap();
        assert_eq!(unreferenced(&conn).unwrap(), 1);
        // 参照されている行（2）は消さない
        let released = release(&conn, &[[1u8; 32], [2u8; 32], [9u8; 32]]).unwrap();
        assert_eq!(
            released,
            vec![ReleasedBlob {
                hash: [1u8; 32],
                ext: "jpg".into(),
                size_bytes: 10
            }]
        );
        assert_eq!(unreferenced(&conn).unwrap(), 0);
        let left: (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT count(*) FROM blobs), (SELECT count(*) FROM media_facts),
                        (SELECT count(*) FROM analysis_results)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(left, (1, 1, 1));
        let plan = query_plan(
            &conn,
            &format!(
                "SELECT count(*) FROM blobs WHERE {}",
                not_referenced("blobs")
            ),
        );
        assert!(plan.contains("sqlite_autoindex_items"), "{plan}");
    }

    #[test]
    fn release_sees_every_table_that_references_the_ledger() {
        // blobs を参照する外部キーのうち、CASCADE でないもの（＝行が残っていれば台帳の行を消してはいけないもの）は
        // すべて REFERRERS にある（M12 の版・アンカーの表を足したら、ここが落ちる）
        let conn = test_conn();
        let mut stmt = conn
            .prepare(
                "SELECT s.name, f.\"from\", f.\"to\", f.on_delete FROM sqlite_schema s, pragma_foreign_key_list(s.name) f
                  WHERE s.type = 'table' AND f.\"table\" = 'blobs' ORDER BY s.name",
            )
            .unwrap();
        let refs: Vec<(String, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!refs.is_empty());
        for (table, from, to, on_delete) in refs {
            if on_delete == "CASCADE" {
                continue;
            }
            assert!(
                REFERRERS
                    .iter()
                    .any(|(t, f, p)| *t == table && *f == from && *p == to),
                "{table}.{from} → blobs.{to} を REFERRERS に足す"
            );
        }
    }

    #[test]
    fn expected_paths_come_from_the_ledger() {
        let conn = test_conn();
        insert(&conn, 1, 10);
        // アイテムの行がなくても、台帳にある間はファイルがあるはず
        conn.execute("DELETE FROM items WHERE id = 1", []).unwrap();
        let hex1 = hex(&[1u8; 32]);
        assert_eq!(
            expected_paths(&conn).unwrap(),
            HashSet::from([content_rel_path(&hex1, "jpg")])
        );
    }
}
