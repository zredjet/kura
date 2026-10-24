//! 実ファイルの台帳 `blobs`（0007。docs/data-model.md §3・§4）。`files/ab/cd/<hash>.<ext>` の 1 ファイルに 1 行。
//!
//! 容量・重複の見込み・整合性チェックは台帳から数える。アイテムの今の登録は items のトリガが行い、版・アンカーのファイル
//! （M12）は `register` で載せる。台帳の行を消すのは `release` だけ（どの表からも参照されない行だけを消す。版・アンカーの
//! 行を消さないように）。
//!
//! 台帳にまだない中身を `files/` に置く処理（保存。段 3 の GC が消すときも）は、置く前に `announce` で予告を DB に
//! コミットし、台帳に載せるトランザクションで `clear_announced` で消す。途中で落ちたら、次に開くときに `settle_pending` が
//! 予告を照らして、台帳にないファイルを片付ける（docs/data-model.md §4「保存と版（M12）」）。

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::layout::{Layout, content_rel_path, hex};
use crate::db::Db;
use crate::{CoreError, Result};

/// 台帳の行を参照する表と列（ON DELETE CASCADE で付いているもの＝中身ごとの事実と結果は除く）。
/// `blobs` を参照する表を足したら、ここにも足す（`release_sees_every_table_that_references_the_ledger` が見張る）。
/// アイテムの今（hash）・版のファイル（猶予中の版と元。M12）・アンカー（M12）。
const REFERRERS: &[(&str, &str, &str)] = &[
    ("items", "hash", "hash"),
    ("item_versions", "file_blob_id", "id"),
    ("edit_anchors", "blob_id", "id"),
];

/// 台帳の予告の種類（`ledger_pending.kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    /// これから `files/` に置く（保存の出力・アンカー）。
    Place,
    /// これから `files/` から消す（段 3 の GC）。
    Delete,
}

impl PendingKind {
    /// DB に書く名前。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Place => "place",
            Self::Delete => "delete",
        }
    }
}

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

/// 台帳の行の ID（なければ None）。
pub fn id_of(conn: &Connection, hash: &[u8; 32]) -> Result<Option<i64>> {
    let mut stmt = conn.prepare_cached("SELECT id FROM blobs WHERE hash = ?1")?;
    Ok(stmt.query_row([&hash[..]], |row| row.get(0)).optional()?)
}

