//! 音声の流れの受け入れテスト（M13 段 1。docs/editing.md §17 の E39・E40）。
//!
//! - streamed_windows_concatenate_to_render：ランダムな音声の文書（量子化を含む）で、`Engine::stream_audio` の窓を連結したものが
//!   `render` の出力と同じ。窓は頭から順・重ならず・チャンクに揃う。区間を頼むと、その区間に掛かるチャンクだけを流す。
//! - streamed_digest_equals_value_digest：`Engine::digest_audio_output` が `render` の出力のダイジェストと同じ。
//! - presolved_measurement_equals_run：流れの解析（`audio.loudness_gating` の流れ）を窓（1・7・4,096・C・ランダム）で解いた結果が
//!   `run` と同じ正規の形。
//! - patcher_equals_compute（E40）は peaks の段の後で足す。

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

fn engine() -> Engine {
    Engine::new().unwrap()
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
    let engine = engine();
    let mut r = determinism::rng(39);
    let (mut checked, mut quantized) = (0, 0);
    for case in 0..30 {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 4) as usize;
        let g = random_audio_chain(&engine, &mut r, &input.shape(), len);
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
    assert!(checked > 20 && quantized > 3, "{checked} {quantized}");
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
        for step in [7usize, 4_096, AUDIO_CHUNK, 0] {
            let mut st = a.stream_any(&params, &v.shape()).unwrap().unwrap();
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
            let got = a.encode_any(&st.finish(&cx).unwrap()).unwrap();
            assert_eq!(got, want, "{i}・窓 {step}");
        }
    }
    // 長さの分からない形は流せない
    let unknown = kura_engine::shape::Shape::Audio {
        rate: 8_000,
        channels: 1,
        frames: kura_engine::shape::Dim::AtMost(10),
    };
    assert!(a.stream_any(&params, &unknown).unwrap().is_none());
}
