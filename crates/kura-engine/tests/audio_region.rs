//! 音声の範囲の描画の受け入れテスト（M13 段 1。docs/editing.md §17 の E37）。
//!
//! - audio_region_equals_full_on_random_documents：ランダムな音声の文書・ランダムな区間（1 フレーム・チャンクの境 ±1・終わり・
//!   2 つの区間）で、`render_area` の有効なチャンクが `render` と同じ。入力は常駐と遅延（源の参照）の両方。依存の外を読んで
//!   全体に落ちず（`fallbacks` 0）、警告は `render` の部分列。量子化した出力も同じ。
//! - audio_reach_is_sound：`audio_reach` の逆写しの外の入力を壊しても（無効なチャンクにしても）、区間の中は同じ。
//! - audio_region_declarations_match：音声を出す操作は、例で `audio_reach` が `Global` でない。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::audio::source::MemorySource;
use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::op::{ApplyCx, DynOp};
use kura_engine::render::area::AreaRequest;
use kura_engine::render::audio::chunks_covering;
use kura_engine::render::cache::{Fingerprint, FingerprintedInput, NodeCache};
use kura_engine::render::region::{Area, Reach};
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::shape::Shape;
use kura_engine::testgen::InputGen;
use kura_engine::testing::region::{
    below, check_region, corrupt_outside, only_chunks, random_audio_area, random_audio_chain,
    random_audio_input,
};
use kura_engine::value::{AUDIO_CHUNK, Audio, ChunkSet};
use kura_engine::{Digest, Issue, Limits, NoMonitor, Value, determinism};
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

/// 遅延の値（源の参照のチャンク）にした入力。
fn lazy(v: &Value) -> Value {
    let Value::Audio(a) = v else {
        panic!("音声ではありません")
    };
    Value::Audio(Arc::new(
        Audio::from_source(
            Arc::new(MemorySource::new(a.clone()).unwrap()),
            a.meta.clone(),
        )
        .unwrap(),
    ))
}

fn fingerprinted(v: &Value, source: Digest) -> BTreeMap<InputId, FingerprintedInput> {
    let Value::Audio(_) = v else {
        panic!("音声ではありません")
    };
    BTreeMap::from([(
        InputId(0),
        FingerprintedInput {
            value: v.clone(),
            source,
            fingerprint: Fingerprint(Digest([9; 32])),
        },
    )])
}

fn render(
    engine: &Engine,
    plan: &Plan,
    input: &Value,
    source: Digest,
) -> Option<(Value, Vec<Issue>)> {
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source,
        },
    )]);
    let mut r = engine.render(plan, &plain, &NoMonitor).ok()?;
    Some((r.outputs.swap_remove(0).value, r.warnings))
}

fn frames_of(v: &Value) -> u64 {
    match v {
        Value::Audio(a) => a.frames() as u64,
        _ => 0,
    }
}

