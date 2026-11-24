//! 音声の流れの受け入れテスト（M13 段 1。docs/editing.md §17 の E39・E40）。
//!
//! - streamed_windows_concatenate_to_render：ランダムな音声の文書（量子化を含む）で、`Engine::stream_audio` の窓を連結したものが
//!   `render` の出力と同じ。窓は頭から順・重ならず・チャンクに揃う。区間を頼むと、その区間に掛かるチャンクだけを流す。
//! - streamed_digest_equals_value_digest：`Engine::digest_audio_output` が `render` の出力のダイジェストと同じ。
//! - presolved_measurement_equals_run：流れの解析（`audio.loudness_gating` の流れ）を窓（1・7・4,096・C・ランダム）で解いた結果が
//!   `run` と同じ正規の形（長さの決まった形と、長さが上限しか分からない形。M13 段 2）。
//! - patcher_equals_compute（E40）：波形の段の部分更新を流れに載せた `Patcher` が `compute` と同じ。
//! - e35_stream_lengths_are_checked_for_every_analysis（E35。M13 段 2 の検証の直し）：音声の解析の流れのどれも、終わりの長さ・
//!   形の長さ・窓の順・チャンネルの数・上限を照らして `Internal`。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::analysis::AnalyzeCx;
use kura_engine::audio::stream::{AudioSink, StreamInfo};
use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::region::{Area, Spans};
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::testgen::InputGen;
use kura_engine::testing::region::{
    below, random_audio_area, random_audio_chain, random_audio_input,
};
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{Digest, EngineError, Limits, Monitor, NoMonitor, Value, determinism};
use rand_chacha::rand_core::Rng;
use serde_json::json;

const C: u64 = AUDIO_CHUNK as u64;

/// 流れの網のエンジン：今の登録表に、試験の状態を運ぶ操作と状態の表の解析を足したもの（M17 段 1）。
fn engine() -> Engine {
    kura_engine::testing::stateful::engine().unwrap()
}

fn compile(engine: &Engine, g: &Graph, input: &Value) -> Option<Plan> {
    engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .ok()
}

fn render(engine: &Engine, plan: &Plan, input: &Value, source: Digest) -> Option<(Value, Digest)> {
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source,
        },
    )]);
    let mut r = engine.render(plan, &plain, &NoMonitor).ok()?;
    let out = r.outputs.swap_remove(0);
    Some((out.value, out.digest))
}

/// 流れを集める（窓の順・揃い・メタデータを確かめる）。
#[derive(Default)]
struct Collect {
    info: Option<StreamInfo>,
    planes: Vec<Vec<f32>>,
    windows: Vec<(u64, u64)>,
    finished: bool,
}

impl AudioSink for Collect {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        assert!(self.info.is_none(), "begin が 2 度");
        self.planes = vec![Vec::new(); info.channels];
        self.info = Some(info.clone());
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        _mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let n = planes[0].len() as u64;
        if let Some((_, end)) = self.windows.last() {
            assert!(start >= *end, "窓が順でない");
        }
        assert_eq!(start % C, 0, "窓がチャンクに揃わない");
        assert!(n <= 16 * C);
        self.windows.push((start, start + n));
        for (o, p) in self.planes.iter_mut().zip(planes) {
            o.extend_from_slice(p);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        self.finished = true;
        Ok(())
    }
}

/// E39：窓を連結したものが `render` の出力と同じ（量子化を含む）。区間を頼むと、その区間に掛かるチャンクだけを流す。
#[test]
fn streamed_windows_concatenate_to_render() {
    let (checked, quantized, _) = windows_concatenate(39, 30, false);
    assert!(checked > 20 && quantized > 3, "{checked} {quantized}");
}

/// E39（M17 段 1）：試験の状態を運ぶ操作を混ぜたランダムな文書でも、窓を連結したものが `render` の出力と同じ（窓ごとの
/// 範囲の描画は前の値なしで、状態の表は流し解き 1 回）。
#[test]
fn stateful_windows_concatenate_to_render() {
    let (checked, _, with_state) = windows_concatenate(139, 24, true);
    assert!(checked > 16 && with_state > 8, "{checked} {with_state}");
}

