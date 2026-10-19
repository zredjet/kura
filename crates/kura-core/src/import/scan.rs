//! ドロップされたファイル・フォルダを走査し、取り込む候補を決める（読み取りだけで、元のファイルには触らない）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::kind::{MediaKind, Rejected, classify_file};
use crate::text::{natural_cmp, nfc};

/// 中を見ないフォルダの拡張子（ライブラリや他のアプリのパッケージ）。
pub const SKIP_DIR_EXTS: &[&str] = &[
    "kuralib",
    "photoslibrary",
    "app",
    "bundle",
    "framework",
    "fcpbundle",
    "imovielibrary",
    "musiclibrary",
    "tvlibrary",
    "logicx",
    "band",
];

/// ドロップされたもの（ルート）の 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRoot {
    /// 名前（NFC）。
    pub name: String,
    pub is_dir: bool,
}

/// 取り込む候補のファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    /// 元の場所（読むだけ）。
    pub path: PathBuf,
    /// どのルートから見つかったか（`ScanResult::roots` の添字）。
    pub root: usize,
    /// ドロップされたフォルダからこのファイルの親までのフォルダ名（ドロップされたフォルダ自身を含む、NFC）。
    /// ファイルを直接ドロップしたときは空。
    pub dirs: Vec<String>,
    /// ファイル名（NFC）。
    pub name: String,
    /// 小文字の拡張子。
    pub ext: String,
    pub kind: MediaKind,
    pub size: u64,
}

/// 走査の結果。
#[derive(Debug, Clone, Default)]
pub struct ScanResult {
    pub roots: Vec<ScanRoot>,
    /// 取り込む候補（決まった順。再開のときに同じ順になる）。
    pub files: Vec<ScannedFile>,
    /// 対応していない形式・空のファイルの数（`rejected` を含む）。
    pub skipped: u64,
    /// 拡張子は対応しているが中身がその形式でないので取り込まないもの（`.ase` の色見本など）と理由。
    pub rejected: Vec<(PathBuf, String)>,
    /// ドロップされたフォルダの下にあるフォルダの数。
    pub subfolders: u64,
    /// 下にフォルダを持つフォルダ（`(root, dirs)`）。これに含まれないフォルダが「末端」。
    pub parent_dirs: HashSet<(usize, Vec<String>)>,
    /// 読めなかったもの。
    pub errors: Vec<(PathBuf, String)>,
}

/// `paths` を走査する。隠しファイル・パッケージ・シンボリックリンクは対象にしない。
pub fn scan(paths: &[PathBuf]) -> ScanResult {
    let mut result = ScanResult::default();
    for path in paths {
        let root = result.roots.len();
        let name = path
            .file_name()
            .map(|n| nfc(&n.to_string_lossy()))
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(err) => {
                result.errors.push((path.clone(), err.to_string()));
                continue;
            }
        };
        if meta.is_dir() {
            if is_package_name(&name) {
                result.errors.push((
                    path.clone(),
                    "ライブラリやアプリのパッケージは取り込めません".to_string(),
                ));
                continue;
            }
            result.roots.push(ScanRoot {
                name: name.clone(),
                is_dir: true,
            });
            scan_dir(&mut result, root, path, &name);
        } else {
            result.roots.push(ScanRoot {
                name,
                is_dir: false,
            });
            add_file(&mut result, root, path, Vec::new(), meta.len());
        }
    }
    result
}

fn scan_dir(result: &mut ScanResult, root: usize, root_path: &Path, root_name: &str) {
    let walker = walkdir::WalkDir::new(root_path)
        .follow_links(false)
        .sort_by(|a, b| {
            natural_cmp(
                &a.file_name().to_string_lossy(),
                &b.file_name().to_string_lossy(),
            )
        })
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !should_skip(e));
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                let path = err.path().unwrap_or(root_path).to_path_buf();
                result.errors.push((path, err.to_string()));
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let file_type = entry.file_type();
        let chain = parent_chain(root_name, root_path, entry.path());
        if file_type.is_dir() {
            result.subfolders += 1;
            result.parent_dirs.insert((root, chain));
        } else if file_type.is_file() {
            match entry.metadata() {
                Ok(meta) => add_file(result, root, entry.path(), chain, meta.len()),
                Err(err) => result
                    .errors
                    .push((entry.path().to_path_buf(), err.to_string())),
            }
        }
        // シンボリックリンクはたどらない
    }
}

