//! 作り直しの見本集（docs/editing.md §8.6 の 1、§17 の E1・E2）。複数のノードの文書を、保存した JSON から読み直して描き、凍結した値と照らす。
//!
//! - ファイルは `goldens-replay/replay.jsonl`。1 行に `{"digest","doc","inputs","name"}`（doc は保存した文書の JSON）。
//! - **追記だけ**（scripts/check-goldens-append-only.sh が見る）。値が変わったら、どの操作・規則が結果を変えたかを調べ、
//!   操作の版か全体の規則の版（`RULES`）を上げる（§8.4）。
//! - 凍結した行の `doc` が正本。凍結したときのスキーマのまま読み直す（スキーマの移行を通る）。`examples()` は、
//!   凍結した文書を今のスキーマへ移したものと一致すること、まだ凍結していない見本を足すことだけに使う。
//! - 足りない行は `KURA_BLESS=1 cargo test -p kura-engine --test replay` で足せる。浮動小数を含む見本は主なターゲット（aarch64-macos）の値。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use kura_engine::NoMonitor;
use kura_engine::digest::canonical;
use kura_engine::doc::Graph;
use kura_engine::render::Engine;
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::testing::{render_with, source_of};
use kura_engine::value::{LoopRange, PixelFormat};
use serde::{Deserialize, Serialize};
use serde_json::json;

const PRIMARY: &str = "aarch64-macos";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    name: String,
    /// 浮動小数を含む見本は `aarch64-macos` など、整数だけなら `*`。
    target: String,
    doc: serde_json::Value,
    inputs: Vec<InputGen>,
    digest: String,
}

fn chain(steps: &[(&str, u16, serde_json::Value, u64, bool)]) -> Graph {
    let mut g = Graph::new_single();
    for (op, v, params, seed, enabled) in steps {
        let id = g.push_linear(op, *v, params.clone(), *seed).unwrap();
        if !enabled {
            g.nodes.iter_mut().find(|n| n.id == id).unwrap().enabled = false;
        }
    }
    g
}