/// E39 の本体：（確かめた文書, 量子化した文書, 試験の操作のある文書）。
fn windows_concatenate(seed: u64, cases: usize, stateful: bool) -> (usize, usize, usize) {
    let engine = engine();
    let mut r = determinism::rng(seed);
    let (mut checked, mut quantized, mut with_state) = (0, 0, 0);
    for case in 0..cases {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 4) as usize;
        let g = if stateful {
            kura_engine::testing::region::random_stateful_chain(
                &engine,
                &mut r,
                &input.shape(),
                len,
            )
        } else {
            random_audio_chain(&engine, &mut r, &input.shape(), len)
        };
        with_state += usize::from(
            g.nodes
                .iter()
                .any(|n| n.op == kura_engine::testing::stateful::OP_ID),
        );
        let Some(plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let source = Digest([case as u8; 32]);
        let Some((want, _)) = render(&engine, &plan, &input, source) else {
            continue;
        };
        let Value::Audio(w) = &want else { panic!() };
        let fin = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(input.clone(), source).unwrap(),
        )]);
        let doc = String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned();
        // 全体
        let mut sink = Collect::default();
        let stats = engine
            .stream_audio(
                &plan,
                &fin,
                &mut NodeCache::new([0, 1 << 30][case % 2]),
                OutputId(0),
                None,
                &mut sink,
                &NoMonitor,
            )
            .unwrap_or_else(|e| panic!("{case}: {e}\n{doc}"));
        assert!(sink.finished);
        let info = sink.info.as_ref().unwrap();
        assert_eq!(info.meta, w.meta, "{case}\n{doc}");
        assert_eq!(info.frames, w.frames() as u64);
        let planes = w.to_planes().unwrap();
        for (got, want) in sink.planes.iter().zip(&planes) {
            assert!(
                got.len() == want.len()
                    && got
                        .iter()
                        .zip(want.iter())
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{case}：流れが render と違う\n{doc}"
            );
        }
        assert_eq!(stats.windows as usize, sink.windows.len());
        assert_eq!(stats.area.fallbacks, 0, "{case}\n{doc}");
        // 区間だけ
        let area = random_audio_area(&mut r, w.frames() as u64);
        let spans = match &area {
            Area::Samples(s) => s.clone(),
            _ => Spans::one(0, w.frames() as u64),
        };
        let mut part = Collect::default();
        engine
            .stream_audio(
                &plan,
                &fin,
                &mut NodeCache::new(0),
                OutputId(0),
                Some(&spans),
                &mut part,
                &NoMonitor,
            )
            .unwrap();
        let mut at = 0usize;
        for (s, e) in &part.windows {
            for (ch, p) in part.planes.iter().enumerate() {
                let got = &p[at..at + (e - s) as usize];
                let want = &planes[ch][*s as usize..*e as usize];
                assert!(
                    got.iter()
                        .zip(want)
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "{case}"
                );
            }
            at += (e - s) as usize;
        }
        for (a, b) in spans.spans() {
            let b = (*b).min(w.frames() as u64);
            for i in [*a, b.saturating_sub(1)] {
                if *a < b && i < b {
                    assert!(
                        part.windows.iter().any(|(s, e)| *s <= i && i < *e),
                        "{case}：頼んだ位置 {i} を流していない（{spans:?}・{:?}・{}）",
                        part.windows,
                        w.frames()
                    );
                }
            }
        }
        quantized += usize::from(g.outputs[0].quantize.is_some());
        checked += 1;
    }
    (checked, quantized, with_state)
}

/// M13 段 3（E1）：`Engine::output_stream` の生産者を、全体・窓ごとに後ろから・区間を何度も頼んで流しても、`render` の出力と
/// 同じ標本・メタデータになる。警告は `render` の警告と同じ（計画の順・重複なし）。流し解きは何度流しても 1 回。
#[test]
fn output_stream_reproduces_render_in_any_order() {
    use kura_engine::audio::stream::{AudioProducer, stream_windows};
    let engine = engine();
    let mut r = determinism::rng(1301);
    let (mut checked, mut warned) = (0, 0);
    for case in 0..30 {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 4) as usize;
        let g = random_audio_chain(&engine, &mut r, &input.shape(), len);
        let Some(plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let source = Digest([case as u8; 32]);
        let plain = BTreeMap::from([(
            InputId(0),
            InputValue {
                value: input.clone(),
                source,
            },
        )]);
        let Ok(mut rendered) = engine.render(&plan, &plain, &NoMonitor) else {
            continue;
        };
        let want = rendered.outputs.swap_remove(0).value;
        let Value::Audio(w) = &want else { panic!() };
        let planes = w.to_planes().unwrap();
        let fin = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(input.clone(), source).unwrap(),
        )]);
        let doc = String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned();
        let mut cache = NodeCache::new([0, 1 << 30][case % 2]);
        let mut out = engine
            .output_stream(&plan, &fin, &mut cache, OutputId(0), &NoMonitor)
            .unwrap_or_else(|e| panic!("{case}: {e}\n{doc}"));
        assert_eq!(out.info().meta, w.meta, "{case}\n{doc}");
        assert_eq!(out.info().frames, w.frames() as u64);
        // 全体
        let mut sink = Collect::default();
        out.produce(None, &mut sink, &NoMonitor).unwrap();
        for (got, want) in sink.planes.iter().zip(&planes) {
            assert!(
                got.len() == want.len()
                    && got
                        .iter()
                        .zip(want.iter())
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{case}：流れが render と違う\n{doc}"
            );
        }
        let presolved = out.stats().area.presolved;
        // 窓ごとに後ろから
        let windows = stream_windows(w.frames() as u64, None);
        for (s, e) in windows.iter().rev() {
            let mut one = Collect::default();
            out.produce(Some(&Spans::one(*s, *e)), &mut one, &NoMonitor)
                .unwrap();
            assert_eq!(one.windows, vec![(*s, *e)]);
            assert_eq!(one.info.as_ref().unwrap().meta, w.meta);
            for (got, want) in one.planes.iter().zip(&planes) {
                assert!(
                    got.iter()
                        .zip(&want[*s as usize..*e as usize])
                        .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "{case}\n{doc}"
                );
            }
        }
        assert_eq!(out.stats().area.presolved, presolved, "流し解きは 1 回");
        assert_eq!(
            out.warnings(),
            rendered.warnings.as_slice(),
            "{case}\n{doc}"
        );
        warned += usize::from(!rendered.warnings.is_empty());
        checked += 1;
    }
    assert!(checked > 20, "{checked} {warned}");
}

