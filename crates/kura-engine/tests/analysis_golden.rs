//! 解析のゴールデン（docs/editing.md §11・§8.5）。解析の版ごとに、例の引数・入力の作り方・結果を凍結する。
//!
//! - ファイルは `goldens-analysis/<解析 ID>/v<版>.jsonl`。1 行に 1 つ、正規の JSON で
//!   `{"digest","input","name","output"?,"params","target"}`。
//!   - `digest`：結果の正規の形のダイジェスト（`analysis::output_digest`。ドメイン `kura-analysis-out-v1`）。照らすのはこれ。
//!   - `output`：結果が正規の JSON で 2 KiB 以下のときだけ、読むために結果そのもの（あれば一致も照らす）。
//!   - `params`：正規の引数（既定値を補ったもの）。`input`：入力の作り方 1 つ。
//!   - `target`：整数だけの解析（`numeric_for` が `Exact`）は `*`、浮動小数を使う解析は `aarch64-macos` など。
//! - **追記だけ**（scripts/check-goldens-append-only.sh も見る）。例（名前・引数・入力）は凍結し、変えずに新しい名前で足す。
//! - 実装のある版（今の版・旧版）の行はすべて照らす。実装を消した表示だけの解析の版（`dropped`）のファイルは照らさない。
//!   実装も `dropped` もない版のファイルは失敗。
//! - 足りない行は `KURA_BLESS=1 cargo test -p kura-engine --test analysis_golden` で足せる（すべて照らし終えて食い違いが
//!   ないときだけ。凍結する前に、値を直接確かめるテストで正しさを確かめておく）。
//! - 浮動小数の解析の行は、主なターゲット（aarch64-macos）にだけ必須。ほかのターゲットでは、あれば照らす。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use kura_engine::NoMonitor;
use kura_engine::analysis::{AnalysisRequest, Analyzed, Encoding, output_digest};
use kura_engine::digest::canonical;
use kura_engine::op::Numeric;
use kura_engine::render::Engine;
use kura_engine::testgen::InputGen;
use serde::{Deserialize, Serialize};

const PRIMARY: &str = "aarch64-macos";
/// 結果をそのまま行に書く大きさの上限（バイト）。
const INLINE_MAX: usize = 2048;

fn target() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens-analysis")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    name: String,
    target: String,
    params: serde_json::Value,
    input: InputGen,
    digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<serde_json::Value>,
}