/// 見本：（名前、整数だけか、文書、入力）。見本は消さない・変えない（新しい名前で足す）。
fn examples() -> Vec<(&'static str, bool, Graph, Vec<InputGen>)> {
    let sheet = InputGen::Sheet {
        cols: 6,
        rows: 4,
        cell: 16,
    };
    vec![
        (
            "crop-flip-crop",
            true,
            chain(&[
                (
                    "image.crop",
                    1,
                    json!({ "rect": { "x": 8, "y": 8, "w": 80, "h": 48 } }),
                    0,
                    true,
                ),
                ("image.flip", 1, json!({ "axis": "horizontal" }), 0, true),
                (
                    "image.crop",
                    1,
                    json!({ "rect": { "x": 16, "y": 0, "w": 32, "h": 32 } }),
                    0,
                    true,
                ),
            ]),
            vec![sheet.clone()],
        ),
        (
            "disabled-middle",
            true,
            chain(&[
                (
                    "image.crop",
                    1,
                    json!({ "rect": { "x": 1, "y": 2, "w": 30, "h": 20 } }),
                    0,
                    true,
                ),
                ("image.flip", 1, json!({ "axis": "vertical" }), 0, false),
                ("image.flip", 1, json!({ "axis": "horizontal" }), 0, true),
            ]),
            vec![InputGen::Noise {
                w: 40,
                h: 30,
                fmt: PixelFormat::Indexed8,
                seed: 3,
            }],
        ),
        (
            "frames-chain",
            true,
            chain(&[
                (
                    "image.crop",
                    1,
                    json!({ "rect": { "x": 2, "y": 2, "w": 12, "h": 10 } }),
                    0,
                    true,
                ),
                ("image.flip", 1, json!({ "axis": "vertical" }), 0, true),
            ]),
            vec![InputGen::NoiseFrames {
                w: 16,
                h: 16,
                fmt: PixelFormat::Rgba16,
                count: 4,
                seed: 5,
            }],
        ),
        (
            "meta-through-chain",
            true,
            chain(&[
                (
                    "image.crop",
                    1,
                    json!({ "rect": { "x": 16, "y": 16, "w": 48, "h": 32 } }),
                    0,
                    true,
                ),
                ("image.flip", 1, json!({ "axis": "horizontal" }), 0, true),
            ]),
            vec![InputGen::WithMeta {
                base: Box::new(sheet),
                meta: MetaGen {
                    dpi: Some((96, 96)),
                    text: vec![("Title".into(), "シート".into())],
                    icc: Some(vec![1, 2, 3]),
                    ..MetaGen::default()
                },
            }],
        ),
        (
            "gain-chain",
            false,
            chain(&[
                ("audio.gain", 1, json!({ "db": -6.0 }), 0, true),
                ("audio.gain", 1, json!({ "db": 3.5 }), 0, true),
                ("audio.gain", 1, json!({ "db": 0.0 }), 0, true),
                ("audio.gain", 1, json!({ "db": 12.0 }), 0, false),
            ]),
            vec![InputGen::AudioNoise {
                rate: 44_100,
                channels: 2,
                frames: 3_000,
                seed: 7,
            }],
        ),
        (
            "loop-gain",
            false,
            chain(&[("audio.gain", 1, json!({ "db": -1.25 }), 0, true)]),
            vec![InputGen::WithMeta {
                base: Box::new(InputGen::Sine {
                    rate: 22_050,
                    channels: 1,
                    frames: 2_000,
                    freq: 330,
                    loop_range: Some(LoopRange {
                        start: 10,
                        end: 1_900,
                    }),
                }),
                meta: MetaGen {
                    markers: vec![(5, "a".into())],
                    tags: vec![("title".into(), "t".into())],
                    ..MetaGen::default()
                },
            }],
        ),
        // スキーマ 2（規則の版 `rules` を持つ文書）で凍結する見本。上の見本はスキーマ 1 のまま凍結してあり、移行の経路を通る
        (
            "schema2-crop-flip",
            true,
            chain(&[
                (
                    "image.crop",
                    1,
                    json!({ "rect": { "x": 3, "y": 5, "w": 21, "h": 13 } }),
                    0,
                    true,
                ),
                ("image.flip", 1, json!({ "axis": "vertical" }), 0, true),
            ]),
            vec![InputGen::Noise {
                w: 32,
                h: 24,
                fmt: PixelFormat::Rgba8,
                seed: 11,
            }],
        ),
        // スキーマ 3（出力の量子化 `quantize` を持つ文書）で凍結する見本。段 6 の音声の操作を通す
        (
            "schema3-audio-chain-quantize",
            false,
            {
                let mut g = chain(&[
                    (
                        "audio.trim",
                        1,
                        json!({ "start": 300, "end": 9_000 }),
                        0,
                        true,
                    ),
                    (
                        "audio.fade",
                        1,
                        json!({ "fadeIn": 500, "fadeOut": 800, "curve": "equalPower" }),
                        0,
                        true,
                    ),
                    ("audio.resample", 1, json!({ "rate": 48_000 }), 0, true),
                    (
                        "audio.normalize",
                        1,
                        json!({ "target": -18.0, "peakLimit": -1.0 }),
                        0,
                        true,
                    ),
                ]);
                g.outputs[0].quantize = Some(kura_engine::doc::Quantize {
                    v: 1,
                    bits: 16,
                    dither: kura_engine::doc::Dither::Tpdf,
                    seed: 42,
                });
                g
            },
            vec![InputGen::WithMeta {
                base: Box::new(InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 2,
                    frames: 10_000,
                    seed: 13,
                }),
                meta: MetaGen {
                    loop_range: Some(LoopRange {
                        start: 1_000,
                        end: 8_000,
                    }),
                    ..MetaGen::default()
                },
            }],
        ),
    ]
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens-replay")
}

fn path() -> PathBuf {
    dir().join("replay.jsonl")
}

fn same<T: Serialize>(a: &T, b: &T) -> bool {
    canonical(a).unwrap() == canonical(b).unwrap()
}

