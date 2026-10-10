//! ライブラリを置く場所の確認（拡張子の補完、既存のライブラリか、同期フォルダの中か）。

use std::path::{Component, Path, PathBuf};

use super::layout::DB_FILE;

/// ライブラリのフォルダの拡張子。
pub const LIBRARY_EXT: &str = "kuralib";

/// 場所の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationStatus {
    /// まだ何もない（新しく作れる）。
    New,
    /// すでにライブラリがある（開ける）。
    ExistingLibrary,
    /// ライブラリではないファイルやフォルダがある（作れない）。
    Occupied,
}

/// 場所を確認した結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationCheck {
    /// 拡張子を補った場所。
    pub path: PathBuf,
    pub status: LocationStatus,
    /// 同期フォルダ（iCloud Drive など）の中ならそのサービス名。SQLite の WAL と相性が悪いので警告する。
    pub sync_service: Option<String>,
}

/// 拡張子 `.kuralib` が付いていなければ付ける。
pub fn normalize_library_path(path: &Path) -> PathBuf {
    let has_ext = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(LIBRARY_EXT));
    if has_ext {
        path.to_path_buf()
    } else {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".");
        name.push(LIBRARY_EXT);
        path.with_file_name(name)
    }
}

/// フォルダがライブラリか（DB ファイルがあるか）。
pub fn is_library(path: &Path) -> bool {
    path.join(DB_FILE).is_file()
}

/// 場所を確認する。`home` は同期フォルダの判定に使う。
pub fn check_location(path: &Path, home: Option<&Path>) -> LocationCheck {
    let path = normalize_library_path(path);
    let status = if is_library(&path) {
        LocationStatus::ExistingLibrary
    } else if path.exists() && !is_empty_dir(&path) {
        LocationStatus::Occupied
    } else {
        LocationStatus::New
    };
    let resolved = resolve_existing_prefix(&path);
    let sync_service = home.and_then(|home| {
        let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
        sync_service_for(&resolved, &home)
    });
    LocationCheck {
        path,
        status,
        sync_service,
    }
}

/// 同期フォルダの中ならサービス名を返す（iCloud Drive と `~/Library/CloudStorage` の各サービス）。
pub fn sync_service_for(path: &Path, home: &Path) -> Option<String> {
    let rest = path.strip_prefix(home.join("Library")).ok()?;
    let mut parts = rest.components().filter_map(|c| match c {
        Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
        _ => None,
    });
    match parts.next()?.as_str() {
        "Mobile Documents" => Some("iCloud Drive".to_string()),
        "CloudStorage" => {
            let provider = parts.next()?;
            // 「GoogleDrive-name@example.com」→「GoogleDrive」
            let name = provider.split('-').next().unwrap_or(&provider);
            Some(name.to_string())
        }
        _ => None,
    }
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path)
        .map(|mut entries| {
            entries.all(|e| e.map(|e| e.file_name() == ".DS_Store").unwrap_or(false))
        })
        .unwrap_or(false)
}

/// まだない場所でも判定できるよう、存在する親までをシンボリックリンクを解決して実際の場所にする。
fn resolve_existing_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
    let mut resolved = existing.canonicalize().unwrap_or(existing);
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_extension_when_missing() {
        assert_eq!(
            normalize_library_path(Path::new("/a/Kura Library")),
            PathBuf::from("/a/Kura Library.kuralib")
        );
        assert_eq!(
            normalize_library_path(Path::new("/a/My.KURALIB")),
            PathBuf::from("/a/My.KURALIB")
        );
    }

    #[test]
    fn detects_sync_folders() {
        let home = Path::new("/Users/me");
        assert_eq!(
            sync_service_for(
                Path::new("/Users/me/Library/Mobile Documents/com~apple~CloudDocs/K.kuralib"),
                home
            ),
            Some("iCloud Drive".into())
        );
        assert_eq!(
            sync_service_for(
                Path::new("/Users/me/Library/CloudStorage/GoogleDrive-me@example.com/K.kuralib"),
                home
            ),
            Some("GoogleDrive".into())
        );
        assert_eq!(
            sync_service_for(Path::new("/Users/me/Pictures/K.kuralib"), home),
            None
        );
    }

    #[test]
    fn classifies_locations() {
        let dir = tempfile::tempdir().unwrap();
        let new = check_location(&dir.path().join("New"), None);
        assert_eq!(new.status, LocationStatus::New);
        assert_eq!(new.path, dir.path().join("New.kuralib"));

        let occupied = dir.path().join("Busy.kuralib");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("note.txt"), "x").unwrap();
        assert_eq!(
            check_location(&occupied, None).status,
            LocationStatus::Occupied
        );

        let library = dir.path().join("Lib.kuralib");
        std::fs::create_dir(&library).unwrap();
        std::fs::write(library.join(DB_FILE), "").unwrap();
        assert_eq!(
            check_location(&library, None).status,
            LocationStatus::ExistingLibrary
        );

        let empty = dir.path().join("Empty.kuralib");
        std::fs::create_dir(&empty).unwrap();
        std::fs::write(empty.join(".DS_Store"), "").unwrap();
        assert_eq!(check_location(&empty, None).status, LocationStatus::New);
    }
}