/// E37：ランダムな音声の文書とランダムな区間で、範囲の描画の有効なチャンクは全体の描画と同じ。
#[test]
fn audio_region_equals_full_on_random_documents() {
    let engine = engine();
    let mut r = determinism::rng(37);
    let (mut checked, mut drawn, mut quantized, mut shared, mut lazy_runs) = (0, 0u64, 0, 0u64, 0);
    for case in 0..60 {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 5) as usize;
        let g = random_audio_chain(&engine, &mut r, &input.shape(), len);
        let Some(plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let source = Digest([case as u8; 32]);
        let Some((want, warnings)) = render(&engine, &plan, &input, source) else {
            continue;
        };
        let doc = String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned();
        for round in 0..3 {
            let resident = round != 1;
            let value = if resident {
                input.clone()
            } else {
                lazy(&input)
            };
            let fin = fingerprinted(&value, source);
            let mut cache = NodeCache::new([0, 1 << 20, 1 << 30][(case + round) % 3]);
            let area = random_audio_area(&mut r, frames_of(&want));
            let got = engine
                .render_area(
                    &plan,
                    &fin,
                    &mut cache,
                    &AreaRequest {
                        output: OutputId(0),
                        area: area.clone(),
                    },
                    None,
                    &NoMonitor,
                )
                .unwrap_or_else(|e| panic!("{case}: {e}\n{doc}"));
            let what = format!("{case}・{round}（{area:?}）\n{doc}");
            assert_eq!(
                (got.stats.fallbacks, got.stats.rebased),
                (0, 0),
                "{what}：依存の外を読んだ"
            );
            check_region(&got.value, &want, &area).unwrap_or_else(|m| panic!("{what}: {m}"));
            assert!(got.value.covers(&area), "{what}");
            let mut it = warnings.iter();
            assert!(
                got.warnings.iter().all(|w| it.any(|x| x == w)),
                "{what}: 警告 {:?} が {warnings:?} の部分列でない",
                got.warnings
            );
            if got.value.exact().is_none() {
                assert!(got.value.audio_valid().is_some());
            }
            drawn += got.stats.drawn_chunks;
            shared += got.stats.shared_chunks;
            quantized += usize::from(g.outputs[0].quantize.is_some());
            lazy_runs += usize::from(!resident);
            checked += 1;
        }
    }
    assert!(
        checked > 120 && drawn > 100 && quantized > 20 && shared > 0 && lazy_runs > 40,
        "{checked} {drawn} {quantized} {shared} {lazy_runs}"
    );
}

/// 音声を出す操作の例と、チャンクの境をまたぐ追加の場合（トリムのチャンクに揃う始まり・揃わない始まり、リサンプルの窓）。
fn audio_cases(engine: &Engine) -> Vec<(Arc<dyn DynOp>, String, serde_json::Value, Value)> {
    let mut out = Vec::new();
    for op in engine.registry().current() {
        for ex in op.examples().unwrap() {
            let input = ex.inputs[0].generate().unwrap();
            if matches!(input, Value::Audio(_)) {
                out.push((op.clone(), ex.name.to_string(), ex.params, input));
            }
        }
    }
    let noise = |frames, channels, seed| {
        InputGen::AudioNoise {
            rate: 8_000,
            channels,
            frames,
            seed,
        }
        .generate()
        .unwrap()
    };
    let extra: Vec<(&str, serde_json::Value, Value)> = vec![
        (
            "audio.trim",
            json!({ "start": C, "end": 3 * C + 5 }),
            noise(3 * C + 9, 2, 1),
        ),
        (
            "audio.trim",
            json!({ "start": C - 1, "end": null }),
            noise(3 * C, 1, 2),
        ),
        (
            "audio.resample",
            json!({ "rate": 6_000 }),
            noise(3 * C, 1, 3),
        ),
        (
            "audio.resample",
            json!({ "rate": 16_000 }),
            noise(2 * C + 37, 2, 4),
        ),
        (
            "audio.fade",
            json!({ "fadeIn": C + 3, "fadeOut": C - 3, "curve": "linear" }),
            noise(3 * C, 2, 5),
        ),
        (
            "audio.normalize",
            json!({ "target": -18.0, "peakLimit": -1.0 }),
            noise(3 * C - 1, 2, 6),
        ),
    ];
    for (id, params, input) in extra {
        let v = engine.registry().current_version(id).unwrap();
        let op = engine
            .registry()
            .all()
            .into_iter()
            .find(|o| o.id() == id && o.version() == v)
            .unwrap();
        out.push((op, format!("境界 {id}"), params, input));
    }
    out
}

/// 核を作る（申告した解析は全体の入力 `full` で解いて、`input` に先に入れる）。
fn kernel_of(
    op: &dyn DynOp,
    params: &serde_json::Value,
    input: &Value,
    full: &Value,
) -> Box<dyn kura_engine::audio::AudioKernel> {
    let p = op.parse(params).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
    let engine = engine();
    for need in op.needs(p.as_ref(), &[input.shape()]).unwrap() {
        let a = engine.analyses().get(need.id, need.v).unwrap();
        let acx = kura_engine::analysis::AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        let out = a.run_any(&need.params, full, &acx).unwrap();
        cx.preload(&need, input, out).unwrap();
    }
    op.audio_kernel(p.as_ref(), &[input], &mut cx)
        .unwrap()
        .unwrap()
}

