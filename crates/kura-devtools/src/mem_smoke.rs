//! メモリの見積もりの煙のテスト（docs/editing.md §12・NFR-17。段 10）。固定の小さな入力で、読む・描く・書くの通しを
//! 子プロセスで回し、`/usr/bin/time -l` の peak memory footprint から基準（同じ子で、ファイルに触れる前に終えるもの）を引いた量が、
//! エンジンの見積もり（`Engine::estimate`）× 1.25 ＋ 4 MiB 以下であることを確かめる。読む段だけの量も、読む段の見積もりと
//! 照らす。コーデック・操作の作業領域の倍率が実装の変更で黙って足りなくなるのを、macOS の CI で捕まえる（倍率そのものの
//! 実測と大きな入力は bench edit）。
//!
//! M13 段 1 のケース `audio-60min-step`（60 分の合成の源の音声に 1 手。E41 の実測）は重いので、別のテスト
//! `audio_60min_step_stays_within_n`（`#[ignore]`。`--release` で回す）にした。判定は N（源の LRU の上限 ＋ 64 MiB）と、範囲の
//! 描画の見積もり（＋ キャッシュが数える源の LRU）× 1.25 ＋ 4 MiB。
//!
//! M13 段 2 のケース `audio-60min-analyze`（60 分・48 kHz ステレオの 16 ビットの WAV を流して解析する。値を持たない）も重いので、
//! 別のテスト `audio_60min_analyze_stays_within_n`（`#[ignore]`。`--release` で回す）にした。判定は N（96 MiB）と、解析のジョブの
//! 見積もり（`kura_core::analysis::job::estimate_stream`）× 1.25 ＋ 4 MiB。
//!
//! M13 段 3 のケース `audio-60min-save`（60 分の合成の源の音声を FLAC・ogg q5・WAV に流して書く。値も書いたバイト列も持たない）も
//! 重いので、別のテスト `audio_60min_save_stays_within_n`（`#[ignore]`。`--release` で回す）にした。判定は N（源の LRU の上限 ＋
//! 128 MiB）と、保存の見積もり（`EstimateSpec::stream`）× 1.25 ＋ 4 MiB。
//!
//! M13 段 4C のケース `audio-editor-60min`（60 分・48 kHz ステレオの 16 ビットの WAV をアプリの取り込みのジョブでライブラリに
//! 入れ、kura-core のエディタで区間の表示で開いて gain・fade・trim の終わり・ループ点の手を当て、裏の仕事を済ませる）も重いので、
//! 別のテスト `audio_editor_60min_stays_within_n`（`#[ignore]`。`--release` で回す）にした。判定は N（保存の N ＋ 持ち続ける分の
//! 上限 70 MiB）と、エディタの見積もり（キャッシュ ＋ 確保の見積もりのピーク ＋ 持ち続ける分）× 1.25 ＋ 4 MiB。
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

    /// 子プロセスを `/usr/bin/time -l` で起こす形。アプリと同じ libmalloc の設定（Info.plist の `LSEnvironment`。ADR-183）にする。
    fn time_l() -> Command {
        let mut cmd = Command::new("/usr/bin/time");
        cmd.arg("-l").env(kura_mac::SPACE_EFFICIENT_ENV, "1");
        cmd
    }

    /// 子プロセスのピーク（バイト）。`/usr/bin/time -l` が使えなければ（ない・peak memory footprint を出さない）None。
    fn peak(case: &Case, file: &Path, stage: &str) -> Option<u64> {
        let exe = std::env::current_exe().unwrap();
        let out = time_l()
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

    // ---- M13 段 1：60 分の音声の 1 手（合成の源） ----

    /// 子プロセスに渡す音声のケース。
    const AUDIO_CASE: &str = "audio-60min-step";
    /// 音声の長さ（秒）と源の LRU の上限。
    const AUDIO_SECONDS: u64 = 60 * 60;
    const AUDIO_RATE: u64 = 48_000;
    const AUDIO_LRU: u64 = 64 * MIB;

    /// trim → fade → normalize → gain → set_loop（kura-engine の tests/long_audio.rs と同じ形）。
    fn audio_document(frames: u64, db: f64) -> Graph {
        let r = AUDIO_RATE;
        let mut g = single_node_graph(
            "audio.trim",
            1,
            json!({ "start": r + 123, "end": frames - r }),
        )
        .unwrap();
        g.push_linear(
            "audio.fade",
            1,
            json!({ "fadeIn": 2 * r, "fadeOut": 3 * r, "curve": "equalPower" }),
            2,
        )
        .unwrap();
        g.push_linear(
            "audio.normalize",
            1,
            json!({ "target": -16.0, "peakLimit": -1.0 }),
            3,
        )
        .unwrap();
        g.push_linear("audio.gain", 1, json!({ "db": db }), 4)
            .unwrap();
        g.push_linear(
            "audio.set_loop",
            1,
            json!({ "loop": { "start": 10 * r, "end": 20 * r } }),
            5,
        )
        .unwrap();
        g
    }

    /// 音声の 1 手の通し：60 分・48 kHz ステレオの合成の源（LRU 64 MiB）を遅延の入力にして、見えている 10 秒 ＋ 試聴の 6 秒を
    /// 表示し（最初の表示で normalize の計測を流して解く）、ゲインの引数の 1 手を表示する。返すのは（最初の表示の見積もりの
    /// ピーク, 1 手の見積もりのピーク, 1 手で源が作ったチャンクの数）。
    fn audio_flow() -> (u64, u64, u64) {
        use kura_engine::render::cache::{Fingerprint, FingerprintedInput, NodeCache};
        use kura_engine::render::estimate::CacheProbe;
        use kura_engine::render::region::Area;
        use kura_engine::render::view::ViewRequest;
        use kura_engine::testing::source::SyntheticSource;
        use kura_engine::value::{Audio, AudioMeta};
        use std::sync::Arc;
        let frames = AUDIO_SECONDS * AUDIO_RATE;
        let engine = Engine::new().unwrap();
        let src = Arc::new(SyntheticSource::new(
            AUDIO_RATE as u32,
            2,
            frames,
            7,
            AUDIO_LRU,
        ));
        let value = Value::Audio(Arc::new(
            Audio::from_source(src.clone(), AudioMeta::default()).unwrap(),
        ));
        let shapes = BTreeMap::from([(InputId(0), value.shape())]);
        let inputs = BTreeMap::from([(
            InputId(0),
            FingerprintedInput {
                value,
                source: Digest([1; 32]),
                fingerprint: Fingerprint(Digest([2; 32])),
            },
        )]);
        let (at, audition) = (frames / 3, frames / 2);
        let area = Area::span(at, at + 10 * AUDIO_RATE)
            .union(&Area::span(audition, audition + 6 * AUDIO_RATE));
        let req = ViewRequest {
            output: OutputId(0),
            area: area.clone(),
            focus: None,
        };
        let mut cache = NodeCache::new(512 * MIB);
        cache.register_external(&inputs[&InputId(0)].value);
        let spec = |plan: &kura_engine::render::Plan| {
            EstimateSpec::new(TASKS as u64)
                .all_inputs(plan, Source::Held)
                .area(OutputId(0), area.clone())
        };
        let first_plan = engine
            .compile(&audio_document(frames, -2.0), &shapes)
            .unwrap();
        let first_est = engine
            .estimate(
                &first_plan,
                &spec(&first_plan),
                Some(CacheProbe {
                    cache: &cache,
                    inputs: &inputs,
                }),
            )
            .unwrap();
        let first = engine
            .render_view(&first_plan, &inputs, &mut cache, &req, None, &NoMonitor)
            .unwrap();
        let plan = engine
            .compile(&audio_document(frames, -5.0), &shapes)
            .unwrap();
        let step_est = engine
            .estimate(
                &plan,
                &spec(&plan),
                Some(CacheProbe {
                    cache: &cache,
                    inputs: &inputs,
                }),
            )
            .unwrap();
        let before = kura_engine::audio::source::AudioSource::stats(src.as_ref()).reads;
        let got = engine
            .render_view(
                &plan,
                &inputs,
                &mut cache,
                &req,
                Some(&first.view),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(got.area.presolved, 0);
        let reads = kura_engine::audio::source::AudioSource::stats(src.as_ref()).reads - before;
        (first_est.peak, step_est.peak, reads)
    }

    /// 子プロセス（音声の 1 手）：`base` は何も作らない、`run` は通しを回す。
    #[test]
    #[ignore = "audio_60min_step_stays_within_n が子プロセスとして起こす"]
    fn audio_child() {
        let Ok(spec) = std::env::var(CASE_ENV) else {
            return;
        };
        let Some((name, stage)) = spec.split_once(':') else {
            return;
        };
        if name != AUDIO_CASE || stage == "base" {
            return;
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(TASKS)
            .build()
            .unwrap();
        pool.install(audio_flow);
    }

    // ---- M13 段 2：60 分の音声の解析（流して解く） ----

    /// 子プロセスに渡す解析のケース。
    const ANALYZE_CASE: &str = "audio-60min-analyze";
    /// 解析の N（流して解くので長さによらない数十 MB。窓 2 つ（16 MiB）・結果・ダイジェスト・読みの塊に余裕を見る）。
    const ANALYZE_N: u64 = 96 * MIB;

    /// 60 分・48 kHz ステレオの 16 ビットの WAV を `path` に書く（標本は位置から決める雑音と正弦。値は丸ごと作らない）。
    fn write_long_wav(path: &Path) {
        use std::io::Write;
        let frames = AUDIO_SECONDS * AUDIO_RATE;
        let data = frames * 2 * 2;
        let mut f =
            std::io::BufWriter::with_capacity(4 << 20, std::fs::File::create(path).unwrap());
        let mut head = Vec::new();
        head.extend_from_slice(b"RIFF");
        head.extend_from_slice(&((36 + data) as u32).to_le_bytes());
        head.extend_from_slice(b"WAVEfmt ");
        head.extend_from_slice(&16u32.to_le_bytes());
        head.extend_from_slice(&1u16.to_le_bytes());
        head.extend_from_slice(&2u16.to_le_bytes());
        head.extend_from_slice(&(AUDIO_RATE as u32).to_le_bytes());
        head.extend_from_slice(&((AUDIO_RATE * 4) as u32).to_le_bytes());
        head.extend_from_slice(&4u16.to_le_bytes());
        head.extend_from_slice(&16u16.to_le_bytes());
        head.extend_from_slice(b"data");
        head.extend_from_slice(&(data as u32).to_le_bytes());
        f.write_all(&head).unwrap();
        let mut x: u32 = 0x9e37_79b9;
        let mut buf = Vec::with_capacity(1 << 20);
        for i in 0..frames {
            for c in 0..2u32 {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                // ゆっくり大きさの変わる雑音（ゲートを通るブロックと通らないブロックを混ぜる）
                let level = ((i / AUDIO_RATE) % 40) as i32 * 300 + c as i32 * 50;
                let s = ((x >> 16) as i32 - 32_768) * level / 12_000;
                buf.extend_from_slice(&(s.clamp(-32_768, 32_767) as i16).to_le_bytes());
            }
            if buf.len() >= 1 << 20 {
                f.write_all(&buf).unwrap();
                buf.clear();
            }
        }
        f.write_all(&buf).unwrap();
        f.flush().unwrap();
    }

    /// 子プロセスに渡す入力の形式（解析のケース）。
    const FORMAT_ENV: &str = "KURA_MEM_SMOKE_FORMAT";
    /// 埋め込みの画像を持つ音声の解析のケース（M13 段 2 の検証の直し）。
    const HELD_CASES: [&str; 2] = ["audio-flac-pictures-analyze", "audio-m4a-covr-analyze"];

    /// 解析のジョブと同じ段で形を読む（段の順に試し、読めた段を返す）。
    fn staged_shape(
        codecs: &CodecSet,
        engine: &Engine,
        path: &Path,
        format: &str,
    ) -> (kura_engine::Shape, kura_engine::codec::PickedStage) {
        codecs
            .shape_path_staged(
                path,
                format,
                &DecodeOptions::default(),
                engine.limits(),
                &NoMonitor,
                None,
                &mut |_| Ok(()),
            )
            .unwrap()
    }

    /// 解析のジョブの見積もり（実際に読む段の `stage_stream_work_bytes` で数えた `estimate_stream` と、その段で形を読む分の
    /// 大きい方。`Run::solve_stream` と同じ）。
    fn analyze_estimate(path: &Path, format: &str) -> u64 {
        let codecs = CodecSet::pinned();
        let engine = Engine::new().unwrap();
        let (shape, stage) = staged_shape(&codecs, &engine, path, format);
        let len = std::fs::metadata(path).unwrap().len();
        let reqs = kura_core::analysis::plan::stream_requests(&shape);
        kura_core::analysis::job::estimate_stream(
            codecs.stage_stream_work_bytes(&stage, format, &shape, len),
            engine.analyses(),
            &shape,
            &reqs,
            TASKS as u64,
        )
        .max(stage.shape_bytes)
    }

    /// 解析の通し：ファイルを解析のジョブと同じ段で流して読み、同じ要求（`stream_requests`）を扇に流して解く。返すのは結果の
    /// 数。
    fn analyze_flow(path: &Path, format: &str) -> usize {
        let engine = Engine::new().unwrap();
        let codecs = CodecSet::pinned();
        let opts = DecodeOptions::default();
        let (shape, stage) = staged_shape(&codecs, &engine, path, format);
        let reqs = kura_core::analysis::plan::stream_requests(&shape);
        let mut fan = engine.analyze_stream(&reqs, &shape);
        codecs
            .decode_stream_with_path(
                stage.id,
                path,
                format,
                &opts,
                engine.limits(),
                &mut fan,
                &NoMonitor,
            )
            .unwrap();
        let got = fan.finish(&NoMonitor);
        assert!(
            got.iter().all(Result::is_ok),
            "{:?}",
            got.iter().map(|r| r.as_ref().err()).collect::<Vec<_>>()
        );
        got.len()
    }

    /// 子プロセス（音声の解析）：`base` は何も作らない、`run` は通しを回す。
    #[test]
    #[ignore = "audio_60min_analyze_stays_within_n・held_pictures_analyze_within_estimate が子プロセスとして起こす"]
    fn analyze_child() {
        let Ok(spec) = std::env::var(CASE_ENV) else {
            return;
        };
        let Some((name, stage)) = spec.split_once(':') else {
            return;
        };
        if !(name == ANALYZE_CASE || HELD_CASES.contains(&name)) || stage == "base" {
            return;
        }
        let path = std::path::PathBuf::from(std::env::var(FILE_ENV).unwrap());
        let format = std::env::var(FORMAT_ENV).unwrap_or_else(|_| "wav".into());
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(TASKS)
            .build()
            .unwrap();
        let t = std::time::Instant::now();
        let n = pool.install(|| analyze_flow(&path, &format));
        eprintln!("{name}: 結果 {n} 個・{:.2} 秒", t.elapsed().as_secs_f64());
    }

    /// 解析の子プロセスを `base`・`run` で起こし、(ピークから基準を引いた量, 基準, `run` の標準エラー)。`/usr/bin/time -l` が
    /// 使えなければ None。
    fn analyze_peak(name: &str, path: &Path, format: &str) -> Option<(u64, u64, String)> {
        let exe = std::env::current_exe().unwrap();
        let run = |stage: &str| -> Option<(u64, String)> {
            let out = time_l()
                .arg(&exe)
                .args([
                    "--exact",
                    "mem_smoke::tests::analyze_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                    "-q",
                ])
                .env(CASE_ENV, format!("{name}:{stage}"))
                .env(FILE_ENV, path)
                .env(FORMAT_ENV, format)
                .output()
                .ok()?;
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let footprint = stderr
                .lines()
                .find(|l| l.contains("peak memory footprint"))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|n| n.parse().ok())?;
            assert!(out.status.success(), "{stage}: {stderr}");
            Some((footprint, stderr))
        };
        let (base, _) = run("base")?;
        let (peak, log) = run("run")?;
        Some((peak.saturating_sub(base), base, log))
    }

    /// M13 段 2（D12）：60 分・48 kHz ステレオの 16 ビットの WAV を流して解析する子プロセスのピークから基準を引いた量が、
    /// N（96 MiB）以下で、解析のジョブの見積もり（`estimate_stream`）× 1.25 ＋ 4 MiB 以下。値（1.38 GB）を持たない。
    /// 重い（691 MB のファイルを書く）ので、ふだんは回さない：
    /// `cargo test --release -p kura-devtools mem_smoke::tests::audio_60min_analyze -- --ignored --nocapture`。
    #[test]
    #[ignore = "重い（--release で --ignored を付けて回す）"]
    fn audio_60min_analyze_stays_within_n() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.wav");
        let t = std::time::Instant::now();
        write_long_wav(&path);
        eprintln!(
            "{ANALYZE_CASE}: WAV を書いた（{:.1} 秒）",
            t.elapsed().as_secs_f64()
        );
        let est = analyze_estimate(&path, "wav");
        let Some((used, base, log)) = analyze_peak(ANALYZE_CASE, &path, "wav") else {
            eprintln!(
                "/usr/bin/time -l が使えないので、音声の解析のメモリの煙のテストを見送ります"
            );
            return;
        };
        let took = log
            .lines()
            .find(|l| l.starts_with(ANALYZE_CASE))
            .unwrap_or_default()
            .to_string();
        eprintln!(
            "{ANALYZE_CASE}: 通し {:.1} MiB（N {:.1}・見積もり {:.1}）・基準 {:.1} MiB・値なら {:.1} MiB・{took}",
            mib(used),
            mib(ANALYZE_N),
            mib(est),
            mib(base),
            mib(AUDIO_SECONDS * AUDIO_RATE * 2 * 4),
        );
        assert!(
            used <= ANALYZE_N,
            "{ANALYZE_CASE}: 実測 {used} バイト > N {ANALYZE_N}"
        );
        assert!(
            used <= bound(est),
            "{ANALYZE_CASE}: 実測 {used} バイト > 見積もり {est} × 1.25 ＋ 4 MiB"
        );
    }

    /// 2 秒・44.1 kHz ステレオの FLAC の STREAMINFO の後ろに、PICTURE（各 `size` バイトの画像）を `n` 個差し込む。
    fn flac_with_pictures(n: usize, size: usize) -> Vec<u8> {
        let v = InputGen::AudioNoise {
            rate: 44_100,
            channels: 2,
            frames: 88_200,
            seed: 7,
        }
        .generate()
        .unwrap();
        let bytes = CodecSet::pinned()
            .encode_bytes(&v, "flac", &json!({ "bits": 16 }), &NoMonitor)
            .unwrap()
            .0;
        let at = 4 + 4 + 34;
        let mut f = bytes[..at].to_vec();
        let was_last = f[4] & 0x80 != 0;
        f[4] &= 0x7f;
        for i in 0..n {
            let mut body = 3u32.to_be_bytes().to_vec();
            body.extend_from_slice(&9u32.to_be_bytes());
            body.extend_from_slice(b"image/png");
            body.extend_from_slice(&[0u8; 20]);
            body.extend_from_slice(&u32::try_from(size).unwrap().to_be_bytes());
            body.resize(body.len() + size, 0x11);
            f.push(6 | if was_last && i + 1 == n { 0x80 } else { 0 });
            f.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes()[1..]);
            f.extend_from_slice(&body);
        }
        f.extend_from_slice(&bytes[at..]);
        f
    }

    /// ffmpeg の AAC の m4a（フィクスチャ。moov が最後）の moov の後ろに、`size` バイトの covr を持つ udta/meta/ilst を足す。
    fn m4a_with_covr(size: usize) -> Vec<u8> {
        let b = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../kura-engine/fixtures/aac-lc-ffmpeg.m4a"),
        )
        .unwrap();
        let atom = |kind: &[u8; 4], body: &[u8]| {
            let mut o = u32::try_from(8 + body.len())
                .unwrap()
                .to_be_bytes()
                .to_vec();
            o.extend_from_slice(kind);
            o.extend_from_slice(body);
            o
        };
        let mut data = 14u32.to_be_bytes().to_vec();
        data.extend_from_slice(&[0; 4]);
        data.resize(data.len() + size, 0x55);
        let ilst = atom(b"ilst", &atom(b"covr", &atom(b"data", &data)));
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(b"mdirappl");
        hdlr.extend_from_slice(&[0u8; 9]);
        let mut meta = vec![0u8; 4];
        meta.extend_from_slice(&atom(b"hdlr", &hdlr));
        meta.extend_from_slice(&ilst);
        let udta = atom(b"udta", &atom(b"meta", &meta));
        let mut at = 0usize;
        let mut moov = None;
        while at + 8 <= b.len() {
            let s = u32::from_be_bytes(b[at..at + 4].try_into().unwrap()) as usize;
            if &b[at + 4..at + 8] == b"moov" {
                moov = Some((at, s));
            }
            at += s;
        }
        let (m, ms) = moov.unwrap();
        assert_eq!(m + ms, b.len(), "moov が最後でない");
        let mut f = b.clone();
        f[m..m + 4].copy_from_slice(&u32::try_from(ms + udta.len()).unwrap().to_be_bytes());
        f.extend_from_slice(&udta);
        f
    }

    /// M13 段 2 の検証の直し：大きな画像を埋め込んだ FLAC（15 MiB の PICTURE が 3 つ）と m4a（30 MiB の covr）を流して
    /// 解析する子プロセスのピークから基準を引いた量が、解析のジョブの見積もり × 1.25 ＋ 4 MiB 以下（読み手が持ち続ける画像を
    /// 数える。前の見積もりは中身によらない定数で、FLAC は 9.4 MiB に対して山 46.5 MiB だった）。
    #[test]
    fn held_pictures_analyze_within_estimate() {
        let dir = tempfile::tempdir().unwrap();
        for (name, format, bytes) in [
            (HELD_CASES[0], "flac", flac_with_pictures(3, 15 << 20)),
            (HELD_CASES[1], "m4a", m4a_with_covr(30 << 20)),
        ] {
            let path = dir.path().join(format!("{name}.{format}"));
            std::fs::write(&path, &bytes).unwrap();
            let est = analyze_estimate(&path, format);
            // 見積もりは画像を数える
            assert!(est > (bytes.len() as u64), "{name}: 見積もり {est}");
            let Some((used, base, _)) = analyze_peak(name, &path, format) else {
                eprintln!(
                    "/usr/bin/time -l が使えないので、{name} のメモリの煙のテストを見送ります"
                );
                return;
            };
            eprintln!(
                "{name}: 通し {:.1} MiB（見積もり {:.1}・ファイル {:.1}）・基準 {:.1} MiB",
                mib(used),
                mib(est),
                mib(bytes.len() as u64),
                mib(base)
            );
            assert!(
                used <= bound(est),
                "{name}: 実測 {used} バイト > 見積もり {est} × 1.25 ＋ 4 MiB"
            );
        }
    }
    /// 子プロセスのピーク（テスト `test` を、ケース `spec` で起こす）。`/usr/bin/time -l` が使えなければ None。
    fn peak_of(test: &str, spec: &str) -> Option<u64> {
        let exe = std::env::current_exe().unwrap();
        let out = time_l()
            .arg(exe)
            .args([
                "--exact",
                test,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
                "-q",
            ])
            .env(CASE_ENV, spec)
            .output()
            .ok()?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        let footprint = stderr
            .lines()
            .find(|l| l.contains("peak memory footprint"))
            .and_then(|l| l.split_whitespace().next())
            .and_then(|n| n.parse().ok())?;
        assert!(out.status.success(), "{spec}: {stderr}");
        Some(footprint)
    }

    /// M13 段 1（E41 の実測）：60 分の音声の 1 手の子プロセスのピーク（流し解きを含む最初の表示 ＋ 1 手）から基準を引いた量が、
    /// N（源の LRU の上限 ＋ 64 MiB）以下で、見積もり（範囲の描画の見積もりのピーク ＋ キャッシュが数える源の LRU）× 1.25 ＋
    /// 4 MiB 以下。重い（デバッグのビルドで数十秒を 2 回）ので、ふだんは回さない：
    /// `cargo test --release -p kura-devtools mem_smoke::tests::audio_60min_step -- --ignored --nocapture`。
    #[test]
    #[ignore = "重い（--release で --ignored を付けて回す）"]
    fn audio_60min_step_stays_within_n() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(TASKS)
            .build()
            .unwrap();
        let (first_est, step_est, reads) = pool.install(audio_flow);
        let test = "mem_smoke::tests::audio_child";
        let (Some(base), Some(run)) = (
            peak_of(test, &format!("{AUDIO_CASE}:base")),
            peak_of(test, &format!("{AUDIO_CASE}:run")),
        ) else {
            eprintln!("/usr/bin/time -l が使えないので、音声のメモリの煙のテストを見送ります");
            return;
        };
        let used = run.saturating_sub(base);
        let n = AUDIO_LRU + 64 * MIB;
        let est = first_est.max(step_est);
        eprintln!(
            "{AUDIO_CASE}: 通し {:.1} MiB（N {:.1}・見積もり 最初の表示 {:.1}／1 手 {:.1} ＋ 源の LRU {:.1}）・1 手で源が作った\
             チャンク {reads}・基準 {:.1} MiB",
            mib(used),
            mib(n),
            mib(first_est),
            mib(step_est),
            mib(AUDIO_LRU),
            mib(base),
        );
        assert!(used <= n, "{AUDIO_CASE}: 実測 {used} バイト > N {n}");
        assert!(
            used <= bound(est + AUDIO_LRU),
            "{AUDIO_CASE}: 実測 {used} バイト > 見積もり {} × 1.25 ＋ 4 MiB",
            est + AUDIO_LRU
        );
    }

    // ---- M13 段 3：60 分の音声の保存（流して書く） ----

    /// 子プロセスに渡す保存のケース。
    const SAVE_CASE: &str = "audio-60min-save";
    /// 保存の N（源の LRU の上限 ＋ 128 MiB。窓・エンコーダの流れの作業領域・ダイジェスト・流し解きの結果）。音声のチャンク
    /// （1 チャンネル 256 KiB）を libmalloc が抱える分は、アプリと同じ設定（`MallocSpaceEfficient`。[`time_l`]）で返るので数えない
    /// （M13 段 7。設定なしでは FLAC 163・ogg 196・WAV 158 MiB、ありでは 25.5・34.5・25.4 MiB。ADR-183）。
    const SAVE_N: u64 = AUDIO_LRU + 128 * MIB;

    /// 保存の見積もり（60 分の合成の源を遅延の入力に、trim → fade → normalize → gain → set_loop を `format` に流して書く）。
    fn save_estimate(format: &str, opts: &serde_json::Value) -> u64 {
        let frames = AUDIO_SECONDS * AUDIO_RATE;
        let engine = Engine::new().unwrap();
        let codecs = CodecSet::pinned();
        let shape = kura_engine::Shape::Audio {
            rate: AUDIO_RATE as u32,
            channels: 2,
            frames: kura_engine::Dim::Exact(frames),
        };
        let plan = engine
            .compile(
                &audio_document(frames, -2.0),
                &BTreeMap::from([(InputId(0), shape)]),
            )
            .unwrap();
        let spec = EstimateSpec::new(TASKS as u64)
            .stream(OutputId(0))
            .digests(true)
            .input(
                InputId(0),
                Source::Lazy {
                    resident: AUDIO_LRU,
                },
            )
            .output(
                OutputId(0),
                Sink::Encode {
                    codecs: &codecs,
                    format,
                    opts,
                },
            );
        engine.estimate(&plan, &spec, None).unwrap().peak
    }

    /// 保存の通し：60 分・48 kHz ステレオの合成の源（LRU 64 MiB）を遅延の入力にして、計画の出力を `format` に流して書く
    /// （書き先は一時のファイル）。返すのは書いたバイト数。
    fn save_flow(format: &str, opts: &serde_json::Value, out: &Path) -> u64 {
        use kura_engine::render::cache::{Fingerprint, FingerprintedInput, NodeCache};
        use kura_engine::testing::source::SyntheticSource;
        use kura_engine::value::{Audio, AudioMeta};
        use std::sync::Arc;
        let frames = AUDIO_SECONDS * AUDIO_RATE;
        let engine = Engine::new().unwrap();
        let codecs = CodecSet::pinned();
        let src = Arc::new(SyntheticSource::new(
            AUDIO_RATE as u32,
            2,
            frames,
            7,
            AUDIO_LRU,
        ));
        let value = Value::Audio(Arc::new(
            Audio::from_source(src, AudioMeta::default()).unwrap(),
        ));
        let shapes = BTreeMap::from([(InputId(0), value.shape())]);
        let inputs = BTreeMap::from([(
            InputId(0),
            FingerprintedInput {
                value,
                source: Digest([1; 32]),
                fingerprint: Fingerprint(Digest([2; 32])),
            },
        )]);
        let plan = engine
            .compile(&audio_document(frames, -2.0), &shapes)
            .unwrap();
        let mut cache = NodeCache::new(0);
        let mut stream = engine
            .output_stream(&plan, &inputs, &mut cache, OutputId(0), &NoMonitor)
            .unwrap();
        let mut file = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
        let se = codecs
            .encode_stream(&mut stream, format, opts, &mut file, &NoMonitor)
            .unwrap();
        use std::io::Write;
        file.flush().unwrap();
        se.bytes
    }

    fn save_opts(format: &str) -> serde_json::Value {
        match format {
            "ogg" => json!({ "quality": 5 }),
            _ => json!({}),
        }
    }

    /// 子プロセス（音声の保存）：`base` は何も作らない、`run` は通しを回す。
    #[test]
    #[ignore = "audio_60min_save_stays_within_n が子プロセスとして起こす"]
    fn save_child() {
        let Ok(spec) = std::env::var(CASE_ENV) else {
            return;
        };
        let Some((name, stage)) = spec.split_once(':') else {
            return;
        };
        if name != SAVE_CASE || stage == "base" {
            return;
        }
        let path = std::path::PathBuf::from(std::env::var(FILE_ENV).unwrap());
        let format = std::env::var(FORMAT_ENV).unwrap_or_else(|_| "flac".into());
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(TASKS)
            .build()
            .unwrap();
        let t = std::time::Instant::now();
        let n = pool.install(|| save_flow(&format, &save_opts(&format), &path));
        eprintln!(
            "{name}・{format}: {n} バイト・{:.2} 秒",
            t.elapsed().as_secs_f64()
        );
    }

    /// M13 段 3（E7）：60 分・48 kHz ステレオの合成の源を FLAC・ogg q5 に流して書く子プロセスのピークから基準を引いた量が、
    /// N（源の LRU ＋ 128 MiB）以下で、保存の見積もり（`EstimateSpec::stream`）× 1.25 ＋ 4 MiB 以下。値（1.38 GB）も書いた
    /// バイト列も持たない。重いので、ふだんは回さない：
    /// `cargo test --release -p kura-devtools mem_smoke::tests::audio_60min_save -- --ignored --nocapture`。
    #[test]
    #[ignore = "重い（--release で --ignored を付けて回す）"]
    fn audio_60min_save_stays_within_n() {
        let dir = tempfile::tempdir().unwrap();
        let mut failed: Vec<String> = Vec::new();
        for format in ["flac", "ogg", "wav"] {
            let est = save_estimate(format, &save_opts(format));
            let path = dir.path().join(format!("out.{format}"));
            let exe = std::env::current_exe().unwrap();
            let run = |stage: &str| -> Option<(u64, String)> {
                let out = time_l()
                    .arg(&exe)
                    .args([
                        "--exact",
                        "mem_smoke::tests::save_child",
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                        "-q",
                    ])
                    .env(CASE_ENV, format!("{SAVE_CASE}:{stage}"))
                    .env(FILE_ENV, &path)
                    .env(FORMAT_ENV, format)
                    .output()
                    .ok()?;
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                let footprint = stderr
                    .lines()
                    .find(|l| l.contains("peak memory footprint"))
                    .and_then(|l| l.split_whitespace().next())
                    .and_then(|n| n.parse().ok())?;
                assert!(out.status.success(), "{stage}: {stderr}");
                Some((footprint, stderr))
            };
            let (Some((base, _)), Some((peak, log))) = (run("base"), run("run")) else {
                eprintln!("/usr/bin/time -l が使えないので、保存のメモリの煙のテストを見送ります");
                return;
            };
            let used = peak.saturating_sub(base);
            let line = log
                .lines()
                .find(|l| l.contains(SAVE_CASE))
                .unwrap_or("")
                .to_string();
            eprintln!(
                "{SAVE_CASE}・{format}: 通し {:.1} MiB（N {:.1}・見積もり {:.1}）・基準 {:.1} MiB・{line}",
                mib(used),
                mib(SAVE_N),
                mib(est),
                mib(base),
            );
            if used > SAVE_N {
                failed.push(format!("{format}: 実測 {used} バイト > N {SAVE_N}"));
            }
            if used > bound(est) {
                failed.push(format!(
                    "{format}: 実測 {used} バイト > 見積もり {est} × 1.25 ＋ 4 MiB"
                ));
            }
        }
        assert!(failed.is_empty(), "{failed:?}");
    }

    // ---- M13 段 4C（F15）：60 分の音声のエディタの 1 手（kura-core のエディタの道） ----

    /// 子プロセスに渡すエディタのケース。
    const EDITOR_CASE: &str = "audio-editor-60min";
    /// 子プロセスに渡すアイテムの ID（`FILE_ENV` はライブラリの場所）。
    const ITEM_ENV: &str = "KURA_MEM_SMOKE_ITEM";
    /// 子プロセスが結果の JSON の行の頭に付ける印。
    const RESULT_MARK: &str = "KURA_MEM_SMOKE_RESULT ";
    /// エディタの N：保存の N（源の LRU の上限 ＋ 128 MiB：窓・作業領域・流し解きの結果。裏の仕事は保存と
    /// 同じく出力を窓ごとに流す）に、エディタが持ち続ける分の上限 70 MiB（design-stage46.md §1.7：区間の表示 24 MiB × 2 ＋
    /// 波形の段 3 つ）を足したもの。
    const EDITOR_N: u64 = SAVE_N + 70 * MIB;
    /// 持ち続ける分（`CacheLease::extra`）の上限（A18 と同じ）。
    const EDITOR_EXTRA: u64 = 70 * MIB;
    /// 子が種類ごとに当てる手の数。
    const EDITOR_STEPS: usize = 8;

    /// 子プロセス（音声のエディタ）：`base` はライブラリを開いて実行の場とコーデックの組を作るだけ。`run` は 60 分の WAV を
    /// 区間（48 チャンク）の表示で開き、文書（trim → fade → gain → set_loop）を作って裏の仕事（波形の段・ラウドネス）を
    /// 済ませ、フェードインの終わり・trim の終わり・ループ点・gain の手を種類ごとに当て、gain のあとの裏の仕事を済ませて
    /// 閉じる。エディタの数（キャッシュ・持ち続ける分・確保の見積もりのピークの最大）を 1 行の JSON で標準出力に書く。
    #[test]
    #[ignore = "audio_editor_60min_stays_within_n が子プロセスとして起こす"]
    fn editor_child() {
        use crate::bench_edit::audio::{self, AudioEditor, Kind};
        let Ok(spec) = std::env::var(CASE_ENV) else {
            return;
        };
        let Some((name, stage)) = spec.split_once(':') else {
            return;
        };
        if name != EDITOR_CASE {
            return;
        }
        let lib = std::path::PathBuf::from(std::env::var(FILE_ENV).unwrap());
        let item: i64 = std::env::var(ITEM_ENV).unwrap().parse().unwrap();
        let session = kura_core::library::LibrarySession::open(
            &lib,
            audio::quiet(),
            audio::services(TASKS, TASKS, None),
        )
        .unwrap();
        session.wait_ready();
        crate::bench_app::settle(&session).unwrap();
        let engine = session.engine().unwrap();
        let budget = engine.runtime().unwrap().budget().total();
        let _codecs = engine.codecs();
        if stage == "base" {
            session.close();
            println!("{RESULT_MARK}{}", json!({ "stage": "base" }));
            return;
        }
        let frames = AUDIO_SECONDS * AUDIO_RATE;
        // 見立てのための数：確保器の目印で、エディタを開いてからの生きている確保（解放した分を引いた総量）の山を段ごとに数える
        // （確保器が抱える解放済みの領域は入らない。判定には使わない）
        let meter = crate::ALLOC.meter();
        meter.start(u64::MAX);
        let mut heap = Vec::new();
        let mut phase = |name: &str| {
            heap.push((name.to_string(), meter.peak()));
            meter.reset_peak();
        };
        let t = std::time::Instant::now();
        let mut ed = AudioEditor::open(&session, item, frames).unwrap();
        let open_ms = ed.open_ns as f64 / 1e6;
        let input = ed.opened.cache_bytes;
        phase("open");
        ed.setup().unwrap();
        phase("setup");
        let first = ed.settle(true).unwrap();
        phase("work-setup");
        for kind in Kind::ALL {
            for i in 0..EDITOR_STEPS {
                ed.step(kind, i).unwrap();
            }
            phase(kind.key());
        }
        let after = ed.settle(true).unwrap();
        phase("work-gain");
        let peak = ed.take_admitted_peak().unwrap();
        let held = ed.held;
        let (restored, _, work_files) = ed.close().unwrap();
        session.close();
        println!(
            "{RESULT_MARK}{}",
            json!({
                "stage": "run",
                "budget": budget,
                "open_ms": open_ms,
                "input_cache_bytes": input,
                "max_cache_bytes": held.max_cache,
                "max_extra_bytes": held.max_extra,
                "max_lease_bytes": held.max_lease,
                "admitted_peak_bytes": peak,
                "first_work_ms": first.loudness.1 as f64 / 1e6,
                "gain_work_ms": after.loudness.1 as f64 / 1e6,
                "restored": restored,
                "work_files": work_files,
                "seconds": t.elapsed().as_secs_f64(),
                "heap_peaks": heap,
            })
        );
    }

    /// M13 段 4C（F15）：60 分・48 kHz ステレオの 16 ビットの WAV をアプリの取り込みのジョブで新しいライブラリに入れ、子プロセスで
    /// kura-core のエディタを通して開いて手を当てる（`editor_child`）。ピークから基準（ライブラリ・実行の場・コーデックの組を作る
    /// だけの子）を引いた量が、N（[`EDITOR_N`]）以下で、エディタの見積もり（キャッシュが持つ量（入力の源の LRU の上限を含む）の
    /// 最大 ＋ 描く前に確保した見積もりのピークの最大 ＋ 持ち続ける分（`extra`）の最大）× 1.25 ＋ 4 MiB
    /// 以下。持ち続ける分は 70 MiB 以下、閉じたら予算と作業用のファイルが戻る。重い
    /// （691 MB のファイルを書いて取り込む）ので、ふだんは回さない：
    /// `cargo test --release -p kura-devtools mem_smoke::tests::audio_editor_60min -- --ignored --nocapture`。
    #[test]
    #[ignore = "重い（--release で --ignored を付けて回す）"]
    fn audio_editor_60min_stays_within_n() {
        use crate::bench_edit::audio;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let t = std::time::Instant::now();
        audio::write_wav(
            &src.join("long.wav"),
            AUDIO_RATE as u32,
            AUDIO_SECONDS * AUDIO_RATE,
        )
        .unwrap();
        let lib = dir.path().join("editor.kuralib");
        let item = audio::prepare_library(&lib, &src).unwrap();
        // ライブラリが写しを持つので、元の WAV は消す
        std::fs::remove_dir_all(&src).unwrap();
        eprintln!(
            "{EDITOR_CASE}: WAV を書いて取り込んだ（{:.1} 秒）",
            t.elapsed().as_secs_f64()
        );
        let exe = std::env::current_exe().unwrap();
        let run = |stage: &str| -> Option<(u64, serde_json::Value)> {
            let out = time_l()
                .arg(&exe)
                .args([
                    "--exact",
                    "mem_smoke::tests::editor_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                    "-q",
                ])
                .env(CASE_ENV, format!("{EDITOR_CASE}:{stage}"))
                .env(FILE_ENV, &lib)
                .env(ITEM_ENV, item.to_string())
                .output()
                .ok()?;
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            let footprint = stderr
                .lines()
                .find(|l| l.contains("peak memory footprint"))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|n| n.parse().ok())?;
            assert!(out.status.success(), "{stage}: {stderr}");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let got = stdout
                .lines()
                .find_map(|l| l.strip_prefix(RESULT_MARK))
                .and_then(|l| serde_json::from_str(l).ok())
                .unwrap_or_else(|| panic!("{stage}: 結果がありません: {stdout} {stderr}"));
            Some((footprint, got))
        };
        let (Some((base, _)), Some((peak, got))) = (run("base"), run("run")) else {
            eprintln!(
                "/usr/bin/time -l が使えないので、音声のエディタのメモリの煙のテストを見送ります"
            );
            return;
        };
        let used = peak.saturating_sub(base);
        let field = |k: &str| got[k].as_u64().unwrap();
        let (cache, extra, admitted) = (
            field("max_cache_bytes"),
            field("max_extra_bytes"),
            field("admitted_peak_bytes"),
        );
        let est = cache + extra + admitted;
        eprintln!(
            "{EDITOR_CASE}: 通し {:.1} MiB（N {:.1}・見積もり {:.1} ＝ キャッシュ {:.1}（入力の源の LRU {:.1} を含む）＋ 確保の見積もりの\
             ピーク {:.1} ＋ 持ち続ける分 {:.1}。見積もり × 1.25 ＋ 4 MiB を {:+.1} MiB）・基準 {:.1} MiB・開く {:.1} ms・\
             裏の仕事 {:.0} ms（文書を作ったあと）／{:.0} ms（gain のあと）・予算 {:.0} MiB・子 {:.1} 秒",
            mib(used),
            mib(EDITOR_N),
            mib(est),
            mib(cache),
            mib(field("input_cache_bytes")),
            mib(admitted),
            mib(extra),
            (used as f64 - bound(est) as f64) / MIB as f64,
            mib(base),
            got["open_ms"].as_f64().unwrap_or(0.0),
            got["first_work_ms"].as_f64().unwrap_or(0.0),
            got["gain_work_ms"].as_f64().unwrap_or(0.0),
            mib(field("budget")),
            got["seconds"].as_f64().unwrap_or(0.0),
        );
        // 見立て：生きている確保の山（段ごと）。通しとの差は、確保器が抱える解放済みの領域・スタックなど
        let heap: Vec<(String, u64)> = got["heap_peaks"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_u64()?)))
                    .collect()
            })
            .unwrap_or_default();
        let heap_max = heap.iter().map(|(_, b)| *b).max().unwrap_or(0);
        eprintln!(
            "{EDITOR_CASE}: 生きている確保の山 {:.1} MiB（{}）・通しとの差 {:.1} MiB",
            mib(heap_max),
            heap.iter()
                .map(|(n, b)| format!("{n} {:.1}", mib(*b)))
                .collect::<Vec<_>>()
                .join("・"),
            (used as f64 - heap_max as f64) / MIB as f64,
        );
        let mut failed: Vec<String> = Vec::new();
        if used > EDITOR_N {
            failed.push(format!("実測 {used} バイト > N {EDITOR_N}"));
        }
        if used > bound(est) {
            failed.push(format!(
                "実測 {used} バイト > 見積もり {est} × 1.25 ＋ 4 MiB"
            ));
        }
        if extra > EDITOR_EXTRA {
            failed.push(format!("持ち続ける分 {extra} バイト > 70 MiB"));
        }
        if got["restored"] != true || got["work_files"] != 0 {
            failed.push(format!(
                "閉じたあと予算か作業用のファイルが戻りません: {got}"
            ));
        }
        assert!(failed.is_empty(), "{failed:?}");
    }
}
