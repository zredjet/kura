//! メモリの見積もりの煙のテスト（docs/editing.md §12・NFR-17。段 10）。固定の小さな入力で、読む・描く・書くの通しを
//! 子プロセスで回し、`/usr/bin/time -l` の peak memory footprint から基準（同じ子で、ファイルに触れる前に終えるもの）を引いた量が、
//! エンジンの見積もり（`Engine::estimate`）× 1.25 ＋ 4 MiB 以下であることを確かめる。読む段だけの量も、読む段の見積もりと
//! 照らす。コーデック・操作の作業領域の倍率が実装の変更で黙って足りなくなるのを、macOS の CI で捕まえる（倍率そのものの
//! 実測と大きな入力は bench edit）。
//!
//! 子プロセスは、このテストのバイナリ自身を `--exact mem_smoke::tests::child --ignored` で起こす（環境変数でケースと段を
//! 渡す）。`/usr/bin/time -l` がない・peak memory footprint を出さない環境（Linux）では確かめずに通す（CI の Linux の job は
//! kura-devtools を回さない）。

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::process::Command;

    use kura_engine::codec::CodecSet;
    use kura_engine::doc::{DecodeOptions, InputId, OutputId};
    use kura_engine::render::estimate::{EstimateSpec, MemoryEstimate, Sink, Source};
    use kura_engine::render::{Engine, InputValue};
    use kura_engine::testfiles::{AseCel, AseFrame, AseLayer, FileGen};
    use kura_engine::testgen::InputGen;
    use kura_engine::testing::single_node_graph;
    use kura_engine::value::PixelFormat;
    use kura_engine::{Digest, Graph, NoMonitor, Value};
    use serde_json::json;

    /// 子プロセスに渡す「ケース:段」（段は `base`・`decode`・`run`）。
    const CASE_ENV: &str = "KURA_MEM_SMOKE_CASE";
    /// 子プロセスに渡す入力のファイル。
    const FILE_ENV: &str = "KURA_MEM_SMOKE_FILE";
    /// 描くプールのスレッドの数（見積もりの並列の数と同じ）。
    const TASKS: usize = 3;
    const MIB: u64 = 1 << 20;
    /// 余裕：見積もりの 1.25 倍 ＋ 4 MiB（アロケータの断片・テストの枠組み・スレッドのスタック・タイル 1 枚ぶんの一時の領域）。
    fn bound(estimate: u64) -> u64 {
        estimate * 5 / 4 + 4 * MIB
    }

    /// ケース：入力のファイル（とその形式）、文書、書き出しの形式と設定。
    struct Case {
        name: &'static str,
        input: Input,
        format_in: &'static str,
        graph: fn() -> Graph,
        format_out: &'static str,
        opts: fn() -> serde_json::Value,
    }

    /// 入力のファイルの作り方。
    enum Input {
        /// 値を `format_in`・設定で書いたもの。
        Encoded {
            value: fn() -> Value,
            write_opts: fn() -> serde_json::Value,
        },
        /// そのままのバイト列（エンジンが書けない形式。aseprite）。
        Bytes(fn() -> Vec<u8>),
    }

    impl Input {
        fn bytes(&self, format: &str) -> Vec<u8> {
            match self {
                Self::Encoded { value, write_opts } => {
                    CodecSet::pinned()
                        .encode_bytes(&value(), format, &write_opts(), &NoMonitor)
                        .unwrap()
                        .0
                }
                Self::Bytes(make) => make(),
            }
        }
    }

    /// 層の多いアニメの aseprite（256² の RGBA・8 層・4 フレーム。フレーム 1 からは、層 0〜3 がフレーム 0 のセルへのリンクで、
    /// 層 4〜7 は新しいセル）。セルはどれもキャンバスいっぱいの圧縮した画像。
    fn layered_aseprite() -> Vec<u8> {
        let cel = |layer: u16, kind: u16| AseCel {
            layer,
            x: 0,
            y: 0,
            opacity: 255,
            z: 0,
            kind,
            w: 256,
            h: 256,
            link: 0,
            flip: false,
        };
        FileGen::Aseprite {
            w: 256,
            h: 256,
            depth: 32,
            flags: 1,
            transparent: 0,
            palette_len: 2,
            layers: (0..8)
                .map(|_| AseLayer {
                    flags: 1,
                    kind: 0,
                    level: 0,
                    blend: 0,
                    opacity: 200,
                })
                .collect(),
            frames: (0..4)
                .map(|f| AseFrame {
                    duration: 100,
                    cels: (0..8)
                        .map(|l| cel(l, if f > 0 && l < 4 { 1 } else { 2 }))
                        .collect(),
                })
                .collect(),
            tileset: None,
            tags: vec![],
            slices: vec![],
            seed: 4,
            opaque_palette: false,
        }
        .bytes()
        .unwrap()
    }

    fn cases() -> Vec<Case> {
        vec![
            // プログレッシブの JPEG の読み（値の 3 倍）・回転・平滑な縮小・PNG の書き（fast は 4 倍）
            Case {
                name: "jpeg-rotate-resize-png",
                input: Input::Encoded {
                    value: || {
                        InputGen::Photo {
                            w: 1536,
                            h: 1024,
                            seed: 1,
                        }
                        .generate()
                        .unwrap()
                    },
                    write_opts: || json!({ "quality": 90, "progressive": true }),
                },
                format_in: "jpeg",
                graph: || {
                    let mut g =
                        single_node_graph("image.rotate90", 1, json!({ "turn": "cw" })).unwrap();
                    g.push_linear(
                        "image.resize",
                        1,
                        json!({ "width": { "px": 512 }, "height": "auto", "filter": "lanczos3" }),
                        0,
                    )
                    .unwrap();
                    g
                },
                format_out: "png",
                opts: || json!({ "compression": "fast" }),
            },
            // PNG の読み・Indexed8 への減色（作業領域が値の数十倍）・PNG の書き
            Case {
                name: "png-indexed8-png",
                input: Input::Encoded {
                    value: || {
                        InputGen::Noise {
                            w: 256,
                            h: 256,
                            fmt: PixelFormat::Rgb8,
                            seed: 2,
                        }
                        .generate()
                        .unwrap()
                    },
                    write_opts: || json!({}),
                },
                format_in: "png",
                graph: || {
                    single_node_graph(
                        "image.convert",
                        1,
                        json!({ "format": "indexed8", "colors": 256, "dither": "errorDiffusion", "matte": [255, 255, 255] }),
                    )
                    .unwrap()
                },
                format_out: "png",
                opts: || json!({}),
            },
            // WAV の読み・音量・Ogg Vorbis の書き（値の 1.8 倍）
            Case {
                name: "wav-gain-ogg",
                input: Input::Encoded {
                    value: || {
                        InputGen::AudioNoise {
                            rate: 44_100,
                            channels: 2,
                            frames: 44_100 * 4,
                            seed: 3,
                        }
                        .generate()
                        .unwrap()
                    },
                    write_opts: || json!({}),
                },
                format_in: "wav",
                graph: || single_node_graph("audio.gain", 1, json!({ "db": -3.0 })).unwrap(),
                format_out: "ogg",
                opts: || json!({ "quality": 5 }),
            },
            // 層の多いアニメの aseprite の読み（ファイル全体 ＋ リンクが指すセル ＋ キャンバス）・反転・APNG の書き
            Case {
                name: "aseprite-flip-apng",
                input: Input::Bytes(layered_aseprite),
                format_in: "aseprite",
                graph: || {
                    single_node_graph("image.flip", 1, json!({ "axis": "horizontal" })).unwrap()
                },
                format_out: "png",
                opts: || json!({}),
            },
            // 1px の市松の 8 連結の塗りつぶし（区間の待ち行列が画素の数に比例する形。集合 2 枚 ＋ 待ち行列の上限）
            Case {
                name: "png-fill-png",
                input: Input::Encoded {
                    value: || {
                        InputGen::Checker {
                            w: 2048,
                            h: 2048,
                            fmt: PixelFormat::Gray8,
                            cell: 1,
                        }
                        .generate()
                        .unwrap()
                    },
                    write_opts: || json!({}),
                },
                format_in: "png",
                graph: || {
                    single_node_graph(
                        "paint.actions",
                        1,
                        json!({ "actions": [{ "type": "fill", "at": { "x": 1024, "y": 1024 },
                            "color": { "rgba": [10, 10, 10, 255] }, "tolerance": 0,
                            "connectivity": "eight", "contiguous": true, "frame": 0 }] }),
                    )
                    .unwrap()
                },
                format_out: "png",
                opts: || json!({}),
            },
        ]
    }

    /// 子プロセス：環境変数のケースを、段（`base` はファイルに触れる前・`decode` は形と値を読んだところ・`run` は書くまで）
    /// まで回す。親のテストから起こすだけなので無視にしてある。
    #[test]
    #[ignore = "peak_stays_within_the_estimate が子プロセスとして起こす"]
    fn child() {
        let (Some(spec), Some(file)) = (std::env::var(CASE_ENV).ok(), std::env::var_os(FILE_ENV))
        else {
            return;
        };
        let (name, stage) = spec.split_once(':').unwrap();
        let case = cases().into_iter().find(|c| c.name == name).unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(TASKS)
            .build()
            .unwrap();
        let engine = Engine::new().unwrap();
        let codecs = CodecSet::pinned();
        let graph = (case.graph)();
        let opts = (case.opts)();
        let path = Path::new(&file);
        pool.install(|| {
            // 基準はファイルに触れる前まで（形を読むのに全体を読む形式（aseprite）の分も、読む段に入れる）
            if stage == "base" {
                return;
            }
            let decode = DecodeOptions::default();
            let (shape, _) = codecs
                .shape_path(path, case.format_in, &decode, engine.limits())
                .unwrap();
            let plan = engine
                .compile(&graph, &BTreeMap::from([(InputId(0), shape)]))
                .unwrap();
            let (value, _) = codecs
                .decode_path(path, case.format_in, &decode, engine.limits(), &NoMonitor)
                .unwrap();
            if stage == "decode" {
                return;
            }
            let inputs = BTreeMap::from([(
                InputId(0),
                InputValue {
                    value,
                    source: Digest::default(),
                },
            )]);
            let rendered = engine.render(&plan, &inputs, &NoMonitor).unwrap();
            let (bytes, _) = codecs
                .encode_bytes(
                    &rendered.outputs[0].value,
                    case.format_out,
                    &opts,
                    &NoMonitor,
                )
                .unwrap();
            assert!(!bytes.is_empty());
        });
    }

    /// 子プロセスのピーク（バイト）。`/usr/bin/time -l` が使えなければ（ない・peak memory footprint を出さない）None。
    fn peak(case: &Case, file: &Path, stage: &str) -> Option<u64> {
        let exe = std::env::current_exe().unwrap();
        let out = Command::new("/usr/bin/time")
            .arg("-l")
            .arg(exe)
            .args([
                "--exact",
                "mem_smoke::tests::child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
                "-q",
            ])
            .env(CASE_ENV, format!("{}:{stage}", case.name))
            .env(FILE_ENV, file)
            .output()
            .ok()?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        let footprint = stderr
            .lines()
            .find(|l| l.contains("peak memory footprint"))
            .and_then(|l| l.split_whitespace().next())
            .and_then(|n| n.parse().ok())?;
        assert!(out.status.success(), "{}（{stage}）: {stderr}", case.name);
        Some(footprint)
    }

    /// 親が求める見積もり（子と同じ文書・入力の形・読み方（ファイルの長さを含む）・書き方・並列の数。`render` は出力の
    /// ダイジェストを取る）。
    fn estimate(case: &Case, file: &Path) -> MemoryEstimate {
        let engine = Engine::new().unwrap();
        let codecs = CodecSet::pinned();
        let (shape, _) = codecs
            .shape_path(
                file,
                case.format_in,
                &DecodeOptions::default(),
                engine.limits(),
            )
            .unwrap();
        let plan = engine
            .compile(&(case.graph)(), &BTreeMap::from([(InputId(0), shape)]))
            .unwrap();
        let opts = (case.opts)();
        let len = std::fs::metadata(file).unwrap().len();
        let spec = EstimateSpec::new(TASKS as u64)
            .digests(true)
            .input(
                InputId(0),
                Source::Decode {
                    codecs: &codecs,
                    format: case.format_in,
                },
            )
            .input_len(InputId(0), len)
            .output(
                OutputId(0),
                Sink::Encode {
                    codecs: &codecs,
                    format: case.format_out,
                    opts: &opts,
                },
            );
        engine.estimate(&plan, &spec, None).unwrap()
    }

    fn mib(bytes: u64) -> f64 {
        bytes as f64 / MIB as f64
    }

    /// NFR-17：子プロセスで読む段だけ・読む描く書くの通しを回したピークから基準を引いた量が、それぞれ読む段の見積もり・
    /// 見積もりのピーク（段をまたいで残る分を含む）× 1.25 ＋ 4 MiB 以下。
    #[test]
    fn peak_stays_within_the_estimate() {
        let dir = tempfile::tempdir().unwrap();
        for case in cases() {
            let file = dir.path().join(format!("{}.{}", case.name, case.format_in));
            std::fs::write(&file, case.input.bytes(case.format_in)).unwrap();
            let est = estimate(&case, &file);
            let (Some(base), Some(decode), Some(run)) = (
                peak(&case, &file, "base"),
                peak(&case, &file, "decode"),
                peak(&case, &file, "run"),
            ) else {
                eprintln!("/usr/bin/time -l が使えないので、メモリの煙のテストを見送ります");
                return;
            };
            let (decoded, used) = (decode.saturating_sub(base), run.saturating_sub(base));
            eprintln!(
                "{}: 読む {:.1} MiB（見積もり {:.1}）・通し {:.1} MiB（見積もり {:.1}：描く {:.1}・書く {:.1}・持ち越し \
                 {:.1}＋{:.1}）・基準 {:.1} MiB",
                case.name,
                mib(decoded),
                mib(est.decode),
                mib(used),
                mib(est.peak),
                mib(est.render),
                mib(est.encode),
                mib(est.carried_decode),
                mib(est.carried_render),
                mib(base),
            );
            assert!(
                decoded <= bound(est.decode),
                "{}: 読む段の実測 {decoded} バイト > 見積もり {} × 1.25 ＋ 4 MiB",
                case.name,
                est.decode
            );
            assert!(
                used <= bound(est.peak),
                "{}: 通しの実測 {used} バイト > 見積もり {} × 1.25 ＋ 4 MiB",
                case.name,
                est.peak
            );
        }
    }
}