/// 凍結した (解析, 版) → 行。
fn read_all() -> BTreeMap<(String, u16), Vec<Line>> {
    let mut out = BTreeMap::new();
    let Ok(dirs) = std::fs::read_dir(root()) else {
        return out;
    };
    for dir in dirs {
        let dir = dir.unwrap().path();
        assert!(
            dir.is_dir(),
            "goldens-analysis/ の直下は解析ごとのフォルダだけ: {}",
            dir.display()
        );
        let id = dir.file_name().unwrap().to_string_lossy().to_string();
        for file in std::fs::read_dir(&dir).unwrap() {
            let path = file.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let v: u16 = name
                .strip_prefix('v')
                .and_then(|s| s.strip_suffix(".jsonl"))
                .and_then(|s| s.parse().ok())
                .filter(|v: &u16| format!("v{v}.jsonl") == name)
                .unwrap_or_else(|| {
                    panic!("ゴールデンのファイル名は v<版>.jsonl: {}", path.display())
                });
            assert!(
                out.insert((id.clone(), v), read_lines(&path)).is_none(),
                "同じ版のゴールデンのファイルが 2 つあります: {}",
                path.display()
            );
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

fn same<T: Serialize>(a: &T, b: &T) -> bool {
    canonical(a).unwrap() == canonical(b).unwrap()
}

fn analyze(
    engine: &Engine,
    id: &str,
    v: u16,
    params: &serde_json::Value,
    input: &InputGen,
) -> Result<Analyzed, String> {
    let value = input.generate().map_err(|e| e.to_string())?;
    engine
        .analyze_dyn(
            &AnalysisRequest {
                id: id.to_string(),
                v: Some(v),
                params: params.clone(),
            },
            &value,
            &NoMonitor,
        )
        .map_err(|e| e.to_string())
}

/// 読むために行に書く結果（正規の JSON で小さいときだけ）。
fn inline(out: &Analyzed) -> Option<serde_json::Value> {
    (out.encoding == Encoding::Json && out.output.len() <= INLINE_MAX)
        .then(|| serde_json::from_slice(&out.output).unwrap())
}

#[test]
fn analysis_goldens_match_and_are_complete() {
    let engine = Engine::new().unwrap();
    // テストだけが bless を環境変数で受け付ける（エンジンの結果は環境に依存しない）
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let here = target();
    let frozen = read_all();
    let analyses = engine.analyses();
    let mut failures = String::new();

    // 1. 凍結した行をすべて照らす（今の版・旧版）。実装を消した表示だけの版は照らさない
    for ((id, v), lines) in &frozen {
        let Some(imp) = analyses.get(id, *v) else {
            if !analyses.is_dropped(id, *v) {
                let _ = writeln!(
                    failures,
                    "{id} v{v}：実装も dropped もない版のゴールデンです（操作が申告する解析の旧版は legacy に残す）"
                );
            }
            continue;
        };
        let names: Vec<&str> = imp.examples().unwrap().iter().map(|e| e.name).collect();
        let mut seen: BTreeMap<(&str, &str), ()> = BTreeMap::new();
        let mut by_name: BTreeMap<&str, &Line> = BTreeMap::new();
        for line in lines {
            if seen
                .insert((line.name.as_str(), line.target.as_str()), ())
                .is_some()
            {
                let _ = writeln!(
                    failures,
                    "{id} v{v} {}：同じ例・ターゲットの行が 2 つあります",
                    line.name
                );
            }
            match by_name.get(line.name.as_str()) {
                Some(first)
                    if !same(&first.params, &line.params) || !same(&first.input, &line.input) =>
                {
                    let _ = writeln!(
                        failures,
                        "{id} v{v} {}：同じ例の行で引数か入力が違います",
                        line.name
                    );
                }
                Some(_) => {}
                None => {
                    by_name.insert(line.name.as_str(), line);
                }
            }
            if !names.contains(&line.name.as_str()) {
                let _ = writeln!(
                    failures,
                    "{id} v{v} {}：凍結した例が実装から消えています（例は消さない）",
                    line.name
                );
            }
            let numeric = imp.numeric(&line.params).unwrap();
            if (line.target == "*") != (numeric == Numeric::Exact) {
                let _ = writeln!(
                    failures,
                    "{id} v{v} {}：数値の申告（{numeric:?}）と行のターゲットが合いません",
                    line.name
                );
            }
            if line.target != "*" && line.target != here {
                continue;
            }
            match analyze(&engine, id, *v, &line.params, &line.input) {
                Ok(out) => {
                    let d = out.output_digest().to_hex();
                    if d != line.digest {
                        let _ = writeln!(
                            failures,
                            "{id} v{v} {}（{}）：凍結した値 {} と違う結果 {d}。結果を変えるなら解析の版を上げる（操作が申告する解析は旧版を legacy に残す。docs/editing.md §11）",
                            line.name, line.target, line.digest
                        );
                    }
                    if let Some(want) = &line.output
                        && canonical(want).unwrap() != out.output
                    {
                        let _ = writeln!(
                            failures,
                            "{id} v{v} {}：行の output と結果が違います",
                            line.name
                        );
                    }
                }
                Err(e) => {
                    let _ = writeln!(failures, "{id} v{v} {}：解けません: {e}", line.name);
                }
            }
        }
    }

    // 2. 実装のある版の例は、すべて凍結されていること（引数・入力が変わっていないこと）
    let mut appends: Vec<(PathBuf, String)> = Vec::new();
    for imp in analyses.all() {
        let (id, v) = (imp.id(), imp.version());
        let lines = frozen
            .get(&(id.to_string(), v))
            .cloned()
            .unwrap_or_default();
        let mut text = String::new();
        for ex in imp.examples().unwrap() {
            let exact = imp.numeric(&ex.params).unwrap() == Numeric::Exact;
            let want = if exact { "*".to_string() } else { here.clone() };
            let same_name: Vec<&Line> = lines.iter().filter(|l| l.name == ex.name).collect();
            if same_name
                .iter()
                .any(|l| !same(&l.params, &ex.params) || !same(&l.input, &ex.input))
            {
                let _ = writeln!(
                    failures,
                    "{id} v{v} {}：凍結した例と引数か入力が違います（例は変えずに、新しい名前の例を足す）",
                    ex.name
                );
                continue;
            }
            if same_name.iter().any(|l| l.target == want) || (!exact && here != PRIMARY) {
                continue;
            }
            let out = analyze(&engine, id, v, &ex.params, &ex.input)
                .unwrap_or_else(|e| panic!("{id} {}: {e}", ex.name));
            let line = Line {
                name: ex.name.to_string(),
                target: want,
                params: ex.params,
                input: ex.input,
                digest: output_digest(&out.output).to_hex(),
                output: inline(&out),
            };
            text.push_str(&String::from_utf8(canonical(&line).unwrap()).unwrap());
            text.push('\n');
        }
        if !text.is_empty() {
            appends.push((root().join(id).join(format!("v{v}.jsonl")), text));
        }
    }
    // ほかに食い違いがあるときは、どのファイルにも追記しない
    if bless && failures.is_empty() {
        for (path, text) in &appends {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut all = std::fs::read_to_string(path).unwrap_or_default();
            all.push_str(text);
            std::fs::write(path, all).unwrap();
        }
    } else {
        for (path, text) in &appends {
            let _ = write!(
                failures,
                "{}：行が足りません（KURA_BLESS=1 で追記できます）:\n{text}",
                path.display()
            );
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}