/// 遅延の値（源の参照のチャンク）にした入力。
fn lazy(v: &Value) -> Value {
    let Value::Audio(a) = v else {
        panic!("音声ではありません")
    };
    Value::Audio(std::sync::Arc::new(
        kura_engine::value::Audio::from_source(
            std::sync::Arc::new(kura_engine::audio::source::MemorySource::new(a.clone()).unwrap()),
            a.meta.clone(),
        )
        .unwrap(),
    ))
}

/// 検証の直し（M13 段 1）：流れの窓（`STREAM_CHUNKS` チャンク）が 2 つ以上になる長さで、窓を連結したもの・流して取った
/// ダイジェスト・窓の境（16C）をまたぐ区間の範囲の描画が `render` と同じ。量子化（窓ごとの `only`）・リサンプルの窓が流れの
/// 窓の境をまたぐもの・窓をまたぐ流し解き（normalize の入力が部分の値）・遅延の入力を通す。決まった文書 3 つと、ランダムな
/// 長い文書（`random_long_audio_input`。デバッグのビルドで重くならないようリサンプルなし・8 kHz）。
#[test]
fn long_streams_cross_windows_and_equal_render() {
    use kura_engine::doc::{Dither, QUANTIZE_VERSION, Quantize};
    use kura_engine::testing::region::{
        check_region, random_long_audio_chain, random_long_audio_input,
    };
    let engine = engine();
    let w = 16 * C;
    let q = |seed| {
        Some(Quantize {
            v: QUANTIZE_VERSION,
            bits: 16,
            dither: Dither::Tpdf,
            seed,
        })
    };
    let noise = |channels: u16, frames: u64, seed: u64| {
        InputGen::AudioNoise {
            rate: 8_000,
            channels,
            frames,
            seed,
        }
        .generate()
        .unwrap()
    };
    // (名前, 入力, 文書, 範囲の描画で流し解きが起きるか)
    let mut cases: Vec<(String, Value, Graph, bool)> = Vec::new();
    {
        // ゲイン → 8k→12k → フェード（出力 27C ほど。リサンプルの窓が流れの窓の境をまたぐ）
        let mut g = Graph::new_single();
        g.push_linear("audio.gain", 1, json!({ "db": -3.0 }), 1)
            .unwrap();
        g.push_linear("audio.resample", 1, json!({ "rate": 12_000 }), 2)
            .unwrap();
        g.push_linear(
            "audio.fade",
            1,
            json!({ "fadeIn": 100, "fadeOut": 2 * C, "curve": "linear" }),
            3,
        )
        .unwrap();
        g.outputs[0].quantize = q(3);
        cases.push(("リサンプル".into(), noise(1, 18 * C + 5, 51), g, false));
        // トリム（揃わない始まり）→ 正規化 → ゲイン（正規化の入力は 3 窓）
        let mut g = Graph::new_single();
        g.push_linear("audio.trim", 1, json!({ "start": 777, "end": null }), 1)
            .unwrap();
        g.push_linear(
            "audio.normalize",
            1,
            json!({ "target": -20.0, "peakLimit": -1.0 }),
            2,
        )
        .unwrap();
        g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 3)
            .unwrap();
        g.outputs[0].quantize = q(4);
        cases.push(("正規化".into(), noise(2, 2 * w + 9, 52), g, true));
        // 正規化 → フェード → 正規化（下流の計測は上流の範囲の描画を窓ごとに流して解く）
        let mut g = Graph::new_single();
        g.push_linear(
            "audio.normalize",
            1,
            json!({ "target": -23.0, "peakLimit": null }),
            1,
        )
        .unwrap();
        g.push_linear(
            "audio.fade",
            1,
            json!({ "fadeIn": 3 * C, "fadeOut": 7, "curve": "equalPower" }),
            2,
        )
        .unwrap();
        g.push_linear(
            "audio.normalize",
            1,
            json!({ "target": -16.0, "peakLimit": -2.0 }),
            3,
        )
        .unwrap();
        cases.push(("正規化 2 つ".into(), noise(1, w + 3 * C + 1, 53), g, true));
    }
    let mut r = determinism::rng(139);
    for k in 0..3 {
        let input = random_long_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 3) as usize;
        let g = random_long_audio_chain(&engine, &mut r, &input.shape(), len);
        cases.push((format!("ランダム {k}"), input, g, false));
    }
    let mut multi = 0;
    for (name, input, g, presolves) in cases {
        let doc = String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned();
        let what = format!("{name}\n{doc}");
        let plan = compile(&engine, &g, &input).unwrap_or_else(|| panic!("{what}"));
        let source = Digest([9; 32]);
        let (want, digest) = render(&engine, &plan, &input, source).unwrap();
        let Value::Audio(wa) = &want else { panic!() };
        let n = wa.frames() as u64;
        let planes = wa.read_planes(0, wa.frames()).unwrap();
        for (resident, cap) in [(true, 0u64), (false, 1 << 30)] {
            let value = if resident {
                input.clone()
            } else {
                lazy(&input)
            };
            let fin =
                BTreeMap::from([(InputId(0), FingerprintedInput::new(value, source).unwrap())]);
            let mut sink = Collect::default();
            let stats = engine
                .stream_audio(
                    &plan,
                    &fin,
                    &mut NodeCache::new(cap),
                    OutputId(0),
                    None,
                    &mut sink,
                    &NoMonitor,
                )
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(stats.windows, n.div_ceil(w), "{what}");
            assert_eq!(stats.area.fallbacks, 0, "{what}");
            for (got, want) in sink.planes.iter().zip(&planes) {
                assert!(
                    got.len() == want.len()
                        && got
                            .iter()
                            .zip(want.iter())
                            .all(|(a, b)| a.to_bits() == b.to_bits()),
                    "{what}：流れが render と違う（常駐 {resident}）"
                );
            }
            multi += usize::from(stats.windows >= 2);
            if resident {
                let got = engine
                    .digest_audio_output(
                        &plan,
                        &fin,
                        &mut NodeCache::new(cap),
                        OutputId(0),
                        &NoMonitor,
                    )
                    .unwrap();
                assert_eq!(got, digest, "{what}");
            }
            // 窓の境をまたぐ区間と終わり
            let area = Area::span(w - 3, w + 3).union(&Area::span(n.saturating_sub(2), n));
            let got = engine
                .render_area(
                    &plan,
                    &fin,
                    &mut NodeCache::new(cap),
                    &kura_engine::render::area::AreaRequest {
                        output: OutputId(0),
                        area: area.clone(),
                    },
                    None,
                    &NoMonitor,
                )
                .unwrap();
            check_region(&got.value, &want, &area)
                .unwrap_or_else(|m| panic!("{what}（常駐 {resident}）: {m}"));
            assert_eq!(got.stats.fallbacks, 0, "{what}");
            if presolves {
                assert!(got.stats.presolved >= 1, "{what}: {:?}", got.stats);
            }
        }
    }
    assert!(multi >= 10, "窓が 2 つ以上の流れ {multi}");
}

