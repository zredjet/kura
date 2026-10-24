//! 整合性チェック（起動時と、設定の「いま確かめる」）。staging の残骸を消し、台帳とファイルの食い違いを数える（自動では
//! 消さない）。items の台帳の行の ID の写し（`blob_id`。0010）だけは、いま確かめるときに食い違えば直す（検索の正しさに
//! 関わる派生の値。items を全件走査するので起動ごとには走らせない。.claude/rules/database.md）。

use serde::Serialize;

use super::layout::Layout;
use super::ledger;
use crate::Result;
use crate::db::Db;
use crate::edit::gc;

/// 整合性チェックの結果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrityReport {
    /// 消した staging の残骸の数。
    pub staging_removed: u64,
    /// DB にあってファイルがないアイテムの数。
    pub missing_files: u64,
    /// ファイルがあって DB にないものの数。
    pub orphan_files: u64,
    /// どの表からも参照されない台帳の行の数（ゴミ箱を空にする途中で落ちたなど）。
    pub unreferenced_blobs: u64,
    /// 台帳の行の ID の写し（`items.blob_id`）が hash の台帳の行と食い違っていて、直したアイテムの数（いま確かめるときだけ
    /// 数える。ふつうは 0。M12）。
    pub blob_ids_fixed: u64,
    /// GC の照合が合わずにファイルを残した版（kept）の数（M12 の段 3。問題には数えない。版の表を走査するので、いま確かめる
    /// ときだけ数え、起動時は None）。
    pub kept_versions: Option<u64>,
    /// 7 日とバックアップの条件を満たしたのに、まだ消していない消し待ちの版の数（GC がまだ来ていない・今は照らせないもの。
    /// M12 の段 3。問題には数えない。起動時は GC のジョブの前なので数えず None）。
    pub overdue_versions: Option<u64>,
}

/// どこまで確かめるか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// 起動時：台帳と `files/` の突き合わせと、参照されない台帳の行。
    Startup,
    /// 設定の「いま確かめる」：起動時の分に加えて、`items.blob_id` の写しを確かめて直す（items を全件走査する）。
    Full,
}

/// 整合性チェック：台帳と `files/` の突き合わせ（[`check_files`]）、参照されない台帳の行、`Full` なら `items.blob_id` の
/// 写しの修理（[`repair_blob_ids`]）と、GC が残した版・7 日を過ぎた消し待ちの版の数（問題には数えない）。`staging_removed` は先に消した staging の残骸の数（起動時。いま確かめるときは 0）。
pub fn check(
    db: &Db,
    layout: &Layout,
    staging_removed: u64,
    depth: Depth,
) -> Result<IntegrityReport> {
    let blob_ids_fixed = match depth {
        Depth::Startup => 0,
        Depth::Full => repair_blob_ids(db)?,
    };
    let (kept_versions, overdue_versions) = match depth {
        Depth::Startup => (None, None),
        Depth::Full => {
            let cutoff = gc::GcClock::system().cutoff(layout)?;
            db.read(|c| Ok((Some(gc::kept(c)?), Some(gc::overdue(c, cutoff)?))))?
        }
    };
    let (missing_files, orphan_files) = check_files(db, layout)?;
    Ok(IntegrityReport {
        staging_removed,
        missing_files,
        orphan_files,
        unreferenced_blobs: db.read(ledger::unreferenced)?,
        blob_ids_fixed,
        kept_versions,
        overdue_versions,
    })
}

/// `items.blob_id` が hash の台帳の行の ID と食い違う（NULL を含む）アイテム。
const BLOB_ID_MISMATCH: &str =
    "SELECT i.id FROM items i LEFT JOIN blobs b ON b.id = i.blob_id WHERE b.hash IS NOT i.hash";

/// `items.blob_id`（台帳の行の ID の写し。0010 のトリガが入れる）が hash の台帳の行と食い違うアイテムを直し、直した数を返す。
/// トリガが写すので食い違わないはずだが、事実の検索とタイルの事実はこの列で引くので、いま確かめるときに直す（items を 1 回
/// 走査して台帳を整数の主キーで引く。10 万件で約 10ms。食い違いがあるときだけ書く。全件の走査なので起動ごとには呼ばない）。
pub fn repair_blob_ids(db: &Db) -> Result<u64> {
    let mismatched: i64 = db.read(|conn| {
        Ok(conn.query_row(
            &format!("SELECT count(*) FROM ({BLOB_ID_MISMATCH})"),
            [],
            |r| r.get(0),
        )?)
    })?;
    if mismatched == 0 {
        return Ok(0);
    }
    let fixed = db.write(|tx| {
        Ok(tx.execute(
            &format!(
                "UPDATE items SET blob_id = (SELECT b.id FROM blobs b WHERE b.hash = items.hash)
                  WHERE id IN ({BLOB_ID_MISMATCH})"
            ),
            [],
        )?)
    })?;
    tracing::warn!(fixed, "items.blob_id が台帳と食い違っていたので直しました");
    Ok(fixed as u64)
}