/// items のトリガを通らないファイル（版の出力・アンカー。M12）を台帳に載せ、行の ID を返す（呼び手のトランザクションの中で）。
/// 同じ中身の行がすでにあれば、拡張子とサイズが同じときだけその行を使う。食い違えば items のトリガ（`items_blob_mismatch`）と
/// 同じ言葉の Conflict（`files/` の 1 ファイルに 1 行なので、同じ中身を別の名前で載せない）。
pub fn register(conn: &Connection, hash: &[u8; 32], ext: &str, size: u64, now: i64) -> Result<i64> {
    let size = i64::try_from(size)
        .map_err(|_| CoreError::Invalid(format!("ファイルが大きすぎます: {size}")))?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO blobs (hash, ext, size_bytes, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(hash) DO NOTHING",
    )?;
    insert.execute(params![&hash[..], ext, size, now])?;
    let mut stmt = conn.prepare_cached("SELECT id, ext, size_bytes FROM blobs WHERE hash = ?1")?;
    let (id, kept_ext, kept_size): (i64, String, i64) = stmt.query_row([&hash[..]], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    if kept_ext != ext || kept_size != size {
        return Err(CoreError::Conflict(format!(
            "items_blob_mismatch: 台帳の拡張子・サイズと食い違います（{}：台帳は {kept_ext}・{kept_size} バイト、載せるのは {ext}・{size} バイト）",
            hex(hash)
        )));
    }
    Ok(id)
}

/// 台帳にまだない中身を `files/` に置く（消す）前の予告を書く（呼び手が置く前にコミットする）。
pub fn announce(
    conn: &Connection,
    hash: &[u8; 32],
    ext: &str,
    kind: PendingKind,
    job_id: Option<i64>,
    now: i64,
) -> Result<()> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO ledger_pending (hash, ext, kind, job_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    stmt.execute(params![&hash[..], ext, kind.as_str(), job_id, now])?;
    Ok(())
}

/// ジョブ `job_id` の予告を消す（台帳に載せる・台帳から外すのと同じトランザクションで）。消した数を返す。
/// 予告の表は小さい（実行中の保存・GC の分だけ）ので索引を持たず、走査する（docs/data-model.md §4）。
pub fn clear_announced(conn: &Connection, job_id: i64) -> Result<usize> {
    let mut stmt = conn.prepare_cached("DELETE FROM ledger_pending WHERE job_id = ?1")?;
    Ok(stmt.execute([job_id])?)
}

/// 残っている予告を照らして片付ける（ライブラリを開くとき、staging の掃除のあと・ジョブを動かす前に同期で）。片付けた予告の
/// 数を返す。予告の中身が同じ拡張子で台帳にあれば、置き終えて載せた（消すのをやめた）ので予告の行だけを消す。台帳になければ、
/// 置いたが載せる前に落ちた（台帳から外したが消す前に落ちた）ので、そのファイルを消してから予告の行を消す。サムネイルなどの
/// 派生のファイルは、その中身が台帳にどの拡張子でもないときだけ消す（ほかの拡張子で載っている中身の派生は、そのアイテムのもの）。
/// ファイルを消せなかった予告は残し、次に開くときにやり直す。
pub fn settle_pending(db: &Db, layout: &Layout) -> Result<u64> {
    type Pending = (i64, Vec<u8>, String, bool, bool);
    let rows: Vec<Pending> = db.read(|conn| {
        // 予告の表は小さいので走査する（索引なしの例外。docs/data-model.md §4）。台帳は hash の一意の索引で引く
        let mut stmt = conn.prepare_cached(
            "SELECT p.id, p.hash, p.ext,
                    EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash AND b.ext = p.ext),
                    EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash)
               FROM ledger_pending p ORDER BY p.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    })?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut settled = Vec::with_capacity(rows.len());
    for (id, hash, ext, listed, any_ext) in rows {
        if listed {
            settled.push(id);
            continue;
        }
        let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
            tracing::warn!(id, "予告のハッシュが 32 バイトではないので、行だけ消します");
            settled.push(id);
            continue;
        };
        let mut paths = vec![layout.content_path(&hash, &ext)];
        if !any_ext {
            paths.extend(layout.derived_paths(&hex(&hash)));
        }
        let mut removed_all = true;
        for path in paths {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    removed_all = false;
                    tracing::warn!(%err, path = %path.display(), "予告のファイルを消せませんでした（次に開くときにやり直します）");
                }
            }
        }
        if removed_all {
            settled.push(id);
        }
    }
    let n = settled.len();
    db.write(move |tx| {
        let mut stmt = tx.prepare_cached("DELETE FROM ledger_pending WHERE id = ?1")?;
        for id in &settled {
            stmt.execute([id])?;
        }
        Ok(())
    })?;
    Ok(u64::try_from(n).unwrap_or(u64::MAX))
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

    /// 版のファイル・アンカーが指す台帳の行は、items から参照されなくなっても解放しない。3 つの参照はどれも索引で引く。
    #[test]
    fn versions_and_anchors_keep_their_ledger_rows() {
        let conn = test_conn();
        insert(&conn, 1, 10);
        let past = register(&conn, &[7; 32], "png", 7, 1).unwrap();
        let anchor = register(&conn, &[8; 32], "wav", 8, 1).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO item_versions (item_id, version, kind, output_hash, ext, size_bytes, record, file_blob_id,
                                        file_state, replaced_at, created_at)
               VALUES (1, 0, 'original', x'{}', 'png', 7, '{{}}', {past}, 'waiting', 5, 1);
             INSERT INTO edit_anchors (source_hash, options_digest, options, decoded, value_digest, blob_id, created_at)
               VALUES (x'01', x'02', '{{}}', '{{}}', x'03', {anchor}, 1);",
            "07".repeat(32)
        ))
        .unwrap();
        assert_eq!(unreferenced(&conn).unwrap(), 0);
        assert!(release(&conn, &[[7; 32], [8; 32]]).unwrap().is_empty());
        conn.execute_batch("DELETE FROM item_versions; DELETE FROM edit_anchors;")
            .unwrap();
        assert_eq!(unreferenced(&conn).unwrap(), 2);
        let released: Vec<String> = release(&conn, &[[7; 32], [8; 32]])
            .unwrap()
            .into_iter()
            .map(|b| b.ext)
            .collect();
        assert_eq!(released, ["png", "wav"]);
        let plan = query_plan(
            &conn,
            &format!(
                "SELECT count(*) FROM blobs WHERE {}",
                not_referenced("blobs")
            ),
        );
        for index in [
            "sqlite_autoindex_items_1",
            "idx_item_versions_file_blob",
            "idx_edit_anchors_blob",
        ] {
            assert!(plan.contains(index), "{index}: {plan}");
        }
    }

    #[test]
    fn register_reuses_a_matching_row_and_refuses_a_mismatch() {
        let conn = test_conn();
        assert_eq!(id_of(&conn, &[7; 32]).unwrap(), None);
        let id = register(&conn, &[7; 32], "png", 7, 1).unwrap();
        assert_eq!(id_of(&conn, &[7; 32]).unwrap(), Some(id));
        assert_eq!(register(&conn, &[7; 32], "png", 7, 2).unwrap(), id);
        for (ext, size) in [("webp", 7), ("png", 8)] {
            let err = register(&conn, &[7; 32], ext, size, 3).unwrap_err();
            assert!(
                matches!(&err, CoreError::Conflict(m) if m.starts_with("items_blob_mismatch")),
                "{err}"
            );
        }
        // アイテムの今として入っている中身も同じ（トリガで載せた行を使う）
        insert(&conn, 1, 10);
        assert_eq!(
            register(&conn, &[1; 32], "jpg", 10, 4).unwrap(),
            id_of(&conn, &[1; 32]).unwrap().unwrap()
        );
        assert!(register(&conn, &[1; 32], "png", 10, 4).is_err());
        assert_eq!(total_size(&conn).unwrap(), 17);
        for sql in [
            "SELECT id FROM blobs WHERE hash = x'07'",
            "SELECT id, ext, size_bytes FROM blobs WHERE hash = x'07'",
        ] {
            let plan = query_plan(&conn, sql);
            assert!(plan.contains("sqlite_autoindex_blobs_1"), "{sql}: {plan}");
        }
    }

    #[test]
    fn announcements_are_cleared_by_job() {
        let conn = test_conn();
        announce(&conn, &[7; 32], "png", PendingKind::Place, Some(3), 1).unwrap();
        announce(&conn, &[8; 32], "png", PendingKind::Place, Some(3), 1).unwrap();
        announce(&conn, &[9; 32], "png", PendingKind::Delete, Some(4), 1).unwrap();
        assert_eq!(clear_announced(&conn, 3).unwrap(), 2);
        assert_eq!(clear_announced(&conn, 3).unwrap(), 0);
        let left: (String, i64) = conn
            .query_row("SELECT kind, job_id FROM ledger_pending", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(left, ("delete".to_string(), 4));
        // 予告の表は索引を持たない（実行中の保存・GC の分だけの小さな表。docs/data-model.md §4 の例外）
        let plan = query_plan(&conn, "DELETE FROM ledger_pending WHERE job_id = 3");
        assert!(plan.contains("SCAN ledger_pending"), "{plan}");
        let plan = query_plan(
            &conn,
            "SELECT p.id, p.hash, p.ext,
                    EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash AND b.ext = p.ext),
                    EXISTS (SELECT 1 FROM blobs b WHERE b.hash = p.hash)
               FROM ledger_pending p ORDER BY p.id",
        );
        assert!(plan.contains("sqlite_autoindex_blobs_1"), "{plan}");
        assert!(!plan.contains("SCAN b"), "{plan}");
    }

    /// 開くときの照合：台帳にあれば予告だけ消す。台帳になければファイルと（どの拡張子でも台帳にない中身なら）派生のファイルを
    /// 消す。ほかの拡張子で台帳にある中身は、その拡張子のファイルと派生のファイルを残す。
    #[test]
    fn settle_pending_removes_only_files_missing_from_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let lib = crate::library::Library::create(&dir.path().join("L")).unwrap();
        let layout = lib.layout().clone();
        let put = |path: std::path::PathBuf| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, b"x").unwrap();
        };
        let (listed, gone, other) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        lib.db()
            .write(move |tx| {
                insert(tx, 1, 1);
                register(tx, &other, "png", 1, 1)?;
                // 1：置いて台帳に載せた（予告が残った）、2：置いたが載せる前に落ちた、3：webp で置いたが png で台帳にある
                announce(tx, &listed, "jpg", PendingKind::Place, Some(1), 1)?;
                announce(tx, &gone, "png", PendingKind::Place, Some(1), 1)?;
                announce(tx, &other, "webp", PendingKind::Place, Some(1), 1)?;
                Ok(())
            })
            .unwrap();
        for (hash, ext) in [
            (listed, "jpg"),
            (gone, "png"),
            (other, "png"),
            (other, "webp"),
        ] {
            put(layout.content_path(&hash, ext));
            put(layout.thumb_path(&hex(&hash), crate::library::ThumbSize::Small));
        }
        assert_eq!(settle_pending(lib.db(), &layout).unwrap(), 3);
        assert!(layout.content_path(&listed, "jpg").exists());
        assert!(!layout.content_path(&gone, "png").exists());
        assert!(
            !layout
                .thumb_path(&hex(&gone), crate::library::ThumbSize::Small)
                .exists()
        );
        assert!(!layout.content_path(&other, "webp").exists());
        assert!(layout.content_path(&other, "png").exists());
        assert!(
            layout
                .thumb_path(&hex(&other), crate::library::ThumbSize::Small)
                .exists()
        );
        let left: i64 = lib
            .db()
            .read(|c| Ok(c.query_row("SELECT count(*) FROM ledger_pending", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(left, 0);
        assert_eq!(
            crate::library::check_files(lib.db(), &layout).unwrap(),
            (0, 0)
        );
        // 予告がなければ何もしない
        assert_eq!(settle_pending(lib.db(), &layout).unwrap(), 0);
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