fn render_saved(engine: &Engine, doc: &serde_json::Value, inputs: &[InputGen]) -> String {
    // 保存した JSON から読み直して描く（作り直しと同じ経路）
    let g = engine.load(&serde_json::to_vec(doc).unwrap()).unwrap();
    let out = render_with(
        engine,
        &g,
        inputs[0].generate().unwrap(),
        source_of(inputs).unwrap(),
        &NoMonitor,
    )
    .unwrap();
    out.digest().unwrap().to_hex()
}

#[test]
fn saved_documents_replay_to_the_frozen_values() {
    #[allow(clippy::disallowed_methods)]
    let bless = std::env::var("KURA_BLESS").is_ok_and(|v| v == "1");
    let here = format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS);
    let engine = Engine::new().unwrap();
    let frozen: Vec<Line> = std::fs::read_to_string(path())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut failures = String::new();
    // 見本のファイルは 1 つだけ（別のファイルに分けて、照らされない行を作らない）
    if let Ok(entries) = std::fs::read_dir(dir()) {
        for e in entries {
            let name = e.unwrap().file_name().to_string_lossy().to_string();
            if name != "replay.jsonl" {
                let _ = writeln!(
                    failures,
                    "goldens-replay/{name}：replay.jsonl のほかのファイルは置けません"
                );
            }
        }
    }
    // 凍結した見本をすべて作り直す
    let mut seen: BTreeMap<(String, String), ()> = BTreeMap::new();
    for line in &frozen {
        if seen
            .insert((line.name.clone(), line.target.clone()), ())
            .is_some()
        {
            let _ = writeln!(failures, "{}：同じ見本の行が 2 つあります", line.name);
        }
        if line.target != "*" && line.target != here {
            continue;
        }
        let d = render_saved(&engine, &line.doc, &line.inputs);
        if d != line.digest {
            let _ = writeln!(
                failures,
                "{}（{}）：凍結した値 {} と違う結果 {d}",
                line.name, line.target, line.digest
            );
        }
    }
    // 見本はすべて凍結されていること
    let mut append = String::new();
    let names: Vec<&str> = examples().iter().map(|e| e.0).collect();
    for l in &frozen {
        if !names.contains(&l.name.as_str()) {
            let _ = writeln!(failures, "{}：凍結した見本が消えています", l.name);
        }
    }
    for (name, exact, graph, inputs) in examples() {
        let doc: serde_json::Value =
            serde_json::from_slice(&graph.to_canonical_json().unwrap()).unwrap();
        let target = if exact { "*".to_string() } else { here.clone() };
        let mine: Vec<&Line> = frozen.iter().filter(|l| l.name == name).collect();
        // 凍結した文書（古いスキーマのこともある）を今のスキーマへ移すと、見本の文書になること
        let differs = mine.iter().any(|l| {
            let loaded = engine.load(&serde_json::to_vec(&l.doc).unwrap());
            !matches!(&loaded, Ok(g) if same(g, &graph)) || !same(&l.inputs, &inputs)
        });
        if differs {
            let _ = writeln!(
                failures,
                "{name}：凍結した見本と文書か入力が違います（見本は変えずに新しい名前で足す。スキーマの移行が文書を変えていないかも見る）"
            );
            continue;
        }
        if mine.iter().any(|l| l.target == target) || (!exact && here != PRIMARY) {
            continue;
        }
        let digest = render_saved(&engine, &doc, &inputs);
        let line = Line {
            name: name.into(),
            target,
            doc,
            inputs,
            digest,
        };
        append.push_str(&(String::from_utf8(canonical(&line).unwrap()).unwrap() + "\n"));
    }
    if !append.is_empty() {
        if bless && failures.is_empty() {
            std::fs::create_dir_all(path().parent().unwrap()).unwrap();
            let mut all = std::fs::read_to_string(path()).unwrap_or_default();
            all.push_str(&append);
            std::fs::write(path(), all).unwrap();
        } else {
            let _ = write!(
                failures,
                "行が足りません（KURA_BLESS=1 で追記できます）:\n{append}"
            );
        }
    }
    assert!(failures.is_empty(), "\n{failures}");
}