/// `staging/` の中身を消す（取り込み・ダウンロードの書きかけ）。ジョブを動かす前に呼ぶ。
/// `keep` の名前のもの（待機中・一時停止中のダウンロードの作業フォルダ）は続きに使うので残す。
pub fn clean_staging(layout: &Layout, keep: &[String]) -> Result<u64> {
    let dir = layout.staging_dir();
    let mut removed = 0;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| keep.iter().any(|k| n == k.as_str()))
        {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        removed += 1;
    }
    Ok(removed)
}

/// 台帳 `blobs` と `files/` を突き合わせ、(ファイルがない台帳の行の数, 台帳にないファイルの数) を返す。
/// どの表からも参照されない台帳の行は `ledger::unreferenced` で数える。
pub fn check_files(db: &Db, layout: &Layout) -> Result<(u64, u64)> {
    let mut expected = db.read(ledger::expected_paths)?;

    let files_dir = layout.files_dir();
    let mut orphans = 0;
    for entry in walkdir::WalkDir::new(&files_dir).min_depth(3).max_depth(3) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&files_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !expected.remove(&rel) {
            orphans += 1;
        }
    }
    Ok((expected.len() as u64, orphans))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;

    fn insert_item(lib: &Library, hash: [u8; 32]) {
        lib.db()
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO items (hash, ext, kind, size_bytes, title, original_name, imported_at, updated_at)
                     VALUES (?1, 'jpg', 'image', 1, 't', 't.jpg', 0, 0)",
                    [hash.to_vec()],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn put_file(lib: &Library, hash: &[u8; 32]) {
        let path = lib.layout().content_path(hash, "jpg");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "x").unwrap();
    }

    /// いま確かめるとき（`Depth::Full`）だけ、台帳の行の ID の写し（`items.blob_id`）の食い違いを直して数える。起動時は直さない。
    #[test]
    fn full_check_repairs_blob_ids_and_startup_leaves_them() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        insert_item(&lib, [1u8; 32]);
        insert_item(&lib, [2u8; 32]);
        put_file(&lib, &[1u8; 32]);
        put_file(&lib, &[2u8; 32]);
        let broken = |lib: &Library| -> i64 {
            lib.db()
                .read(|c| {
                    Ok(c.query_row(
                        &format!("SELECT count(*) FROM ({BLOB_ID_MISMATCH})"),
                        [],
                        |r| r.get(0),
                    )?)
                })
                .unwrap()
        };
        assert_eq!(broken(&lib), 0);
        // トリガを通らない食い違い（NULL と、ほかの行の ID）を作る
        lib.db()
            .write(|tx| {
                tx.execute_batch(
                    "UPDATE items SET blob_id = NULL WHERE id = 1;
                     UPDATE items SET blob_id = (SELECT id FROM blobs WHERE hash = (SELECT hash FROM items WHERE id = 1))
                      WHERE id = 2;",
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(broken(&lib), 2);
        let startup = check(lib.db(), lib.layout(), 3, Depth::Startup).unwrap();
        assert_eq!((startup.staging_removed, startup.blob_ids_fixed), (3, 0));
        // GC が残した版・7 日を過ぎた消し待ちは、起動時には数えない
        assert_eq!(
            (startup.kept_versions, startup.overdue_versions),
            (None, None)
        );
        assert_eq!(broken(&lib), 2);
        let full = check(lib.db(), lib.layout(), 0, Depth::Full).unwrap();
        assert_eq!(
            full,
            IntegrityReport {
                blob_ids_fixed: 2,
                kept_versions: Some(0),
                overdue_versions: Some(0),
                ..IntegrityReport::default()
            }
        );
        assert_eq!(broken(&lib), 0);
        assert_eq!(repair_blob_ids(lib.db()).unwrap(), 0);
        // 全件の走査（.claude/rules/database.md の例外：利用者が「いま確かめる」を押したときだけ走る）
        let plan = lib
            .db()
            .read(|c| Ok(crate::db::query_plan(c, BLOB_ID_MISMATCH)))
            .unwrap();
        assert!(plan.contains("SCAN i"), "{plan}");
        // kept の数え上げも版の表を走査する（同じ例外。7 日を過ぎた消し待ちは部分索引で引く）
        let plan = lib
            .db()
            .read(|c| {
                Ok(crate::db::query_plan(
                    c,
                    "SELECT count(*) FROM item_versions WHERE file_state = 'kept'",
                ))
            })
            .unwrap();
        assert!(plan.contains("SCAN item_versions"), "{plan}");
    }

    /// いま確かめるとき、GC の照合が合わずに残した版（kept）と、7 日を過ぎた消し待ちの版を数える（問題には数えない）。
    #[test]
    fn full_check_counts_kept_and_overdue_versions() {
        use crate::edit::testing::{insert_item, insert_version};
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        lib.db()
            .write(|tx| {
                let a = insert_item(tx, 1, [1; 32]);
                insert_version(tx, a, 0, [10; 32], "hold", None);
                insert_version(tx, a, 1, [11; 32], "kept", None);
                insert_version(tx, a, 2, [12; 32], "waiting", None);
                insert_version(tx, a, 3, [13; 32], "waiting", None);
                insert_version(tx, a, 4, [1; 32], "hold", None);
                // 版 3 は置き換えたばかり（7 日たっていない）
                tx.execute(
                    "UPDATE item_versions SET replaced_at = ?1 WHERE version = 3",
                    [crate::time::now_ms()],
                )?;
                Ok(())
            })
            .unwrap();
        for hash in [[1u8; 32], [10; 32], [11; 32], [12; 32], [13; 32]] {
            let path = lib.layout().content_path(&hash, "png");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        let full = check(lib.db(), lib.layout(), 0, Depth::Full).unwrap();
        assert_eq!(
            (full.kept_versions, full.overdue_versions),
            (Some(1), Some(1))
        );
        assert_eq!(
            (
                full.missing_files,
                full.orphan_files,
                full.unreferenced_blobs
            ),
            (0, 0, 0)
        );
    }

    #[test]
    fn removes_staging_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let staging = lib.layout().staging_dir();
        std::fs::write(staging.join("import-1-0.jpg"), "x").unwrap();
        std::fs::create_dir(staging.join("dl-2")).unwrap();
        std::fs::write(staging.join("dl-2").join("a.part"), "x").unwrap();

        std::fs::create_dir(staging.join("dl-3")).unwrap();
        std::fs::write(staging.join("dl-3").join("b.part"), "x").unwrap();

        assert_eq!(clean_staging(lib.layout(), &["dl-3".into()]).unwrap(), 2);
        let left: Vec<_> = std::fs::read_dir(&staging)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from("dl-3")]);
    }

    #[test]
    fn counts_missing_and_orphan_files() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let ok = [1u8; 32];
        let missing = [2u8; 32];
        let orphan = [3u8; 32];
        insert_item(&lib, ok);
        put_file(&lib, &ok);
        insert_item(&lib, missing);
        put_file(&lib, &orphan);

        assert_eq!(check_files(lib.db(), lib.layout()).unwrap(), (1, 1));
    }

    #[test]
    fn integrity_counts_from_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(&dir.path().join("L")).unwrap();
        let (ok, unreferenced, orphan) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        for hash in [ok, unreferenced] {
            insert_item(&lib, hash);
            put_file(&lib, &hash);
        }
        put_file(&lib, &orphan);
        // アイテムの行だけが消え、台帳の行は残った（ゴミ箱を空にする途中で落ちたのと同じ形）。
        // 台帳にある間はファイルがあるはずなので「DB にないファイル」には数えず、参照されない行として数える
        lib.db()
            .write(move |tx| {
                tx.execute("DELETE FROM items WHERE hash = ?1", [&unreferenced[..]])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(check_files(lib.db(), lib.layout()).unwrap(), (0, 1));
        assert_eq!(lib.db().read(ledger::unreferenced).unwrap(), 1);
        // 台帳の行があってファイルがないもの
        std::fs::remove_file(lib.layout().content_path(&ok, "jpg")).unwrap();
        assert_eq!(check_files(lib.db(), lib.layout()).unwrap(), (1, 1));
    }
}