/// E39：流して取ったダイジェストは `render` の出力のダイジェストと同じ（量子化・長い音声のチャンクの境を含む）。
#[test]
fn streamed_digest_equals_value_digest() {
    let engine = engine();
    let mut r = determinism::rng(40);
    let mut checked = 0;
    for case in 0..16 {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = below(&mut r, 4) as usize;
        let g = random_audio_chain(&engine, &mut r, &input.shape(), len);
        let Some(plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let source = Digest([3; 32]);
        let Some((_, want)) = render(&engine, &plan, &input, source) else {
            continue;
        };
        let fin = BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(input.clone(), source).unwrap(),
        )]);
        let got = engine
            .digest_audio_output(
                &plan,
                &fin,
                &mut NodeCache::new(1 << 20),
                OutputId(0),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(got, want, "{case}");
        checked += 1;
    }
    assert!(checked > 10, "{checked}");
}

/// E39：流れの解析（`audio.loudness_gating`）を窓（1・7・4,096・C・ランダム）で解いた結果は、`run` と同じ正規の形。
#[test]
fn presolved_measurement_equals_run() {
    let engine = engine();
    let a = engine.analyses().get("audio.loudness_gating", 1).unwrap();
    let params = json!({});
    let limits = Limits::default();
    let cx = AnalyzeCx {
        mon: &NoMonitor,
        limits: &limits,
    };
    let mut r = determinism::rng(41);
    for (i, make) in [
        InputGen::AudioNoise {
            rate: 48_000,
            channels: 2,
            frames: 2 * C + 37,
            seed: 1,
        },
        InputGen::SineLevels {
            rate: 8_000,
            freq: 1_000,
            segments: vec![(16_000, -66), (64_000, -73)],
        },
        InputGen::AudioNoise {
            rate: 44_100,
            channels: 1,
            frames: 4_000,
            seed: 2,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let v = make.generate().unwrap();
        let Value::Audio(au) = &v else { panic!() };
        let want = a.encode_any(&a.run_any(&params, &v, &cx).unwrap()).unwrap();
        let planes = au.to_planes().unwrap();
        let n = au.frames();
        let end = kura_engine::audio::stream::StreamEnd {
            frames: n as u64,
            meta: au.meta.clone(),
        };
        // 長さの決まった形と、長さが上限しか分からない形（M13 段 2）
        let bounded = kura_engine::shape::Shape::Audio {
            rate: au.rate(),
            channels: au.channels() as u16,
            frames: kura_engine::shape::Dim::AtMost(n as u64 + 4_097),
        };
        for (shape, step) in [7usize, 4_096, AUDIO_CHUNK, 0]
            .into_iter()
            .flat_map(|step| [(v.shape(), step), (bounded, step)])
        {
            let mut st = a.stream_any(&params, &shape).unwrap().unwrap();
            let mut at = 0;
            while at < n {
                let len = if step == 0 {
                    1 + (r.next_u64() % 20_000) as usize
                } else {
                    step
                };
                let end = (at + len).min(n);
                let views: Vec<&[f32]> = planes.iter().map(|p| &p[at..end]).collect();
                st.feed(at as u64, &views, &cx).unwrap();
                at = end;
            }
            let got = a.encode_any(&st.finish(&end, &cx).unwrap()).unwrap();
            assert_eq!(got, want, "{i}・窓 {step}・{shape:?}");
        }
    }
    // 終わりの長さが受けた長さ・形と違えば Internal
    let v = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 100,
        seed: 3,
    }
    .generate()
    .unwrap();
    let Value::Audio(au) = &v else { panic!() };
    let planes = au.to_planes().unwrap();
    let views: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
    let end = |frames| kura_engine::audio::stream::StreamEnd {
        frames,
        meta: au.meta.clone(),
    };
    for (frames, bound, ok) in [
        (kura_engine::shape::Dim::Exact(100), 100, true),
        (kura_engine::shape::Dim::Exact(100), 99, false),
        (kura_engine::shape::Dim::AtMost(150), 100, true),
        (kura_engine::shape::Dim::AtMost(150), 101, false),
    ] {
        let shape = kura_engine::shape::Shape::Audio {
            rate: 8_000,
            channels: 1,
            frames,
        };
        let mut st = a.stream_any(&params, &shape).unwrap().unwrap();
        st.feed(0, &views, &cx).unwrap();
        let got = st.finish(&end(bound), &cx);
        assert_eq!(got.is_ok(), ok, "{frames:?}・{bound}");
        if !ok {
            assert!(matches!(got, Err(EngineError::Internal(_))));
        }
    }
    // 上限を超えて受けたら Internal
    let mut st = a
        .stream_any(
            &params,
            &kura_engine::shape::Shape::Audio {
                rate: 8_000,
                channels: 1,
                frames: kura_engine::shape::Dim::AtMost(50),
            },
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        st.feed(0, &views, &cx),
        Err(EngineError::Internal(_))
    ));
}

