//! プロジェクトのフォルダを見る（M15 段 2。docs/data-model.md §4「プロジェクトと使った記録（M15）」、ADR-204）：エンジンの
//! 見分けと、照らすファイル（取り込める形式だけ。エンジンのキャッシュと隠しフォルダは見ない。シンボリックリンクはたどらない）。

use std::path::Path;
use std::time::UNIX_EPOCH;

use super::Engine;
use crate::import::classify;

/// ルートの直下で見ないフォルダ（エンジンのキャッシュ・書き出し先。隠しフォルダ `.godot` などは別に見ない）。
pub fn excluded_at_root(engine: Engine) -> &'static [&'static str] {
    match engine {
        Engine::Unity => &[
            "Library",
            "Temp",
            "Logs",
            "obj",
            "Build",
            "Builds",
            "UserSettings",
            "MemoryCaptures",
        ],
        Engine::Godot | Engine::Folder => &[],
    }
}

/// フォルダの中身からエンジンを見分ける：`project.godot` があれば Godot、`ProjectSettings/ProjectVersion.txt` と `Assets/` が
/// あれば Unity、ほかはフォルダ。
pub fn detect(root: &Path) -> Engine {
    if root.join("project.godot").is_file() {
        Engine::Godot
    } else if root.join("ProjectSettings/ProjectVersion.txt").is_file()
        && root.join("Assets").is_dir()
    {
        Engine::Unity
    } else {
        Engine::Folder
    }
}

/// 見つけたファイル 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// ルートからの相対パス（`/` 区切り）。
    pub path: String,
    pub size: u64,
    /// 更新時刻（Unix ナノ秒。1970 年より前・読めなければ 0）。
    pub mtime_ns: i64,
}

/// 見た結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Walked {
    /// 照らすファイル（パスの順）。
    pub files: Vec<Found>,
    /// 読めなかったフォルダ・名前が UTF-8 でないもの（飛ばした数）。
    pub skipped: usize,
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

/// `root` の中の照らすファイルを集める（パスの順）。`stop` が真を返したら途中でやめて None。ルートを読めなければ誤り。
pub fn walk(
    root: &Path,
    engine: Engine,
    stop: &dyn Fn() -> bool,
) -> std::io::Result<Option<Walked>> {
    let mut out = Walked::default();
    // ルートは読めなければ誤り（見つからない・許されていない）。中のフォルダは飛ばして数える
    let first = std::fs::read_dir(root)?;
    let mut stack: Vec<(std::fs::ReadDir, String)> = vec![(first, String::new())];
    let mut visited = 0usize;
    while let Some((mut dir, prefix)) = stack.pop() {
        let Some(entry) = dir.next() else {
            continue;
        };
        stack.push((dir, prefix.clone()));
        visited += 1;
        if visited.is_multiple_of(256) && stop() {
            return Ok(None);
        }
        let Ok(entry) = entry else {
            out.skipped += 1;
            continue;
        };
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            out.skipped += 1;
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            out.skipped += 1;
            continue;
        };
        // シンボリックリンクはたどらない（プロジェクトの外・ループ）
        if kind.is_symlink() {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_dir() {
            if prefix.is_empty() && excluded_at_root(engine).contains(&name.as_str()) {
                continue;
            }
            match std::fs::read_dir(entry.path()) {
                Ok(sub) => stack.push((sub, rel)),
                Err(_) => out.skipped += 1,
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let Some((_, ext)) = name.rsplit_once('.') else {
            continue;
        };
        if classify(ext).is_none() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            out.skipped += 1;
            continue;
        };
        out.files.push(Found {
            path: rel,
            size: meta.len(),
            mtime_ns: mtime_ns(&meta),
        });
    }
    out.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &[u8]) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn detects_godot_unity_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let g = dir.path().join("g");
        write(&g, "project.godot", b"");
        assert_eq!(detect(&g), Engine::Godot);
        let u = dir.path().join("u");
        write(
            &u,
            "ProjectSettings/ProjectVersion.txt",
            b"m_EditorVersion: 6000.0",
        );
        std::fs::create_dir_all(u.join("Assets")).unwrap();
        assert_eq!(detect(&u), Engine::Unity);
        // Assets がなければ Unity とみなさない
        let half = dir.path().join("half");
        write(&half, "ProjectSettings/ProjectVersion.txt", b"");
        assert_eq!(detect(&half), Engine::Folder);
        assert_eq!(detect(dir.path()), Engine::Folder);
    }

    #[test]
    fn walk_keeps_media_and_skips_caches_hidden_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("u");
        write(&root, "ProjectSettings/ProjectVersion.txt", b"");
        write(&root, "Assets/Sprites/hero.png", b"png");
        write(&root, "Assets/Sprites/hero.png.meta", b"meta");
        write(&root, "Assets/Audio/jump.OGG", b"ogg!");
        write(&root, "Assets/Library/kept.png", b"x");
        write(&root, "Assets/Scripts/Hero.cs", b"class");
        write(&root, "Library/cache.png", b"cache");
        write(&root, "Temp/t.png", b"t");
        write(&root, ".git/objects/x.png", b"git");
        write(&root, "Assets/.hidden/h.png", b"h");
        write(&root, "Assets/.dot.png", b"d");
        std::os::unix::fs::symlink(root.join("Assets/Sprites"), root.join("Assets/Link")).unwrap();
        let walked = walk(&root, Engine::Unity, &|| false).unwrap().unwrap();
        let paths: Vec<(&str, u64)> = walked
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.size))
            .collect();
        assert_eq!(
            paths,
            vec![
                ("Assets/Audio/jump.OGG", 4),
                ("Assets/Library/kept.png", 1),
                ("Assets/Sprites/hero.png", 3),
            ]
        );
        assert!(walked.files.iter().all(|f| f.mtime_ns > 0));
        // フォルダとして見れば Library も照らす
        let walked = walk(&root, Engine::Folder, &|| false).unwrap().unwrap();
        assert!(walked.files.iter().any(|f| f.path == "Library/cache.png"));
        // 止めれば None。ルートがなければ誤り
        let many = dir.path().join("many");
        for k in 0..300 {
            write(&many, &format!("{k}.png"), b"p");
        }
        assert_eq!(walk(&many, Engine::Folder, &|| true).unwrap(), None);
        assert!(walk(&dir.path().join("none"), Engine::Folder, &|| false).is_err());
    }
}