/// ドロップしたフォルダからこのエントリの親までのフォルダ名（NFC）。
fn parent_chain(root_name: &str, root_path: &Path, path: &Path) -> Vec<String> {
    let mut chain = vec![root_name.to_string()];
    if let Some(parent) = path.parent()
        && let Ok(rel) = parent.strip_prefix(root_path)
    {
        chain.extend(
            rel.components()
                .map(|c| nfc(&c.as_os_str().to_string_lossy())),
        );
    }
    chain
}

fn add_file(result: &mut ScanResult, root: usize, path: &Path, dirs: Vec<String>, size: u64) {
    if size == 0 {
        result.skipped += 1;
        return;
    }
    let ext = lowercase_ext(path);
    // 先頭のバイトで見分ける形式（`.ase`）だけ、ここで先頭を読む
    let kind = match classify_file(path, &ext) {
        Ok(kind) => kind,
        Err(Rejected::Unsupported) => {
            result.skipped += 1;
            return;
        }
        Err(rejected @ Rejected::NotThisFormat(_)) => {
            result.skipped += 1;
            result
                .rejected
                .push((path.to_path_buf(), rejected.reason()));
            return;
        }
        Err(rejected @ Rejected::Unreadable(_)) => {
            result.errors.push((path.to_path_buf(), rejected.reason()));
            return;
        }
    };
    result.files.push(ScannedFile {
        path: path.to_path_buf(),
        root,
        dirs,
        name: path
            .file_name()
            .map(|n| nfc(&n.to_string_lossy()))
            .unwrap_or_default(),
        ext,
        kind,
        size,
    });
}

fn should_skip(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    name.starts_with('.') || (entry.file_type().is_dir() && is_package_name(&name))
}

fn is_package_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|ext| SKIP_DIR_EXTS.contains(&ext.as_str()))
}

