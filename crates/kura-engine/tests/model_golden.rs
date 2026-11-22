//! 3D モデル（glb）の描画のゴールデン（M11。`kura_engine::model`）。描いた値のダイジェストと、読んだ情報を凍結する。
//!
//! - ファイルは `goldens-model/<描画の約束の版の / を @ にしたもの>.jsonl`（`model-kura-render@1.jsonl`）。1 行に
//!   `{"info","name","samples","size","value","view"}`。`name` は見本（`model::sample_glbs`）の名前と描き方、`value` は描いた
//!   値のダイジェスト、`info` は `read_info` の結果。
//! - 描画は決定性の規則に従う（超越関数は `crate::math`、並列は `par_blocks`）ので、どの環境でも同じ値になる。
//! - **追記だけ**（scripts/check-goldens-append-only.sh が見る）。描く値が変わったら、描画の約束の版（`model::RENDERER`）を
//!   上げて新しいファイルにする。見本（名前と作り方）は消さない・変えない（新しい名前で足す）。
//! - 足りない行は `KURA_BLESS=1 cargo test -p kura-engine --test model_golden` で足せる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use kura_engine::NoMonitor;
use kura_engine::model::{
    ModelLimits, RENDERER, RenderOptions, ViewDir, load_scene, read_info, render, sample_glbs,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    name: String,
    view: String,
    size: u32,
    samples: u32,
    info: serde_json::Value,
    value: String,
}

fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("goldens-model")
        .join(format!("{}.jsonl", RENDERER.replace('/', "@")))
}

/// 見本 × 描き方の行を作る。
fn lines() -> Vec<Line> {
    let limits = ModelLimits::default();
    let mut out = Vec::new();
    for (name, bytes) in sample_glbs().unwrap() {
        let info = serde_json::to_value(read_info(&bytes, &limits).unwrap()).unwrap();
        let scene = load_scene(&bytes, &limits, &NoMonitor).unwrap();
        for (view, view_name, size, samples) in [
            (ViewDir::ThreeQuarter, "threeQuarter", 96, 3),
            (ViewDir::Front, "front", 64, 2),
        ] {
            let opts = RenderOptions {
                size,
                samples,
                view,
            };
            let v = render(&scene, &opts, &limits, &NoMonitor).unwrap();
            out.push(Line {
                name: format!("{name}-{view_name}-{size}x{samples}"),
                view: view_name.into(),
                size,
                samples,
                info: info.clone(),
                value: v.digest().unwrap().to_hex(),
            });
        }
    }
    out
}

#[test]
fn model_renders_match_goldens() {
    let path = path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let frozen: BTreeMap<String, Line> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let line: Line = serde_json::from_str(l).unwrap();
            (line.name.clone(), line)
        })
        .collect();
    let now = lines();
    let names: Vec<&str> = now.iter().map(|l| l.name.as_str()).collect();
    for name in frozen.keys() {
        assert!(
            names.contains(&name.as_str()),
            "ゴールデンの行 {name} の見本がありません（見本は消さない）"
        );
    }
    let mut missing = String::new();
    for line in &now {
        match frozen.get(&line.name) {
            Some(f) => assert_eq!(f, line, "{}：描いた値か情報が変わりました", line.name),
            None => {
                writeln!(missing, "{}", serde_json::to_string(line).unwrap()).unwrap();
            }
        }
    }
    if missing.is_empty() {
        return;
    }
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    assert!(
        bless,
        "{} に足りない行があります（KURA_BLESS=1 で足す）：\n{missing}",
        path.display()
    );
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut all = text;
    if !all.is_empty() && !all.ends_with('\n') {
        all.push('\n');
    }
    all.push_str(&missing);
    std::fs::write(&path, all).unwrap();
}
