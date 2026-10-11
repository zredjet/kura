//! 起動時の整合性チェック。staging の残骸を消し、DB とファイルの食い違いを数える（自動では消さない）。

use std::collections::HashSet;

use serde::Serialize;

use super::layout::{Layout, content_rel_path, hex};
use crate::Result;
use crate::db::Db;

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

/// DB と `files/` を突き合わせ、(ファイルがないアイテム数, DB にないファイル数) を返す。
pub fn check_files(db: &Db, layout: &Layout) -> Result<(u64, u64)> {
    // すべてのアイテムを確かめるので全件を読む（意図した全件スキャン）
    let mut expected: HashSet<String> = db.read(|conn| {
        let mut stmt = conn.prepare_cached("SELECT hash, ext FROM items")?;
        let rows = stmt.query_map([], |row| {
            let hash: Vec<u8> = row.get(0)?;
            let ext: String = row.get(1)?;
            Ok(content_rel_path(&hex(&hash), &ext))
        })?;
        Ok(rows.collect::<std::result::Result<HashSet<_>, _>>()?)
    })?;

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
}
