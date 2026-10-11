//! 取り込み前の確認（種類別件数・合計サイズ・重複の見込み・フォルダの概要）。

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use rayon::prelude::*;

use super::kind::MediaKind;
use super::place::partial_hash;
use super::scan::{ScanRoot, ScannedFile, scan};
use super::store::hashes_by_size;
use crate::Result;
use crate::library::Library;

/// 取り込みシートに出す内容。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPreview {
    /// 取り込む候補の数。
    pub total: u64,
    pub images: u64,
    pub videos: u64,
    pub audios: u64,
    pub total_bytes: u64,
    /// 内容が同一のものがすでにある（または同じ取り込みの中で重なる）見込みの数。
    pub duplicates: u64,
    /// 対応していない形式・空のファイルの数。
    pub skipped: u64,
    pub roots: Vec<ScanRoot>,
    pub subfolders: u64,
    /// 「フォルダ構成の再現」の説明に使う例（なるべく 2 階層以上のもの）。
    pub sample_dirs: Vec<String>,
    /// 読めなかったものの数。
    pub errors: u64,
}

/// `paths` を走査して取り込み前の確認をする。
pub fn preview(library: &Library, paths: &[PathBuf]) -> Result<ImportPreview> {
    let scan = scan(paths);
    let mut preview = ImportPreview {
        total: scan.files.len() as u64,
        skipped: scan.skipped,
        roots: scan.roots.clone(),
        subfolders: scan.subfolders,
        errors: scan.errors.len() as u64,
        ..ImportPreview::default()
    };
    for file in &scan.files {
        match file.kind {
            MediaKind::Image => preview.images += 1,
            MediaKind::Video => preview.videos += 1,
            MediaKind::Audio => preview.audios += 1,
        }
        preview.total_bytes += file.size;
    }
    preview.sample_dirs = scan
        .files
        .iter()
        .find(|f| f.dirs.len() >= 2)
        .or_else(|| scan.files.iter().find(|f| !f.dirs.is_empty()))
        .map(|f| f.dirs.clone())
        .unwrap_or_default();

    let layout = library.layout();
    preview.duplicates = library.db().read(|conn| {
        estimate_duplicates(&scan.files, |size| {
            Ok(hashes_by_size(conn, size)?
                .into_iter()
                .map(|(hash, ext)| layout.content_path(&hash, &ext))
                .collect())
        })
    })?;
    Ok(preview)
}

/// 重複の見込みを数える。`existing(size)` はそのサイズの既存アイテムの実ファイルを返す。
/// サイズと先頭・末尾 64KB のハッシュが一致すれば重複とみなす（同じ取り込みの中の重なりも数える）。
pub fn estimate_duplicates(
    files: &[ScannedFile],
    existing: impl Fn(u64) -> Result<Vec<PathBuf>>,
) -> Result<u64> {
    let mut by_size: BTreeMap<u64, Vec<&ScannedFile>> = BTreeMap::new();
    for file in files {
        by_size.entry(file.size).or_default().push(file);
    }
    let mut duplicates = 0;
    for (size, group) in by_size {
        let existing_paths = existing(size)?;
        if existing_paths.is_empty() && group.len() < 2 {
            continue;
        }
        let known: HashSet<[u8; 32]> = existing_paths
            .par_iter()
            .filter_map(|p| partial_hash(p).ok())
            .collect();
        let candidates: Vec<Option<[u8; 32]>> = group
            .par_iter()
            .map(|f| partial_hash(&f.path).ok())
            .collect();
        let mut seen = HashSet::new();
        for hash in candidates.into_iter().flatten() {
            if known.contains(&hash) || !seen.insert(hash) {
                duplicates += 1;
            }
        }
    }
    Ok(duplicates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::place::hash_file;
    use crate::import::store::{NewItem, insert_item};

    fn write(path: &std::path::Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// ライブラリに中身 `bytes` のアイテムを直接入れる。
    fn add_existing(library: &Library, bytes: &'static [u8]) {
        let hash = *blake3::hash(bytes).as_bytes();
        let path = library.layout().content_path(&hash, "jpg");
        write(&path, bytes);
        assert_eq!(hash_file(&path).unwrap(), hash);
        library
            .db()
            .write(move |tx| {
                insert_item(
                    tx,
                    &NewItem {
                        hash: &hash,
                        ext: "jpg",
                        kind: MediaKind::Image,
                        mime: None,
                        size: bytes.len() as u64,
                        title: "x",
                        original_name: "x.jpg",
                        original_path: "/x.jpg",
                        info: None,
                        source_site: "local",
                        source_url: None,
                        captured_at: None,
                        now: 0,
                    },
                )
            })
            .unwrap();
    }

    #[test]
    fn counts_kinds_sizes_and_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::create(&dir.path().join("L")).unwrap();
        add_existing(&library, b"already in library");

        let src = dir.path().join("旅行2026");
        write(&src.join("dup-of-library.jpg"), b"already in library");
        write(&src.join("鎌倉/a.png"), b"aaaa");
        write(&src.join("鎌倉/a-copy.png"), b"aaaa"); // 取り込みの中で重なる
        write(&src.join("鎌倉/b.png"), b"bbbb"); // サイズは同じだが中身が違う
        write(&src.join("鎌倉/clip.mov"), b"movie!");
        write(&src.join("song.flac"), b"flac");
        write(&src.join("notes.txt"), b"skip");

        let p = preview(&library, &[src]).unwrap();
        assert_eq!(p.total, 6);
        assert_eq!((p.images, p.videos, p.audios), (4, 1, 1));
        assert_eq!(p.total_bytes, 18 + 4 + 4 + 4 + 6 + 4);
        assert_eq!(p.duplicates, 2);
        assert_eq!(p.skipped, 1);
        assert_eq!(p.subfolders, 1);
        assert_eq!(p.roots.len(), 1);
        assert_eq!(
            p.sample_dirs,
            vec!["旅行2026".to_string(), "鎌倉".to_string()]
        );
    }

    #[test]
    fn unique_sizes_need_no_hashing() {
        // 既存アイテムの読み出しが呼ばれても、候補が 1 件だけで既存もなければ重複にならない
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.jpg");
        write(&src, b"unique");
        let files = crate::import::scan::scan(&[src]).files;
        let dups = estimate_duplicates(&files, |_| Ok(vec![])).unwrap();
        assert_eq!(dups, 0);
    }
}
