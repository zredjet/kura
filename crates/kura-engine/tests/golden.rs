//! ゴールデン（docs/editing.md §8.5）。操作の版ごとに、例の引数・入力の作り方・出力のダイジェストを凍結する。
//!
//! - ファイルは `goldens/<操作>/v<版>.jsonl`。1 行に 1 つ、正規の JSON で
//!   `{"digest","inputs","name","params","target"}`。`target` は整数だけの操作なら `*`、浮動小数を使う操作は `aarch64-macos` など。
//! - **追記だけ**。既存の行を書き換えてはいけない（CI の scripts/check-goldens-append-only.sh でも確かめる）。
//! - 例の引数と入力の作り方も凍結するので、実装を消して移行した版（E7）や旧版（E6）の行も、
//!   今の登録表で解決（そのまま／移行）して描き、凍結した値と照らす。解決できない版のファイルは失敗にする。
//! - 足りない行は `KURA_BLESS=1 cargo test -p kura-engine --test golden` で足せる（既存の行は変えない）。
//! - 浮動小数の操作の行は、主なターゲット（aarch64-macos）にだけ必須。ほかのターゲットでは、あれば照らす。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use kura_engine::NoMonitor;
use kura_engine::digest::canonical;
use kura_engine::op::Numeric;
use kura_engine::render::Engine;
use kura_engine::testgen::InputGen;
use kura_engine::testing::{render_with, single_node_graph, source_of};
use serde::{Deserialize, Serialize};

const PRIMARY: &str = "aarch64-macos";

fn target() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    name: String,
    target: String,
    params: serde_json::Value,
    inputs: Vec<InputGen>,
    digest: String,
}

/// 凍結した (操作, 版) → 行。
fn read_all() -> BTreeMap<(String, u16), Vec<Line>> {
    let mut out = BTreeMap::new();
    let Ok(ops) = std::fs::read_dir(root()) else {
        return out;
    };
    for op_dir in ops {
        let op_dir = op_dir.unwrap().path();
        assert!(
            op_dir.is_dir(),
            "goldens/ の直下は操作ごとのフォルダだけ: {}",
            op_dir.display()
        );
        let op = op_dir.file_name().unwrap().to_string_lossy().to_string();
        for file in std::fs::read_dir(&op_dir).unwrap() {
            let path = file.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let v: u16 = name
                .strip_prefix('v')
                .and_then(|s| s.strip_suffix(".jsonl"))
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| {
                    panic!("ゴールデンのファイル名は v<版>.jsonl: {}", path.display())
                });
            out.insert((op.clone(), v), read_lines(&path));
        }
    }
    out
}

fn read_lines(path: &Path) -> Vec<Line> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("{}: 読めない行: {e}\n{l}", path.display()))
        })
        .collect()
}

/// 引数・入力を正規の JSON で比べる（`-6.0` は正規の形では `-6` になり、読み直すと整数になるため）。
fn same<T: Serialize>(a: &T, b: &T) -> bool {
    canonical(a).unwrap() == canonical(b).unwrap()
}

fn line_text(line: &Line) -> String {
    String::from_utf8(canonical(line).unwrap()).unwrap()
}

fn digest_of(
    engine: &Engine,
    op: &str,
    v: u16,
    params: &serde_json::Value,
    inputs: &[InputGen],
) -> Result<String, String> {
    let first = inputs.first().ok_or("入力がありません")?;
    let graph = single_node_graph(op, v, params.clone()).map_err(|e| e.to_string())?;
    let value = first.generate().map_err(|e| e.to_string())?;
    let source = source_of(inputs).map_err(|e| e.to_string())?;
    let out = render_with(engine, &graph, value, source, &NoMonitor).map_err(|e| e.to_string())?;
    Ok(out.digest().map_err(|e| e.to_string())?.to_hex())
}