/// E40：波形の段の部分更新を流れに載せた `Patcher` は、変わった区間（推論）のチャンクだけを流して、新しい出力を `compute`
/// したものと同じ段を組む。フェードの長さ・ゲインの手と、`finest`／`coarsest` の組で。
#[test]
fn patcher_equals_compute() {
    use kura_engine::analysis::audio::peaks::{Patcher, PeaksParams, compute};
    let engine = engine();
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 3 * C + 37,
        seed: 40,
    }
    .generate()
    .unwrap();
    let source = Digest::default();
    let fin = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), source).unwrap(),
    )]);
    let limits = Limits::default();
    let cx = AnalyzeCx {
        mon: &NoMonitor,
        limits: &limits,
    };
    let doc = |fade_in: u64, db: f64| {
        let mut g = Graph::new_single();
        g.push_linear(
            "audio.fade",
            1,
            json!({ "fadeIn": fade_in, "fadeOut": 2_000, "curve": "linear" }),
            1,
        )
        .unwrap();
        g.push_linear("audio.gain", 1, json!({ "db": db }), 2)
            .unwrap();
        g
    };
    let mut streamed_less = 0;
    for (before, after) in [((1_000, -1.0), (C + 10, -1.0)), ((500, -1.0), (500, -3.0))] {
        let (a, b) = (
            compile(&engine, &doc(before.0, before.1), &input).unwrap(),
            compile(&engine, &doc(after.0, after.1), &input).unwrap(),
        );
        let (Some((old, _)), Some((new, _))) = (
            render(&engine, &a, &input, source),
            render(&engine, &b, &input, source),
        ) else {
            panic!()
        };
        let (Value::Audio(old), Value::Audio(new)) = (&old, &new) else {
            panic!()
        };
        let changed = match &engine.delta(&a, &fin, &b, &fin).unwrap().outputs[0].1 {
            Area::Samples(s) => s.clone(),
            _ => Spans::one(0, new.frames() as u64),
        };
        for p in [
            PeaksParams::default(),
            PeaksParams {
                finest: 4_096,
                coarsest: 16_384,
            },
        ] {
            let old_peaks = compute(old, &p, &cx).unwrap();
            let mut patcher = Patcher::new(old_peaks, p, new.frames() as u64, &changed);
            let spans = patcher.spans();
            let stats = engine
                .stream_audio(
                    &b,
                    &fin,
                    &mut NodeCache::new(0),
                    OutputId(0),
                    Some(&spans),
                    &mut patcher,
                    &NoMonitor,
                )
                .unwrap();
            assert_eq!(
                patcher.into_peaks().unwrap(),
                compute(new, &p, &cx).unwrap(),
                "{before:?} → {after:?}・{p:?}"
            );
            streamed_less += usize::from(stats.area.drawn_chunks < 4 * 2);
        }
    }
    // フェードの長さの手は全体を流さない
    assert!(streamed_less >= 2, "{streamed_less}");
    // 長さだけが変わる手（トリムの終わり。縮める・伸ばす）：尻のチャンクだけを流す
    let trim = |end: u64| {
        let mut g = Graph::new_single();
        g.push_linear("audio.trim", 1, json!({ "start": 10, "end": end }), 1)
            .unwrap();
        g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 2)
            .unwrap();
        g
    };
    for (e1, e2) in [(3 * C + 30, 2 * C + 99), (C + 5, 3 * C)] {
        let (a, b) = (
            compile(&engine, &trim(e1), &input).unwrap(),
            compile(&engine, &trim(e2), &input).unwrap(),
        );
        let (Some((old, _)), Some((new, _))) = (
            render(&engine, &a, &input, source),
            render(&engine, &b, &input, source),
        ) else {
            panic!()
        };
        let (Value::Audio(old), Value::Audio(new)) = (&old, &new) else {
            panic!()
        };
        let Area::Samples(changed) = engine.delta(&a, &fin, &b, &fin).unwrap().outputs[0]
            .1
            .clone()
        else {
            panic!("長さだけの手が全体になった");
        };
        let p = PeaksParams::default();
        let mut patcher = Patcher::new(
            compute(old, &p, &cx).unwrap(),
            p,
            new.frames() as u64,
            &changed,
        );
        let spans = patcher.spans();
        // 縮めるときは頭のチャンクを流さない（伸ばすときは、前の最後の半端なチャンクから先を流す）
        let first_full = (e1.min(e2) - 10) / C * C;
        assert!(
            spans.spans()[0].0 >= first_full.min(C),
            "{e1} → {e2}：{spans:?}"
        );
        engine
            .stream_audio(
                &b,
                &fin,
                &mut NodeCache::new(0),
                OutputId(0),
                Some(&spans),
                &mut patcher,
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(
            patcher.into_peaks().unwrap(),
            compute(new, &p, &cx).unwrap(),
            "{e1} → {e2}"
        );
    }
}