/// ファイル名から小文字の拡張子を取り出す。
pub fn lowercase_ext(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// テスト用の取り込み元。
    /// ```text
    /// 旅行2026/
    ///   a.jpg  notes.txt  empty.png(0 バイト)  .DS_Store  ._a.jpg
    ///   鎌倉/ img10.jpg img2.JPG clip.mov  .hidden/x.jpg
    ///   Photos Library.photoslibrary/x.jpg
    ///   Old.kuralib/files/x.jpg
    /// single.mp3  readme.md
    /// ```
    fn source_tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let trip = dir.path().join("旅行2026");
        write(&trip.join("a.jpg"), b"aaa");
        write(&trip.join("notes.txt"), b"memo");
        write(&trip.join("empty.png"), b"");
        write(&trip.join(".DS_Store"), b"x");
        write(&trip.join("._a.jpg"), b"x");
        write(&trip.join("鎌倉/img10.jpg"), b"10");
        write(&trip.join("鎌倉/img2.JPG"), b"2");
        write(&trip.join("鎌倉/clip.mov"), b"mov");
        write(&trip.join("鎌倉/.hidden/x.jpg"), b"x");
        write(&trip.join("Photos Library.photoslibrary/x.jpg"), b"x");
        write(&trip.join("Old.kuralib/files/x.jpg"), b"x");
        write(&dir.path().join("single.mp3"), b"mp3");
        write(&dir.path().join("readme.md"), b"md");
        (dir, trip)
    }

    #[test]
    fn finds_supported_files_in_a_stable_natural_order() {
        let (dir, trip) = source_tree();
        let result = scan(&[
            trip.clone(),
            dir.path().join("single.mp3"),
            dir.path().join("readme.md"),
        ]);

        assert_eq!(
            result.roots,
            vec![
                ScanRoot {
                    name: "旅行2026".into(),
                    is_dir: true
                },
                ScanRoot {
                    name: "single.mp3".into(),
                    is_dir: false
                },
                ScanRoot {
                    name: "readme.md".into(),
                    is_dir: false
                },
            ]
        );
        let found: Vec<(usize, Vec<&str>, &str, &str, MediaKind)> = result
            .files
            .iter()
            .map(|f| {
                (
                    f.root,
                    f.dirs.iter().map(String::as_str).collect(),
                    f.name.as_str(),
                    f.ext.as_str(),
                    f.kind,
                )
            })
            .collect();
        assert_eq!(
            found,
            vec![
                (0, vec!["旅行2026"], "a.jpg", "jpg", MediaKind::Image),
                (
                    0,
                    vec!["旅行2026", "鎌倉"],
                    "clip.mov",
                    "mov",
                    MediaKind::Video
                ),
                (
                    0,
                    vec!["旅行2026", "鎌倉"],
                    "img2.JPG",
                    "jpg",
                    MediaKind::Image
                ),
                (
                    0,
                    vec!["旅行2026", "鎌倉"],
                    "img10.jpg",
                    "jpg",
                    MediaKind::Image
                ),
                (1, vec![], "single.mp3", "mp3", MediaKind::Audio),
            ]
        );
        assert_eq!(result.files[0].size, 3);
        assert_eq!(result.files[0].path, trip.join("a.jpg"));
        // notes.txt・empty.png・readme.md
        assert_eq!(result.skipped, 3);
        // 鎌倉だけ（隠しフォルダ・パッケージは数えない）
        assert_eq!(result.subfolders, 1);
        assert!(
            result
                .parent_dirs
                .contains(&(0, vec!["旅行2026".to_string()]))
        );
        assert!(
            !result
                .parent_dirs
                .contains(&(0, vec!["旅行2026".to_string(), "鎌倉".to_string()]))
        );
        assert!(result.errors.is_empty());
    }

    #[test]
    fn same_input_gives_same_order() {
        let (_dir, trip) = source_tree();
        let a = scan(std::slice::from_ref(&trip));
        let b = scan(std::slice::from_ref(&trip));
        assert_eq!(a.files, b.files);
    }

    #[test]
    fn refuses_library_folders_and_reports_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("Mine.kuralib");
        write(&lib.join("files/x.jpg"), b"x");
        let result = scan(&[lib.clone(), dir.path().join("nothing.jpg")]);
        assert!(result.files.is_empty());
        assert_eq!(result.errors.len(), 2);
        assert_eq!(result.errors[0].0, lib);
    }

    #[test]
    fn folder_names_are_normalized_to_nfc() {
        let dir = tempfile::tempdir().unwrap();
        let nfd = "\u{30AB}\u{3099}イド"; // ガイド（分解形）
        write(&dir.path().join("root").join(nfd).join("p.png"), b"p");
        let result = scan(&[dir.path().join("root")]);
        assert_eq!(
            result.files[0].dirs,
            vec!["root".to_string(), "ガイド".to_string()]
        );
    }

    /// フォント・3D も取り込む。`.ase` は Aseprite のものだけ取り込み、Adobe の色見本は理由を付けて対象外にする。
    #[test]
    fn takes_fonts_models_and_only_real_aseprite_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("素材");
        write(&root.join("a.ttf"), b"\x00\x01\x00\x00");
        write(&root.join("b.woff2"), b"wOF2");
        write(&root.join("c.glb"), b"glTF");
        write(&root.join("d.ase"), &[0x80, 0, 0, 0, 0xE0, 0xA5, 1, 0]);
        write(&root.join("e.ase"), b"ASEF\x00\x01\x00\x00");
        write(&root.join("f.aseprite"), b"ASEF\x00\x01\x00\x00");
        write(&root.join("g.svg"), b"<svg/>");
        let result = scan(std::slice::from_ref(&root));
        let found: Vec<(&str, MediaKind)> = result
            .files
            .iter()
            .map(|f| (f.name.as_str(), f.kind))
            .collect();
        assert_eq!(
            found,
            vec![
                ("a.ttf", MediaKind::Font),
                ("b.woff2", MediaKind::Font),
                ("c.glb", MediaKind::Model),
                ("d.ase", MediaKind::Image),
                ("g.svg", MediaKind::Image),
            ]
        );
        assert_eq!(result.skipped, 2);
        let rejected: Vec<(PathBuf, bool)> = result
            .rejected
            .iter()
            .map(|(p, why)| (p.clone(), why.contains("Aseprite")))
            .collect();
        assert_eq!(
            rejected,
            vec![(root.join("e.ase"), true), (root.join("f.aseprite"), true)]
        );
        assert!(result.errors.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        write(&outside.join("o.jpg"), b"o");
        let root = dir.path().join("root");
        write(&root.join("r.jpg"), b"r");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        std::os::unix::fs::symlink(outside.join("o.jpg"), root.join("l.jpg")).unwrap();
        let result = scan(&[root]);
        let names: Vec<&str> = result.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["r.jpg"]);
    }
}