#[test]
fn goldens_match_and_are_complete() {
    let engine = Engine::new().unwrap();
    // テストだけが bless を環境変数で受け付ける（エンジンの結果は環境に依存しない）
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let here = target();
    let frozen = read_all();
    let mut failures = String::new();

    // 1. 凍結した行をすべて、今の登録表で解決して描き、照らす（旧版・移行した版を含む）
    for ((op, v), lines) in &frozen {
        let mut seen: BTreeMap<(&str, &str), ()> = BTreeMap::new();
        let mut by_name: BTreeMap<&str, &Line> = BTreeMap::new();
        for line in lines {
            if seen
                .insert((line.name.as_str(), line.target.as_str()), ())
                .is_some()
            {
                let _ = writeln!(
                    failures,
                    "{op} v{v} {}：同じ例・ターゲットの行が 2 つあります",
                    line.name
                );
            }
            match by_name.get(line.name.as_str()) {
                Some(first)
                    if !same(&first.params, &line.params) || !same(&first.inputs, &line.inputs) =>
                {
                    let _ = writeln!(
                        failures,
                        "{op} v{v} {}：同じ例の行で引数か入力が違います",
                        line.name
                    );
                }
                Some(_) => {}
                None => {
                    by_name.insert(line.name.as_str(), line);
                }
            }
            if line.target != "*" && line.target != here {
                continue;
            }
            match digest_of(&engine, op, *v, &line.params, &line.inputs) {
                Ok(d) if d == line.digest => {}
                Ok(d) => {
                    let _ = writeln!(
                        failures,
                        "{op} v{v} {}（{}）：凍結した値 {} と違う結果 {d}。結果を変えるなら操作の版を上げて旧版を legacy に残す（docs/editing.md §8.4）",
                        line.name, line.target, line.digest
                    );
                }
                Err(e) => {
                    let _ = writeln!(
                        failures,
                        "{op} v{v} {}：今の登録表で描けません（版を消した？移行がない？）: {e}",
                        line.name
                    );
                }
            }
        }
        // 実装のある版なら、数値の申告と行のターゲットが合っていること
        if let Some(imp) = engine
            .registry()
            .all()
            .into_iter()
            .find(|i| i.id() == op && i.version() == *v)
        {
            let exact = imp.signature().numeric == Numeric::Exact;
            if lines.iter().any(|l| (l.target == "*") != exact) {
                let _ = writeln!(
                    failures,
                    "{op} v{v}：数値の申告（{:?}）と行のターゲットが合いません",
                    imp.signature().numeric
                );
            }
        }
    }

    // 2. 実装のある版の例は、すべて凍結されていること（引数・入力が変わっていないこと）
    for imp in engine.registry().all() {
        let (op, v) = (imp.id(), imp.version());
        let lines = frozen
            .get(&(op.to_string(), v))
            .cloned()
            .unwrap_or_default();
        let exact = imp.signature().numeric == Numeric::Exact;
        let want = if exact { "*".to_string() } else { here.clone() };
        let mut to_append = Vec::new();
        for ex in imp.examples().unwrap() {
            let same_name: Vec<&Line> = lines.iter().filter(|l| l.name == ex.name).collect();
            if same_name
                .iter()
                .any(|l| !same(&l.params, &ex.params) || !same(&l.inputs, &ex.inputs))
            {
                let _ = writeln!(
                    failures,
                    "{op} v{v} {}：凍結した例と引数か入力が違います（例は変えずに、新しい名前の例を足す）",
                    ex.name
                );
                continue;
            }
            if same_name.iter().any(|l| l.target == want) || (!exact && here != PRIMARY) {
                continue;
            }
            let digest = digest_of(&engine, op, v, &ex.params, &ex.inputs)
                .unwrap_or_else(|e| panic!("{op} {}: {e}", ex.name));
            to_append.push(Line {
                name: ex.name.to_string(),
                target: want.clone(),
                params: ex.params,
                inputs: ex.inputs,
                digest,
            });
        }
        if to_append.is_empty() {
            continue;
        }
        let path = root().join(op).join(format!("v{v}.jsonl"));
        let text: String = to_append.iter().map(|l| line_text(l) + "\n").collect();
        // ほかに食い違いがあるときは追記しない（食い違いを、追記で上書きしたように見せない）
        if bless && failures.is_empty() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut all = std::fs::read_to_string(&path).unwrap_or_default();
            all.push_str(&text);
            std::fs::write(&path, all).unwrap();
        } else {
            let _ = write!(
                failures,
                "{}：行が足りません（KURA_BLESS=1 で追記できます）:\n{text}",
                path.display()
            );
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}