/// 音声を受ける今の版の解析（内部用を含む）。
fn audio_analyses(engine: &Engine) -> Vec<std::sync::Arc<dyn kura_engine::analysis::DynAnalysis>> {
    let reg = engine.analyses();
    let ids = [
        "audio.peaks",
        "audio.loudness",
        "audio.silence",
        "audio.loop",
        "audio.loudness_gating",
        // M17 段 1：状態の表（試験の操作のもの。内部用でカタログに出ない）
        kura_engine::testing::stateful::STATE_ID,
    ];
    let all: Vec<_> = ids
        .iter()
        .map(|id| reg.resolve(id, None).unwrap())
        .collect();
    // 登録表の音声の解析はこれで全部（足したら E35 にも足す）
    let catalog: Vec<&str> = engine
        .analysis_catalog()
        .iter()
        .filter(|d| d.accepts.contains(&kura_engine::ValueKind::Audio))
        .map(|d| d.id)
        .collect();
    for id in &catalog {
        assert!(ids.contains(id), "{id} を E35 に足す");
    }
    all
}

/// E35（M13 段 2）：表示の 4 つと `loudness_gating` のすべての例（チャンクの境をまたぐ・NaN・空・低いレートを含む）で、
/// 長さの決まった形と長さの上限の形（`AtMost(frames + k)`）について、窓を 1・7・4,096・C・全体・ChaCha8 の乱数で切って流した
/// `finish` の正規の形が `run` と同じ。
#[test]
fn e35_streamed_analyses_equal_run() {
    let engine = engine();
    let limits = Limits::default();
    let cx = AnalyzeCx {
        mon: &NoMonitor,
        limits: &limits,
    };
    let mut r = determinism::rng(35);
    let mut checked = 0;
    for a in audio_analyses(&engine) {
        for ex in a.examples().unwrap() {
            let v = ex.input.generate().unwrap();
            let Value::Audio(au) = &v else { panic!() };
            let want = a
                .encode_any(&a.run_any(&ex.params, &v, &cx).unwrap())
                .unwrap();
            let n = au.frames();
            let planes = au.read_planes(0, n).unwrap();
            let end = kura_engine::audio::stream::StreamEnd {
                frames: n as u64,
                meta: au.meta.clone(),
            };
            let shapes = [
                v.shape(),
                kura_engine::shape::Shape::Audio {
                    rate: au.rate(),
                    channels: au.channels() as u16,
                    frames: kura_engine::shape::Dim::AtMost(n as u64 + 1 + r.next_u64() % 70_000),
                },
            ];
            for shape in shapes {
                for step in [1usize, 7, 4_096, AUDIO_CHUNK, usize::MAX, 0] {
                    // 細かい窓は短い音声だけ（窓ごとに並列の仕事を立てるので、デバッグのビルドで重い）
                    if (step == 1 && n > 3_000) || (step == 7 && n > 30_000) {
                        continue;
                    }
                    let mut st = a.stream_any(&ex.params, &shape).unwrap().unwrap();
                    let mut at = 0;
                    while at < n {
                        let len = match step {
                            0 => 1 + (r.next_u64() % 50_000) as usize,
                            s => s,
                        };
                        let e = at.saturating_add(len).min(n);
                        let views: Vec<&[f32]> = planes.iter().map(|p| &p[at..e]).collect();
                        st.feed(at as u64, &views, &cx).unwrap();
                        at = e;
                    }
                    let got = a.encode_any(&st.finish(&end, &cx).unwrap()).unwrap();
                    assert!(got == want, "{}・{}・窓 {step}・{shape:?}", a.id(), ex.name);
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 150, "{checked}");
}

/// E35：どの音声の解析も、長さの決まった形と上限の形の両方で流れを返す。
#[test]
fn audio_analyses_all_stream() {
    let engine = engine();
    for a in audio_analyses(&engine) {
        for ex in a.examples().unwrap() {
            let shape = ex.input.generate().unwrap().shape();
            let kura_engine::shape::Shape::Audio {
                rate,
                channels,
                frames,
            } = shape
            else {
                panic!()
            };
            for f in [frames, kura_engine::shape::Dim::AtMost(frames.max() + 10)] {
                let s = kura_engine::shape::Shape::Audio {
                    rate,
                    channels,
                    frames: f,
                };
                assert!(
                    a.stream_any(&ex.params, &s).unwrap().is_some(),
                    "{}・{}・{f:?}",
                    a.id(),
                    ex.name
                );
            }
        }
    }
}

/// E35（M13 段 2 の検証の直し）：音声の解析の流れはどれも、受けた長さと終わりの長さ・形の長さを照らす。長さの決まった形で
/// 終わりの長さが受けた長さ・形と違う、上限の形で終わりの長さが受けた長さと違う、重なる窓・飛んだ窓・チャンネルの数の
/// 違う窓・上限を超えて受けるものは、どれも `Internal`（短い・長い結果を黙って作らない）。
#[test]
fn e35_stream_lengths_are_checked_for_every_analysis() {
    use kura_engine::audio::stream::StreamEnd;
    use kura_engine::shape::{Dim, Shape};
    let engine = engine();
    let limits = Limits::default();
    let cx = AnalyzeCx {
        mon: &NoMonitor,
        limits: &limits,
    };
    let v = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 100,
        seed: 3,
    }
    .generate()
    .unwrap();
    let Value::Audio(au) = &v else { panic!() };
    let planes = au.read_planes(0, 100).unwrap();
    let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
    let end = |frames| StreamEnd {
        frames,
        meta: au.meta.clone(),
    };
    let shape = |frames| Shape::Audio {
        rate: 8_000,
        channels: 2,
        frames,
    };
    for a in audio_analyses(&engine) {
        let id = a.id();
        let p = a.check_params(&json!({})).unwrap();
        // (形, 流す長さ, 終わりの長さ, 通るか)
        for (frames, fed, at_end, ok) in [
            (Dim::Exact(100), 100usize, 100u64, true),
            (Dim::Exact(100), 100, 99, false),
            (Dim::Exact(100), 100, 101, false),
            (Dim::Exact(100), 60, 60, false),
            (Dim::Exact(100), 60, 100, false),
            (Dim::AtMost(150), 100, 100, true),
            (Dim::AtMost(150), 100, 101, false),
            (Dim::AtMost(150), 100, 99, false),
            (Dim::AtMost(100), 100, 100, true),
            (Dim::AtMost(150), 0, 0, true),
        ] {
            let mut st = a.stream_any(&p, &shape(frames)).unwrap().unwrap();
            let part: Vec<&[f32]> = views.iter().map(|x| &x[..fed]).collect();
            st.feed(0, &part, &cx).unwrap();
            let got = st.finish(&end(at_end), &cx);
            assert_eq!(got.is_ok(), ok, "{id}・{frames:?}・{fed}・{at_end}");
            if !ok {
                assert!(
                    matches!(got, Err(EngineError::Internal(_))),
                    "{id}・{frames:?}・{fed}・{at_end}"
                );
            }
        }
        let bounded = shape(Dim::AtMost(150));
        let head: Vec<&[f32]> = views.iter().map(|x| &x[..10]).collect();
        // 重なる窓
        let mut st = a.stream_any(&p, &bounded).unwrap().unwrap();
        st.feed(0, &head, &cx).unwrap();
        assert!(
            matches!(st.feed(5, &head, &cx), Err(EngineError::Internal(_))),
            "{id}：重なる窓"
        );
        // 飛んだ窓
        let mut st = a.stream_any(&p, &bounded).unwrap().unwrap();
        assert!(
            matches!(st.feed(1, &head, &cx), Err(EngineError::Internal(_))),
            "{id}：飛んだ窓"
        );
        // チャンネルの数の違う窓
        let mut st = a.stream_any(&p, &bounded).unwrap().unwrap();
        assert!(
            matches!(st.feed(0, &head[..1], &cx), Err(EngineError::Internal(_))),
            "{id}：チャンネルの数"
        );
        // 上限を超えて受ける
        let mut st = a.stream_any(&p, &shape(Dim::AtMost(50))).unwrap().unwrap();
        assert!(
            matches!(st.feed(0, &views, &cx), Err(EngineError::Internal(_))),
            "{id}：上限を超える"
        );
    }
}

/// E35：解析の扇（`Engine::analyze_stream`）に値を流した結果は、値に `analyze_many` したものと同じ（要求の順・引数の正規の形・
/// 1 つの失敗はほかに効かない）。上限の形で受けて終わりの長さを渡しても同じ。
#[test]
fn fan_over_values_equals_analyze_many() {
    use kura_engine::AnalysisRequest;
    let engine = engine();
    let reqs = vec![
        AnalysisRequest {
            id: "audio.loudness".into(),
            v: None,
            params: json!({}),
        },
        AnalysisRequest {
            id: "audio.loop".into(),
            v: Some(1),
            params: json!({}),
        },
        AnalysisRequest {
            id: "audio.silence".into(),
            v: None,
            params: json!({ "thresholdDb": -50 }),
        },
        AnalysisRequest {
            id: "audio.peaks".into(),
            v: None,
            params: json!({ "finest": 1024 }),
        },
        // 断る要求（知らない ID・引数の範囲の外・画像の解析）
        AnalysisRequest {
            id: "audio.nothing".into(),
            v: None,
            params: json!({}),
        },
        AnalysisRequest {
            id: "audio.silence".into(),
            v: None,
            params: json!({ "thresholdDb": 5 }),
        },
        AnalysisRequest {
            id: "image.alpha".into(),
            v: None,
            params: json!({}),
        },
    ];
    for (i, make) in [
        InputGen::AudioNoise {
            rate: 44_100,
            channels: 2,
            frames: 2 * C + 37,
            seed: 7,
        },
        InputGen::Sine {
            rate: 22_050,
            channels: 1,
            frames: 1_000,
            freq: 440,
            loop_range: Some(kura_engine::value::LoopRange {
                start: 100,
                end: 900,
            }),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let v = make.generate().unwrap();
        let Value::Audio(au) = &v else { panic!() };
        let want = engine.analyze_many(&v, &reqs, &NoMonitor);
        for bounded in [false, true] {
            let shape = if bounded {
                kura_engine::shape::Shape::Audio {
                    rate: au.rate(),
                    channels: au.channels() as u16,
                    frames: kura_engine::shape::Dim::AtMost(au.frames() as u64 + 500),
                }
            } else {
                v.shape()
            };
            let mut fan = engine.analyze_stream(&reqs, &shape);
            let mut sink = FanSink(&mut fan);
            kura_engine::audio::stream::stream_value(au, None, &mut sink, &NoMonitor).unwrap();
            fan.set_end(&kura_engine::audio::stream::StreamEnd {
                frames: au.frames() as u64,
                meta: au.meta.clone(),
            });
            let got = fan.finish(&NoMonitor);
            assert_eq!(got.len(), want.len());
            for (k, (g, w)) in got.iter().zip(&want).enumerate() {
                match (g, w) {
                    (Ok(g), Ok(w)) => {
                        assert_eq!(g.output, w.output, "{i}・{k}・{bounded}");
                        assert_eq!(g.params, w.params, "{i}・{k}");
                        assert_eq!((g.id, g.version), (w.id, w.version));
                    }
                    (Err(g), Err(w)) => {
                        assert_eq!(
                            std::mem::discriminant(g),
                            std::mem::discriminant(w),
                            "{i}・{k}：{g} と {w}"
                        );
                    }
                    _ => panic!("{i}・{k}・{bounded}：{g:?} と {w:?}"),
                }
            }
        }
    }
}

/// 値の流れを扇に渡す包み。
struct FanSink<'a>(&'a mut kura_engine::analysis::stream::AnalysisFan);

impl AudioSink for FanSink<'_> {
    fn begin(&mut self, _info: &StreamInfo) -> Result<(), EngineError> {
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.0.feed(start, planes, mon)
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}
