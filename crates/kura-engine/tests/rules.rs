//! 規則の走査（docs/editing.md §8.2・§12、NFR-16）。clippy.toml で禁じられないもの（テストが使う口）を、src のソースを読んで確かめる。
//!
//! - エンジンはスレッドプールを作らない：`ThreadPoolBuilder`・`.install(`・`::install`（UFCS の `ThreadPool::install(&pool, ..)`
//!   など）・`build_global` は src（テストのモジュールを除く）にない。
//!   プールは呼び手（kura-core の EngineRuntime）が作り、エンジンは呼ばれたスレッドのプールで `par_blocks` を回す。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// src に書いてはいけないもの（コメント・文字列・テストのモジュールを除いたコードで探す）。`::install` は UFCS の
/// `rayon::ThreadPool::install(&pool, ..)`・`<ThreadPool>::install`・関数として取り出す `ThreadPool::install`（clippy.toml の
/// disallowed-methods はテストが使うので入れていない）。
const POOL_TOKENS: &[&str] = &[
    "ThreadPoolBuilder",
    ".install(",
    "::install",
    "build_global",
];

/// ソースから、コメント・文字列・文字の中身を空白にしたもの（行は保つ）。
fn strip_comments_and_strings(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && next == Some('*') {
            i += 2;
            while i < b.len() && !(b[i] == '*' && b.get(i + 1) == Some(&'/')) {
                if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == 'r' && matches!(next, Some('"' | '#')) && !out.chars().last().is_some_and(ident) {
            let mut j = i + 1;
            let mut hashes = 0;
            while b.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if b.get(j) == Some(&'"') {
                j += 1;
                while j < b.len()
                    && !(b[j] == '"' && (0..hashes).all(|h| b.get(j + 1 + h) == Some(&'#')))
                {
                    if b[j] == '\n' {
                        out.push('\n');
                    }
                    j += 1;
                }
                out.push(' ');
                i = j + 1 + hashes;
                continue;
            }
        }
        if c == '"' {
            i += 1;
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' {
                    i += 1;
                } else if b[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            out.push(' ');
            i += 1;
            continue;
        }
        if c == '\'' {
            // 文字（'x'・'\n'・'\u{…}'）。ライフタイム（'a）はそのまま
            if next == Some('\\') {
                let mut j = i + 3;
                while j < b.len() && b[j] != '\'' {
                    j += 1;
                }
                out.push(' ');
                i = j + 1;
                continue;
            }
            if b.get(i + 2) == Some(&'\'') {
                out.push(' ');
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn skip_ws(b: &[char], mut i: usize) -> usize {
    while b.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

fn word_at(b: &[char], i: usize) -> (usize, String) {
    let w: String = b
        .iter()
        .skip(i)
        .take_while(|c| c.is_ascii_alphanumeric() || **c == '_')
        .collect();
    (i + w.chars().count(), w)
}

/// `b[i]` から始まる属性 `#[…]` の終わり（`]` の次）と、中身（空白を除く）。
fn attribute_at(b: &[char], i: usize) -> Option<(usize, String)> {
    if b.get(i) != Some(&'#') {
        return None;
    }
    let open = skip_ws(b, i + 1);
    if b.get(open) != Some(&'[') {
        return None;
    }
    let mut depth = 0usize;
    for (j, c) in b.iter().enumerate().skip(open) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let body = b[open + 1..j].iter().filter(|c| !c.is_whitespace());
                    return Some((j + 1, body.collect()));
                }
            }
            _ => {}
        }
    }
    None
}

/// テストのモジュール：`#[cfg(test)]`・続く属性・可視性のあとの `mod <名前>` が、中身つき（`{ … }`）なら
/// `Inline(終わりの位置)`、別のファイル（`;`）なら `File(名前, 終わりの位置)`。
enum TestModule {
    Inline(usize),
    File(String, usize),
}

fn test_module_at(b: &[char], i: usize) -> Option<TestModule> {
    let (mut j, body) = attribute_at(b, i)?;
    if body != "cfg(test)" {
        return None;
    }
    while let Some((end, _)) = attribute_at(b, skip_ws(b, j)) {
        j = end;
    }
    let (mut j, mut w) = word_at(b, skip_ws(b, j));
    if w == "pub" {
        j = skip_ws(b, j);
        if b.get(j) == Some(&'(') {
            j += b[j..].iter().position(|c| *c == ')')? + 1;
        }
        (j, w) = word_at(b, skip_ws(b, j));
    }
    if w != "mod" {
        return None;
    }
    let (j, name) = word_at(b, skip_ws(b, j));
    let open = skip_ws(b, j);
    if name.is_empty() {
        return None;
    }
    match b.get(open) {
        Some(';') => Some(TestModule::File(name, open + 1)),
        Some('{') => {
            let mut depth = 0usize;
            for (k, c) in b.iter().enumerate().skip(open) {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(TestModule::Inline(k + 1));
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

/// コメント・文字列とテストのモジュール（中身つき）を除いたコードと、別のファイルのテストのモジュールの名前。
fn code_of(src: &str) -> (String, Vec<String>) {
    let stripped = strip_comments_and_strings(src);
    let b: Vec<char> = stripped.chars().collect();
    let mut out = String::with_capacity(stripped.len());
    let mut files = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match test_module_at(&b, i) {
            Some(TestModule::Inline(end)) => {
                out.extend(b[i..end].iter().filter(|c| **c == '\n'));
                i = end;
            }
            Some(TestModule::File(name, end)) => {
                files.push(name);
                i = end;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    (out, files)
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// `file` で宣言した子のモジュール `name` のファイル（`name.rs` か `name/mod.rs`）。
fn child_module_files(file: &Path, name: &str) -> Vec<PathBuf> {
    let parent = file.parent().unwrap();
    let stem = file.file_stem().unwrap().to_string_lossy();
    let base = if stem == "mod" || stem == "lib" || stem == "main" {
        parent.to_path_buf()
    } else {
        parent.join(stem.as_ref())
    };
    vec![
        base.join(format!("{name}.rs")),
        base.join(name).join("mod.rs"),
    ]
}

/// src の中で、テストのモジュールの外に `tokens` が出てくる場所（ファイル: 行: 字句）。別のファイルに置いたテストのモジュール
/// （`#[cfg(test)] mod x;`）は、そのファイルごと除く。
fn find_outside_tests(src: &Path, tokens: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    rust_files(src, &mut files);
    let mut codes = Vec::new();
    let mut test_files = BTreeSet::new();
    for f in &files {
        let (code, children) = code_of(&std::fs::read_to_string(f).unwrap());
        for c in children {
            test_files.extend(child_module_files(f, &c));
        }
        codes.push((f.clone(), code));
    }
    let mut found = Vec::new();
    for (f, code) in codes {
        // テストのモジュールのファイルの子も、テストのモジュール
        if f.ancestors().any(|a| {
            test_files.contains(a)
                || test_files.contains(&a.join("mod.rs"))
                || test_files.contains(&a.with_extension("rs"))
        }) {
            continue;
        }
        for (n, line) in code.lines().enumerate() {
            for t in tokens {
                if line.contains(t) {
                    found.push(format!(
                        "{}:{}: {t}",
                        f.strip_prefix(src).unwrap_or(&f).display(),
                        n + 1
                    ));
                }
            }
        }
    }
    found
}

/// NFR-16：エンジンはスレッドプールを作らない（src のテストのモジュールの外に、プールを作る・入るコードがない）。
#[test]
fn engine_src_creates_no_pools() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = find_outside_tests(&src, POOL_TOKENS);
    assert!(
        found.is_empty(),
        "エンジンの src がスレッドプールを作っています（呼び手の EngineRuntime に任せる）:\n{}",
        found.join("\n")
    );
    // 走査が空回りしていない：テストのモジュールの中には実際にある
    let all = {
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        files.iter().any(|f| {
            std::fs::read_to_string(f)
                .unwrap()
                .contains("ThreadPoolBuilder")
        })
    };
    assert!(all);
}

/// 走査は、コード・テストでない関数の中のものを見つけ、コメント・文字列・テストのモジュール（中身つき・別のファイル）は除く。
#[test]
fn pool_scan_sees_code_and_skips_tests() {
    let found = |src: &str| {
        let (code, _) = code_of(src);
        POOL_TOKENS.iter().any(|t| code.contains(t))
    };
    assert!(found("fn f() { rayon::ThreadPoolBuilder::new(); }"));
    assert!(found("fn f(p: &Pool) { p.install(|| 1); }"));
    // UFCS・型を書いた呼び出し・関数として取り出すもの
    assert!(found(
        "fn f(p: &rayon::ThreadPool) { rayon::ThreadPool::install(p, || 1); }"
    ));
    assert!(found(
        "fn f(p: &ThreadPool) { <ThreadPool>::install(p, || 1); }"
    ));
    assert!(found("fn f() { let g = ThreadPool::install; }"));
    assert!(!found("fn installer() { let installed = 1; }"));
    assert!(found(
        "#[cfg(test)]\nfn probe() { rayon::ThreadPoolBuilder::new().build_global(); }"
    ));
    assert!(!found(
        "// rayon::ThreadPoolBuilder\n/* .install( */ fn f() {}"
    ));
    assert!(!found(
        r#"fn f() { let s = "ThreadPoolBuilder"; let r = r"x.install(y"; }"#
    ));
    assert!(!found(
        "#[cfg(test)]\n#[allow(clippy::panic)]\nmod tests {\n    fn f() { let s = \"}\"; rayon::ThreadPoolBuilder::new(); }\n}"
    ));
    let (_, files) =
        code_of("#[cfg(test)]\npub(crate) mod oracle;\n#[cfg(test)]\nmod stream_tests;\nmod real;");
    assert_eq!(
        files,
        vec!["oracle".to_string(), "stream_tests".to_string()]
    );
    assert_eq!(
        child_module_files(Path::new("/a/codec/vorbis/mod.rs"), "x"),
        vec![
            PathBuf::from("/a/codec/vorbis/x.rs"),
            PathBuf::from("/a/codec/vorbis/x/mod.rs")
        ]
    );
    assert_eq!(
        child_module_files(Path::new("/a/codec/ogg.rs"), "x")[0],
        PathBuf::from("/a/codec/ogg/x.rs")
    );
}