/// E37：`audio_reach` の逆写しの外の入力を壊しても（乱数に替える・無効なチャンクにする）、核が描く出力のチャンクは同じで、
/// 無効なチャンクを読まない。出力のチャンクの区間を 1 つずつ・2 つまとめて・ランダムな区間で。
#[test]
fn audio_reach_is_sound() {
    let engine = engine();
    let mut r = determinism::rng(38);
    let mut checked = 0;
    for (op, name, params, input) in audio_cases(&engine) {
        let p = op.parse(&params).unwrap();
        let reach = op.audio_reach(p.as_ref(), &[input.shape()]).unwrap();
        let full = kernel_of(op.as_ref(), &params, &input, &input);
        let frames = full.frames();
        let out_shape = Shape::Audio {
            rate: full.rate(),
            channels: full.channels() as u16,
            frames: kura_engine::shape::Dim::Exact(frames),
        };
        let count = frames.div_ceil(C);
        let mut areas: Vec<Area> = (0..count)
            .map(|k| Area::span(k * C, ((k + 1) * C).min(frames)))
            .collect();
        if count > 1 {
            areas.push(Area::span(C - 1, C + 1));
        }
        areas.push(random_audio_area(&mut r, frames));
        for area in areas {
            let rounded = kura_engine::render::audio::chunks_covering(&area, frames as usize);
            let out_area = Area::samples(kura_engine::render::audio::chunk_spans(
                &rounded,
                frames as usize,
            ));
            let in_need = reach.backward_in(&out_area, &out_shape);
            let what = format!("{} {name} {area:?} → {in_need:?}", op.id());
            for broken in [
                corrupt_outside(&input, &in_need, r.next_u64()).unwrap(),
                only_chunks(&input, &in_need),
            ] {
                let k = kernel_of(op.as_ref(), &params, &broken, &input);
                for ch in 0..k.channels() {
                    for j in rounded.iter() {
                        let s = j as u64 * C;
                        let e = (s + C).min(frames);
                        let mut got = vec![0f32; (e - s) as usize];
                        let mut want = vec![0f32; (e - s) as usize];
                        k.render(ch, s, &mut got)
                            .unwrap_or_else(|e| panic!("{what}: {e}"));
                        full.render(ch, s, &mut want).unwrap();
                        assert!(
                            got.iter()
                                .zip(&want)
                                .all(|(a, b)| a.to_bits() == b.to_bits()),
                            "{what}：チャンク {j}・チャンネル {ch} が違う"
                        );
                    }
                }
                if let Value::Audio(a) = &broken {
                    assert!(!a.holes_read(), "{what}：無効なチャンクを読んだ");
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 60, "{checked}");
}

use rand_chacha::rand_core::Rng;

/// E37：音声を出す操作は、長さの分かる例で `audio_reach` が `Global` でない（範囲の描画に載る）。normalize は `reach` が
/// 全体で `audio_reach` が同じ位置。
#[test]
fn audio_region_declarations_match() {
    let engine = engine();
    let mut seen = std::collections::BTreeSet::new();
    for (op, name, params, input) in audio_cases(&engine) {
        let p = op.parse(&params).unwrap();
        let reach = op.audio_reach(p.as_ref(), &[input.shape()]).unwrap();
        assert_ne!(reach, Reach::Global, "{} {name}", op.id());
        seen.insert(op.id());
        if op.id() == "audio.normalize" {
            assert_eq!(
                op.reach(p.as_ref(), &[input.shape()]).unwrap(),
                Reach::Global
            );
            assert_eq!(reach, Reach::SameTime);
        }
    }
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        [
            "audio.fade",
            "audio.gain",
            "audio.mono",
            "audio.normalize",
            "audio.resample",
            "audio.set_loop",
            "audio.trim",
        ]
    );
}

/// E37：量子化した出力（TPDF・ディザなし）の範囲の描画は、`render` の量子化した出力と同じチャンクを持つ（量子化は描いた
/// チャンクだけに当てる。乱数は絶対位置）。
#[test]
fn quantized_audio_regions_match_render() {
    use kura_engine::doc::{Dither, QUANTIZE_VERSION, Quantize};
    let engine = engine();
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 3 * C + 11,
        seed: 41,
    }
    .generate()
    .unwrap();
    let source = Digest([4; 32]);
    let mut checked = 0;
    for (dither, bits) in [(Dither::Tpdf, 16), (Dither::None, 8), (Dither::Tpdf, 24)] {
        let mut g = Graph::new_single();
        g.push_linear("audio.gain", 1, json!({ "db": -2.5 }), 1)
            .unwrap();
        g.push_linear("audio.trim", 1, json!({ "start": C + 5, "end": null }), 2)
            .unwrap();
        g.outputs[0].quantize = Some(Quantize {
            v: QUANTIZE_VERSION,
            bits,
            dither,
            seed: 9,
        });
        let plan = compile(&engine, &g, &input).unwrap();
        let (want, _) = render(&engine, &plan, &input, source).unwrap();
        for area in [
            Area::span(0, 1),
            Area::span(C - 1, C + 1),
            Area::span(frames_of(&want) - 3, frames_of(&want)),
            Area::span(10, 20).union(&Area::span(C + 7, C + 9)),
        ] {
            for cap in [0, 1 << 30] {
                let got = engine
                    .render_area(
                        &plan,
                        &fingerprinted(&input, source),
                        &mut NodeCache::new(cap),
                        &AreaRequest {
                            output: OutputId(0),
                            area: area.clone(),
                        },
                        None,
                        &NoMonitor,
                    )
                    .unwrap();
                check_region(&got.value, &want, &area)
                    .unwrap_or_else(|m| panic!("{dither:?} {bits} {area:?}: {m}"));
                assert!(got.value.exact().is_none(), "部分の値は Exact にならない");
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 24);
}

/// 範囲の描画の値は、頼んだ区間のチャンクと、その読みの口（配信）が全体と同じ。描いていない区間は誤り。
#[test]
fn partial_values_deliver_only_drawn_chunks() {
    let engine = engine();
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 3 * C,
        seed: 5,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": 1.0 }), 1)
        .unwrap();
    let plan = compile(&engine, &g, &input).unwrap();
    let (want, _) = render(&engine, &plan, &input, Digest::default()).unwrap();
    let got = engine
        .render_area(
            &plan,
            &fingerprinted(&input, Digest::default()),
            &mut NodeCache::new(0),
            &AreaRequest {
                output: OutputId(0),
                area: Area::span(C + 3, C + 4),
            },
            None,
            &NoMonitor,
        )
        .unwrap();
    let v = &got.value;
    let mut valid = ChunkSet::empty(3);
    valid.insert(1);
    assert_eq!(
        v.audio_valid(),
        Some(kura_engine::render::region::Spans::one(C, 2 * C))
    );
    assert_eq!(
        chunks_covering(&Area::span(C, 2 * C), 3 * C as usize),
        valid
    );
    let Value::Audio(w) = &want else { panic!() };
    let mut out = vec![0f32; 5];
    v.audio_read(1, C + 10, &mut out).unwrap();
    assert_eq!(
        out,
        w.read_planes(C as usize + 10, C as usize + 15).unwrap()[1]
    );
    assert!(v.audio_read(1, C - 1, &mut out).is_err());
    assert!(v.audio_chunk(0, 0).is_err());
    assert_eq!(
        &*v.audio_chunk(0, 1).unwrap(),
        &*w.chunk_data(0, 1).unwrap()
    );
    assert_eq!(v.resident_bytes(), 2 * C * 4);
    assert!(!v.covers(&Area::span(0, 1)) && v.covers(&Area::span(C, C + 2)));
    assert!(v.exact().is_none() && v.audio_meta().is_some());
    assert_eq!(got.stats.drawn_chunks, 1);
}
