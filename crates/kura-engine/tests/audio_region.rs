//! 音声の範囲の描画の受け入れテスト（M13 段 1。docs/editing.md §17 の E37）。
//!
//! - audio_region_equals_full_on_random_documents：ランダムな音声の文書・ランダムな区間（1 フレーム・チャンクの境 ±1・終わり・
//!   2 つの区間）で、`render_area` の有効なチャンクが `render` と同じ。入力は常駐と遅延（源の参照）の両方。依存の外を読んで
//!   全体に落ちず（`fallbacks` 0）、警告は `render` の部分列。量子化した出力も同じ。
//! - audio_reach_is_sound：`audio_reach` の逆写しの外の入力を壊しても（無効なチャンクにしても）、区間の中は同じ。
//! - audio_region_declarations_match：音声を出す操作は、例で `audio_reach` が `Global` でない。
//! - E38（表示の経路）：audio_view_equals_render_on_random_edit_sequences（ランダムな手の列・表示の窓を動かす・キャッシュの
//!   上限 0・1 MiB・1 GiB で、値が `render` と同じ・報告が差を含み推論の中・継ぎはぎがまっさらと同じ・頼んだ区間のチャンク
//!   だけを持つ）、audio_inferred_delta_contains_actual、loop_only_change_reports_no_samples、audio_view_keeps_only_requested_chunks。

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
    random_audio_input, random_stateful_chain,
};
use kura_engine::value::{AUDIO_CHUNK, Audio, ChunkSet};
use kura_engine::{Digest, Issue, Limits, NoMonitor, Value, determinism};
use rand_chacha::ChaCha8Rng;
use serde_json::json;

const C: u64 = AUDIO_CHUNK as u64;

fn engine() -> Engine {
    Engine::new().unwrap()
}

/// 試験用のエンジン（今の登録表 ＋ 試験の状態を運ぶ操作 `test.iir1` とその表の解析。M17 段 1）。
fn stateful_engine() -> Engine {
    kura_engine::testing::stateful::engine().unwrap()
}

/// ランダムな音声の鎖（`stateful` なら試験の状態を運ぶ操作を混ぜる。`engine` は試験用のエンジン）。
fn chain(engine: &Engine, r: &mut ChaCha8Rng, shape: &Shape, len: usize, stateful: bool) -> Graph {
    if stateful {
        random_stateful_chain(engine, r, shape, len)
    } else {
        random_audio_chain(engine, r, shape, len)
    }
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
    let (checked, drawn, quantized, shared, lazy_runs, _) =
        region_on_random_documents(&engine(), 37, 60, false);
    assert!(
        checked > 120 && drawn > 100 && quantized > 20 && shared > 0 && lazy_runs > 40,
        "{checked} {drawn} {quantized} {shared} {lazy_runs}"
    );
}

/// E37（M17 段 1）：試験の状態を運ぶ操作（状態の表を申告して表から区間を描く核。格子は C・C を割り切らない値・2C）を混ぜた
/// ランダムな文書でも、範囲の描画の有効なチャンクは全体の描画と同じ（流し解き・揃えた逆写しを通る）。
#[test]
fn stateful_region_equals_full_on_random_documents() {
    let (checked, drawn, _, _, lazy_runs, stateful) =
        region_on_random_documents(&stateful_engine(), 137, 50, true);
    assert!(
        checked > 100 && drawn > 80 && lazy_runs > 30 && stateful > 20,
        "{checked} {drawn} {lazy_runs} {stateful}"
    );
}

/// E37 の本体：（確かめた数, 描いたチャンク, 量子化した文書, 共有したチャンク, 遅延の入力の回, 試験の操作のある文書の回）。
fn region_on_random_documents(
    engine: &Engine,
    seed: u64,
    cases: usize,
    stateful: bool,
) -> (usize, u64, usize, u64, usize, usize) {
    let mut r = determinism::rng(seed);
    let (mut checked, mut drawn, mut quantized, mut shared, mut lazy_runs, mut with_state) =
        (0, 0u64, 0, 0u64, 0, 0);
    for case in 0..cases {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 5) as usize;
        let g = chain(engine, &mut r, &input.shape(), len, stateful);
        let Some(plan) = compile(engine, &g, &input) else {
            continue;
        };
        let source = Digest([case as u8; 32]);
        let Some((want, warnings)) = render(engine, &plan, &input, source) else {
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
            with_state += usize::from(
                g.nodes
                    .iter()
                    .any(|n| n.op == kura_engine::testing::stateful::OP_ID || n.op == "audio.eq"),
            );
            checked += 1;
        }
    }
    (checked, drawn, quantized, shared, lazy_runs, with_state)
}

/// E37 の網（検証の直し）：流れの窓が 2〜3 つになる長い音声（`random_long_audio_input`。8 kHz・リサンプルなしの鎖）で、
/// ランダムな区間の範囲の描画の有効なチャンクが `render` と同じ（normalize の入力が部分の値なら、入力のポートを窓をまたいで
/// 流して解く）。入力は常駐と遅延。
#[test]
fn audio_region_equals_full_on_long_random_documents() {
    use kura_engine::testing::region::{random_long_audio_chain, random_long_audio_input};
    let engine = engine();
    let mut r = determinism::rng(137);
    let (mut checked, mut presolved) = (0, 0u64);
    for case in 0..6 {
        let input = random_long_audio_input(&mut r).generate().unwrap();
        let len = 2 + below(&mut r, 3) as usize;
        let g = random_long_audio_chain(&engine, &mut r, &input.shape(), len);
        let Some(plan) = compile(&engine, &g, &input) else {
            continue;
        };
        let source = Digest([case as u8; 32]);
        let Some((want, warnings)) = render(&engine, &plan, &input, source) else {
            continue;
        };
        let doc = String::from_utf8_lossy(&g.to_canonical_json().unwrap()).into_owned();
        for round in 0..2 {
            let value = if round == 0 {
                input.clone()
            } else {
                lazy(&input)
            };
            let fin = fingerprinted(&value, source);
            let mut cache = NodeCache::new([0, 1 << 30][(case + round) % 2]);
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
            assert_eq!(got.stats.fallbacks, 0, "{what}");
            check_region(&got.value, &want, &area).unwrap_or_else(|m| panic!("{what}: {m}"));
            let mut it = warnings.iter();
            assert!(got.warnings.iter().all(|w| it.any(|x| x == w)), "{what}");
            presolved += got.stats.presolved;
            checked += 1;
        }
    }
    assert!(checked >= 10 && presolved > 0, "{checked} {presolved}");
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
    engine: &Engine,
    op: &dyn DynOp,
    params: &serde_json::Value,
    input: &Value,
    full: &Value,
) -> Box<dyn kura_engine::audio::AudioKernel> {
    let p = op.parse(params).unwrap();
    let limits = Limits::default();
    let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, kura_engine::doc::RULES);
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
    // 試験用のエンジン（今の登録表 ＋ 試験の状態を運ぶ操作）で、状態の表を使う核の `Aligned` も確かめる（M17 段 1）
    let engine = stateful_engine();
    let mut r = determinism::rng(38);
    let mut checked = 0;
    for (op, name, params, input) in audio_cases(&engine) {
        let p = op.parse(&params).unwrap();
        let reach = op.audio_reach(p.as_ref(), &[input.shape()]).unwrap();
        let full = kernel_of(&engine, op.as_ref(), &params, &input, &input);
        let frames = full.frames();
        let out_shape = Shape::Audio {
            rate: full.rate(),
            channels: full.channels() as u16,
            frames: kura_engine::shape::Dim::Exact(frames),
        };
        let count = frames.div_ceil(C);
        // 全体の核の出力（チャンクごと。区間ごとに描き直さない）
        let want: Vec<Vec<Vec<f32>>> = (0..full.channels())
            .map(|ch| {
                (0..count)
                    .map(|j| {
                        let s = j * C;
                        let mut v = vec![0f32; ((s + C).min(frames) - s) as usize];
                        full.render(ch, s, &mut v).unwrap();
                        v
                    })
                    .collect()
            })
            .collect();
        // 区間：最初と最後のチャンク・チャンクの境をまたぐ 2 つ・ランダムな区間
        let mut areas: Vec<Area> = [0, count.saturating_sub(1)]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
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
                let k = kernel_of(&engine, op.as_ref(), &params, &broken, &input);
                for (ch, want) in want.iter().enumerate().take(k.channels()) {
                    for j in rounded.iter() {
                        let s = j as u64 * C;
                        let e = (s + C).min(frames);
                        let mut got = vec![0f32; (e - s) as usize];
                        k.render(ch, s, &mut got)
                            .unwrap_or_else(|e| panic!("{what}: {e}"));
                        assert!(
                            got.iter()
                                .zip(&want[j])
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
    let engine = stateful_engine();
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
        // 直線位相のイコライザ（M17 段 7）：変わった範囲も核の読みも、ブロックを絶対位置 0 に揃えた Window（前後に遅れ D）
        if op.id() == "audio.eq" && params["phase"] == "linear" {
            if reach != Reach::SameTime {
                assert!(
                    matches!(reach, Reach::Window { before, after, align, .. }
                        if before == after && align == 2 * before),
                    "{name}：{reach:?}"
                );
                assert_eq!(op.reach(p.as_ref(), &[input.shape()]).unwrap(), reach);
            }
            continue;
        }
        // 状態を運ぶ操作（M17 段 1）：変わった範囲は Prefix、核の読みは表の格子に揃えた Aligned（何も変えない引数は SameTime）
        if (op.id() == kura_engine::testing::stateful::OP_ID || op.id() == "audio.eq")
            && reach != Reach::SameTime
        {
            assert!(
                matches!(
                    op.reach(p.as_ref(), &[input.shape()]).unwrap(),
                    Reach::Prefix { .. }
                ),
                "{name}"
            );
            assert!(matches!(reach, Reach::Aligned { before: 0, .. }), "{name}");
        }
    }
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        [
            "audio.eq",
            "audio.fade",
            "audio.gain",
            "audio.mono",
            "audio.normalize",
            "audio.resample",
            "audio.set_loop",
            "audio.trim",
            kura_engine::testing::stateful::OP_ID,
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

/// 検証の直し（M13 段 1）：量子化した出力の範囲の描画・表示は、出力がどこから来ても（入力をそのまま出す出力・外したノード
/// だけの鎖・キャッシュに当たった出力のノード・範囲で描いたノード）、頼んだ区間のチャンクだけを量子化して持つ。範囲の
/// 見積もり（`EstimateSpec::area`）はその量を覆い、全体よりずっと小さい。遅延の源は頼んだ区間のチャンクしか読まない。
/// 全体を頼めば `quantized_output` の全体（由来 Exact で `render` と同じ）。
#[test]
fn quantized_outputs_keep_only_the_requested_chunks() {
    use kura_engine::audio::source::AudioSource;
    use kura_engine::doc::{Dither, QUANTIZE_VERSION, Quantize};
    use kura_engine::render::cache::Origin;
    use kura_engine::render::estimate::{CacheProbe, EstimateSpec, Source};
    use kura_engine::render::view::ViewRequest;
    use kura_engine::testing::source::SyntheticSource;
    let engine = engine();
    let (frames, channels) = (40 * C + 77, 2usize);
    let q = Quantize {
        v: QUANTIZE_VERSION,
        bits: 16,
        dither: Dither::Tpdf,
        seed: 5,
    };
    // 文書：ノードなし・外したゲインだけ・ゲイン（出力のノードはキャッシュに当てる）・ゲイン（範囲で描く）
    let docs: Vec<(&str, Graph, bool)> = {
        let plain = Graph::new_single();
        let mut off = Graph::new_single();
        off.push_linear("audio.gain", 1, json!({ "db": -2.0 }), 1)
            .unwrap();
        off.nodes[0].enabled = false;
        let mut gain = Graph::new_single();
        gain.push_linear("audio.gain", 1, json!({ "db": -2.0 }), 1)
            .unwrap();
        vec![
            ("ノードなし", plain, false),
            ("外したノードだけ", off, false),
            ("当たった出力のノード", gain.clone(), true),
            ("範囲で描いたノード", gain, false),
        ]
    };
    let lazy_input = |lru: u64| {
        let src = Arc::new(SyntheticSource::new(8_000, channels, frames, 11, lru));
        let v = Value::Audio(Arc::new(
            Audio::from_source(src.clone(), Default::default()).unwrap(),
        ));
        // 遅延の入力の指紋は記録か派生の指紋（全体のダイジェストを取らない）
        let fin = BTreeMap::from([(
            InputId(0),
            FingerprintedInput {
                value: v.clone(),
                source: Digest([3; 32]),
                fingerprint: Fingerprint(Digest([8; 32])),
            },
        )]);
        (src, v, fin)
    };
    let full_bytes = frames * channels as u64 * 4;
    for (name, plain_doc, hit) in docs {
        let mut g = plain_doc.clone();
        g.outputs[0].quantize = Some(q);
        let (_, whole_input, _) = lazy_input(1 << 30);
        let plan = compile(&engine, &g, &whole_input).unwrap();
        let (want, _) = render(&engine, &plan, &whole_input, Digest([3; 32])).unwrap();
        for area in [
            Area::span(5 * C + 10, 5 * C + 20),
            Area::span(C - 1, C + 1).union(&Area::span(39 * C, 39 * C + 3)),
        ] {
            let want_chunks = chunks_covering(&area, frames as usize);
            let want_bytes: u64 = want_chunks
                .iter()
                .map(|k| (frames - k as u64 * C).min(C) * channels as u64 * 4)
                .sum();
            for view in [false, true] {
                let what = format!("{name}・{area:?}・表示 {view}");
                let (src, _, fin) = lazy_input(4 * C * channels as u64 * 4);
                let mut cache = NodeCache::new(1 << 30);
                if hit {
                    // 量子化のない同じ文書を全体で描いて、出力のノードの値（等倍の全体）だけをキャッシュに入れる
                    let p = compile(&engine, &plain_doc, &whole_input).unwrap();
                    engine
                        .render_area(
                            &p,
                            &fin,
                            &mut cache,
                            &AreaRequest {
                                output: OutputId(0),
                                area: Area::All,
                            },
                            None,
                            &NoMonitor,
                        )
                        .unwrap();
                }
                let est = engine
                    .estimate(
                        &plan,
                        &EstimateSpec::new(3)
                            .all_inputs(&plan, Source::Held)
                            .area(OutputId(0), area.clone()),
                        Some(CacheProbe {
                            cache: &cache,
                            inputs: &fin,
                        }),
                    )
                    .unwrap();
                let reads = src.stats().reads;
                let got = if view {
                    engine
                        .render_view(
                            &plan,
                            &fin,
                            &mut cache,
                            &ViewRequest {
                                output: OutputId(0),
                                area: area.clone(),
                                focus: None,
                            },
                            None,
                            &NoMonitor,
                        )
                        .unwrap()
                        .view
                        .value()
                        .clone()
                } else {
                    engine
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
                        .unwrap()
                        .value
                };
                let read = src.stats().reads - reads;
                check_region(&got, &want, &area).unwrap_or_else(|m| panic!("{what}: {m}"));
                assert_eq!(got.audio_valid(), Some(want_chunks.clone()), "{what}");
                assert_eq!(got.resident_bytes(), want_bytes, "{what}：常駐");
                assert_eq!(got.origin(), Origin::Partial, "{what}");
                assert!(
                    est.render >= got.resident_bytes() && est.render < full_bytes / 2,
                    "{what}：見積もり {}（常駐 {}・全体 {full_bytes}）",
                    est.render,
                    got.resident_bytes()
                );
                // 源は頼んだ区間のチャンクだけを読む（出力のノードがキャッシュに当たれば読まない）
                let most = if hit { 0 } else { want_chunks.count() as u64 };
                assert!(read <= most, "{what}：源のチャンクを {read} 個読んだ");
            }
        }
        // 全体を頼めば量子化した全体（由来 Exact。render と同じ）
        let (_, _, fin) = lazy_input(1 << 30);
        let got = engine
            .render_area(
                &plan,
                &fin,
                &mut NodeCache::new(0),
                &AreaRequest {
                    output: OutputId(0),
                    area: Area::All,
                },
                None,
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(got.value.exact(), Some(&want), "{name}：全体");
    }
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
    assert_eq!(v.audio_valid(), Some(valid.clone()));
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

/// E37：trim → normalize の範囲の描画は、normalize の計測（入力は trim の出力の部分の値）を、trim の出力を窓ごとに描いて
/// 流して解く（流し解き）。結果は `render` と同じで、キャッシュに入った計測は次の描画で使う（流し直さない）。
#[test]
fn normalize_region_presolves_its_measurement() {
    let engine = engine();
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 4 * C + 17,
        seed: 12,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.trim", 1, json!({ "start": 1_234, "end": 4 * C }), 1)
        .unwrap();
    g.push_linear(
        "audio.normalize",
        1,
        json!({ "target": -20.0, "peakLimit": -2.0 }),
        2,
    )
    .unwrap();
    g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 3)
        .unwrap();
    let plan = compile(&engine, &g, &input).unwrap();
    let source = Digest([2; 32]);
    let (want, warnings) = render(&engine, &plan, &input, source).unwrap();
    for (resident, cap) in [(true, 0u64), (true, 1 << 30), (false, 1 << 30)] {
        let value = if resident {
            input.clone()
        } else {
            lazy(&input)
        };
        let fin = BTreeMap::from([(InputId(0), FingerprintedInput::new(value, source).unwrap())]);
        let mut cache = NodeCache::new(cap);
        for (round, area) in [Area::span(C + 5, C + 6), Area::span(3 * C - 2, 3 * C + 2)]
            .into_iter()
            .enumerate()
        {
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
                .unwrap();
            let what = format!("常駐 {resident}・上限 {cap}・{round} 回目");
            check_region(&got.value, &want, &area).unwrap_or_else(|m| panic!("{what}: {m}"));
            assert_eq!(got.stats.fallbacks, 0, "{what}");
            // normalize は範囲で描く（全体で描くのは、流し解きの窓の中の全体の描画だけ。その trim はキャッシュに入り、次の
            // 描画では当たる）
            assert_eq!(
                got.stats.region_nodes + got.stats.hits,
                3,
                "{what}: {:?}",
                got.stats
            );
            let presolved = if cap > 0 && round > 0 { 0 } else { 1 };
            assert_eq!(got.stats.presolved, presolved, "{what}");
            let mut it = warnings.iter();
            assert!(got.warnings.iter().all(|w| it.any(|x| x == w)), "{what}");
            assert!(!got.warnings.is_empty() || warnings.is_empty(), "{what}");
        }
    }
}

/// 検証の直し（M13 段 1）：normalize を 2 つ重ね、キャッシュの上限が 0（計測が残らない）でも、下流の normalize の流し解きの
/// 窓ごとに上流の normalize の計測を解き直さない（入力が完全な値として解いた計測も、この描画の中の表に入れる）。解いた解析の
/// 数は窓の数によらない。値は `render` と同じ。
#[test]
fn upstream_measurements_are_solved_once_per_render() {
    let engine = engine();
    let mut solved = Vec::new();
    for chunks in [17u64, 33] {
        let input = InputGen::AudioNoise {
            rate: 8_000,
            channels: 1,
            frames: chunks * C + 5,
            seed: 2,
        }
        .generate()
        .unwrap();
        let mut g = Graph::new_single();
        g.push_linear(
            "audio.normalize",
            1,
            json!({ "target": -20.0, "peakLimit": null }),
            1,
        )
        .unwrap();
        g.push_linear(
            "audio.normalize",
            1,
            json!({ "target": -18.0, "peakLimit": -1.0 }),
            2,
        )
        .unwrap();
        let plan = compile(&engine, &g, &input).unwrap();
        let source = Digest([2; 32]);
        let (want, _) = render(&engine, &plan, &input, source).unwrap();
        let area = Area::span(C + 3, C + 4);
        let mut cache = NodeCache::new(0);
        let before = cache.stats();
        let got = engine
            .render_area(
                &plan,
                &inputs_of(&input, source),
                &mut cache,
                &AreaRequest {
                    output: OutputId(0),
                    area: area.clone(),
                },
                None,
                &NoMonitor,
            )
            .unwrap();
        check_region(&got.value, &want, &area).unwrap();
        assert_eq!(
            (got.stats.presolved, got.stats.fallbacks),
            (1, 0),
            "{chunks}"
        );
        solved.push(cache.stats().since(&before).analysis_solved);
    }
    // 上流の計測 1 回（外側の描画と流し解きの窓で共有）＋ 下流の計測の流し解き 1 回
    assert_eq!(solved, vec![2, 2]);
}

// ---- E38：音声の継ぎはぎと変わった区間（表示の経路） ----

use kura_engine::render::delta::OutputChange;
use kura_engine::render::region::Spans;
use kura_engine::render::view::{View, ViewRequest};
use kura_engine::testing::region::{
    check_contains, random_audio_edit, random_audio_edit_in, reseeded,
};

fn inputs_of(v: &Value, source: Digest) -> BTreeMap<InputId, FingerprintedInput> {
    BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(v.clone(), source).unwrap(),
    )])
}

/// 区間の集合 `inner` が、範囲 `outer`（長さ `n` で `All` を開く）の中にあるか。
fn spans_within(inner: &Spans, outer: &Area, n: u64) -> bool {
    let outer = match outer.expand(&Shape::Audio {
        rate: 1,
        channels: 1,
        frames: kura_engine::shape::Dim::Exact(n),
    }) {
        Area::All => return true,
        Area::Samples(s) => s,
        _ => Spans::new(),
    };
    inner
        .spans()
        .iter()
        .all(|(a, b)| outer.spans().iter().any(|(c, d)| c <= a && b <= d))
}

/// 表示の値の有効なチャンクの区間（区間を含めて広げた推論の確かめに使う。畳まれても広がるだけ）。
fn valid_spans(v: &View) -> Spans {
    let frames = match v.value().shape() {
        Shape::Audio { frames, .. } => frames.max() as usize,
        _ => 0,
    };
    v.value()
        .audio_valid()
        .map(|set| kura_engine::render::audio::chunk_spans(&set, frames))
        .unwrap_or_default()
}

#[derive(Debug, Default)]
struct AudioViewCounts {
    steps: usize,
    patched: usize,
    reused: u64,
    partial: usize,
    /// 遅延の入力（源の参照のチャンク）で回した手。
    lazy: usize,
    /// 遅延の入力で継ぎはぎした手。
    lazy_patched: usize,
    kinds: BTreeMap<&'static str, usize>,
}

/// ランダムな音声の文書とランダムな手の列（引数・フェードの長さ・ループ点・ゲイン・トリムの終わり・正規化の目標・有効の
/// 切り替え・構造の手・入力だけの変更）で、表示の窓をときどき動かし、キャッシュの上限 0・1 MiB・1 GiB で回す。入力は 3 回に
/// 1 回、遅延の値（源の参照のチャンク。継ぎはぎ・refine が源の参照を共有として扱う道）にする（検証の直し）。
fn audio_view_cases(engine: &Engine, seed: u64, cases: usize) -> AudioViewCounts {
    audio_view_cases_in(engine, seed, cases, false)
}

/// [`audio_view_cases`] の、`stateful` なら試験の状態を運ぶ操作を混ぜる版（M17 段 1。`engine` は試験用のエンジン）。
fn audio_view_cases_in(
    engine: &Engine,
    seed: u64,
    cases: usize,
    stateful: bool,
) -> AudioViewCounts {
    let mut r = determinism::rng(seed);
    let mut c = AudioViewCounts::default();
    for case in 0..cases {
        let make = random_audio_input(&mut r);
        let mut input = make.generate().unwrap();
        let lazy_in = case % 3 == 2;
        let inputs_of = |v: &Value, source: Digest| {
            if lazy_in {
                inputs_of(&lazy(v), source)
            } else {
                inputs_of(v, source)
            }
        };
        let len = 1 + below(&mut r, 5) as usize;
        let mut g = chain(engine, &mut r, &input.shape(), len, stateful);
        let Some(plan) = compile(engine, &g, &input) else {
            continue;
        };
        let source = Digest([7; 32]);
        let Some((mut want, _)) = render(engine, &plan, &input, source) else {
            continue;
        };
        let mut cache = NodeCache::new([0, 1 << 20, 1 << 30][case % 3]);
        let first_area = random_audio_area(&mut r, frames_of(&want));
        let first = engine
            .render_view(
                &plan,
                &inputs_of(&input, source),
                &mut cache,
                &ViewRequest {
                    output: OutputId(0),
                    area: first_area.clone(),
                    focus: None,
                },
                None,
                &NoMonitor,
            )
            .unwrap();
        check_region(first.view.value(), &want, &first_area).unwrap();
        assert_eq!(first.changed, OutputChange::All);
        let mut prev = first.view;
        let mut area = first_area;
        for _ in 0..5 {
            // 6 回に 1 回は入力の値だけを替える
            let (next, kind, next_input) = if below(&mut r, 6) == 0 {
                let other = reseeded(&make, r.next_u64()).unwrap().generate().unwrap();
                (g.clone(), "input", other)
            } else {
                let Some((next, kind)) =
                    random_audio_edit_in(engine, &mut r, &g, &input.shape(), stateful)
                else {
                    break;
                };
                (next, kind, input.clone())
            };
            let Some(next_plan) = compile(engine, &next, &next_input) else {
                break;
            };
            let Some((next_want, warnings)) = render(engine, &next_plan, &next_input, source)
            else {
                break;
            };
            // ときどき表示の窓を動かす
            if below(&mut r, 3) == 0 {
                area = random_audio_area(&mut r, frames_of(&next_want));
            }
            let fin = inputs_of(&next_input, source);
            let req = ViewRequest {
                output: OutputId(0),
                area: area.clone(),
                focus: None,
            };
            let got = engine
                .render_view(&next_plan, &fin, &mut cache, &req, Some(&prev), &NoMonitor)
                .unwrap();
            let what = format!(
                "種 {seed}・{case} {kind}（{area:?}）\n前 {}\n後 {}",
                String::from_utf8_lossy(&g.to_canonical_json().unwrap()),
                String::from_utf8_lossy(&next.to_canonical_json().unwrap())
            );
            check_region(got.view.value(), &next_want, &area)
                .unwrap_or_else(|m| panic!("{what}: {m}"));
            assert_eq!((got.area.fallbacks, got.area.rebased), (0, 0), "{what}");
            // 継ぎはぎは、まっさらに描いたもの（前の値なし）と同じ有効なチャンクを持つ
            let fresh = engine
                .render_area(
                    &next_plan,
                    &fin,
                    &mut NodeCache::new(0),
                    &AreaRequest {
                        output: OutputId(0),
                        area: area.clone(),
                    },
                    None,
                    &NoMonitor,
                )
                .unwrap();
            assert_eq!(
                fresh.value.audio_valid(),
                got.view.value().audio_valid(),
                "{what}：表示の値は頼んだ区間のチャンクだけを持つ"
            );
            // 報告：前後を全体で描いた差を含み、推論と「前は無効で今は有効」の中にある
            match &got.changed {
                OutputChange::All => {}
                OutputChange::Samples(s) => {
                    check_contains(&want, &next_want, &Area::samples(s.clone()))
                        .unwrap_or_else(|m| panic!("{what}: 報告の外が変わった：{m}"));
                    // 「前は無効で今は有効」は今の有効なチャンクの中（ここではそれで広げて確かめる）
                    let mut guess = got.inferred.clone();
                    guess.union_with(&Area::samples(valid_spans(&got.view)));
                    assert!(
                        spans_within(s, &guess, frames_of(&next_want)),
                        "{what}: 報告 {s:?} が推論 {:?} の外",
                        got.inferred
                    );
                }
                OutputChange::Tiles(_) => panic!("{what}: 音声にタイルの報告"),
            }
            // 前後を全体で描いた差は推論に入る
            check_contains(&want, &next_want, &got.inferred)
                .unwrap_or_else(|m| panic!("{what}: 推論の外が変わった：{m}"));
            let same_meta = match (&want, &next_want) {
                (Value::Audio(a), Value::Audio(b)) => a.meta == b.meta,
                _ => false,
            };
            if !same_meta {
                assert!(got.meta_changed, "{what}: メタデータの変化を報告しない");
            }
            let mut it = warnings.iter();
            assert!(
                got.warnings.iter().all(|w| it.any(|x| x == w)),
                "{what}: 警告が部分列でない"
            );
            match got.view.value().origin() {
                kura_engine::render::cache::Origin::Patched => {
                    c.patched += 1;
                    c.lazy_patched += usize::from(lazy_in);
                    c.reused += got.area.reused_chunks;
                    assert!(got.view.value().exact().is_none());
                }
                kura_engine::render::cache::Origin::Partial => c.partial += 1,
                _ => {}
            }
            c.steps += 1;
            c.lazy += usize::from(lazy_in);
            *c.kinds.entry(kind).or_default() += 1;
            (g, want, prev, input) = (next, next_want, got.view, next_input);
        }
    }
    c
}

/// E38：表示の経路の値は `render` と同じチャンクを持ち、報告は差を含み推論の中にあり、継ぎはぎはまっさらに描いたものと同じ。
#[test]
fn audio_view_equals_render_on_random_edit_sequences() {
    let c = audio_view_cases(&engine(), 38, 40);
    assert!(
        c.steps > 120
            && c.patched > 20
            && c.reused > 20
            && c.partial > 10
            && c.lazy > 30
            && c.lazy_patched > 5,
        "{c:?}"
    );
    for kind in [
        "gain", "fade", "loop", "target", "trim_end", "toggle", "input", "add", "remove",
    ] {
        assert!(
            c.kinds.get(kind).copied().unwrap_or(0) > 0,
            "{kind} が起きない：{c:?}"
        );
    }
}

/// E38（M17 段 1）：試験の状態を運ぶ操作を混ぜた文書と手の列（表に入れない引数だけの手・表を解き直す手・上流の手）でも、
/// 表示の経路の値は `render` と同じチャンクを持ち、報告は差を含み推論の中にある（`Prefix` の前の写し）。
#[test]
fn stateful_view_equals_render_on_random_edit_sequences() {
    let c = audio_view_cases_in(&stateful_engine(), 138, 30, true);
    // 状態を運ぶ操作の上流の手は終わりまで変わりうる（`Prefix` の前の写し）ので、継ぎはぎは今の操作だけの網より少ない
    assert!(c.steps > 80 && c.patched > 5 && c.partial > 20, "{c:?}");
    for kind in ["iir_out", "params", "add", "replace"] {
        assert!(
            c.kinds.get(kind).copied().unwrap_or(0) > 0,
            "{kind} が起きない：{c:?}"
        );
    }
}

/// E38：ランダムな音声の手で、前後を全体で描いた差は推論（`Engine::delta`）に入る（E31 の音声版）。
#[test]
fn audio_inferred_delta_contains_actual() {
    let engine = engine();
    let mut r = determinism::rng(31);
    let mut checked = 0;
    for _ in 0..80 {
        let input = random_audio_input(&mut r).generate().unwrap();
        let len = 1 + below(&mut r, 4) as usize;
        let g = random_audio_chain(&engine, &mut r, &input.shape(), len);
        let Some((next, kind)) = random_audio_edit(&engine, &mut r, &g, &input.shape()) else {
            continue;
        };
        let (Some(a), Some(b)) = (
            compile(&engine, &g, &input),
            compile(&engine, &next, &input),
        ) else {
            continue;
        };
        let source = Digest::default();
        let (Some((x, _)), Some((y, _))) = (
            render(&engine, &a, &input, source),
            render(&engine, &b, &input, source),
        ) else {
            continue;
        };
        let fin = inputs_of(&input, source);
        let d = engine.delta(&a, &fin, &b, &fin).unwrap();
        check_contains(&x, &y, &d.outputs[0].1).unwrap_or_else(|m| {
            panic!(
                "{kind}: {m}\n{}\n{}",
                String::from_utf8_lossy(&g.to_canonical_json().unwrap()),
                String::from_utf8_lossy(&next.to_canonical_json().unwrap())
            )
        });
        checked += 1;
    }
    assert!(checked > 50, "{checked}");
}

/// 決まった鎖（ゲイン → ループ点）と、表示の窓（チャンク 1 を含む区間）。
fn loop_doc(lp: (u64, u64)) -> Graph {
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -1.5 }), 1)
        .unwrap();
    g.push_linear(
        "audio.set_loop",
        1,
        json!({ "loop": { "start": lp.0, "end": lp.1 } }),
        2,
    )
    .unwrap();
    g
}

/// E38：ループ点だけの手は、標本の報告が空で `meta_changed` が真、表示のチャンクを前の表示と共有する。
#[test]
fn loop_only_change_reports_no_samples() {
    let engine = engine();
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 3 * C + 5,
        seed: 3,
    }
    .generate()
    .unwrap();
    let source = Digest::default();
    let fin = inputs_of(&input, source);
    let area = Area::span(C - 10, 2 * C + 10);
    let req = ViewRequest {
        output: OutputId(0),
        area: area.clone(),
        focus: None,
    };
    for cap in [0, 1 << 30] {
        let mut cache = NodeCache::new(cap);
        let a = compile(&engine, &loop_doc((10, 500)), &input).unwrap();
        let first = engine
            .render_view(&a, &fin, &mut cache, &req, None, &NoMonitor)
            .unwrap();
        let b = compile(&engine, &loop_doc((20, 3 * C)), &input).unwrap();
        let got = engine
            .render_view(&b, &fin, &mut cache, &req, Some(&first.view), &NoMonitor)
            .unwrap();
        assert_eq!(
            got.changed,
            OutputChange::Samples(Spans::new()),
            "上限 {cap}"
        );
        assert!(got.meta_changed);
        assert_eq!(got.area.reused_chunks, 3, "上限 {cap}");
        for k in 0..3 {
            for ch in 0..2 {
                assert!(
                    got.view
                        .value()
                        .shares_audio_chunk(first.view.value(), ch, k),
                    "上限 {cap}・チャンク {k}"
                );
            }
        }
        let (want, _) = render(&engine, &b, &input, source).unwrap();
        check_region(got.view.value(), &want, &area).unwrap();
    }
}

/// E38：表示の値は頼んだ区間のチャンクしか持たない（窓を動かすと、前の窓のチャンクを持ち越さない）。
#[test]
fn audio_view_keeps_only_requested_chunks() {
    let engine = engine();
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 3 * C,
        seed: 9,
    }
    .generate()
    .unwrap();
    let source = Digest::default();
    let fin = inputs_of(&input, source);
    let plan = compile(&engine, &loop_doc((1, 2)), &input).unwrap();
    let mut cache = NodeCache::new(1 << 30);
    let view = |area: Area, prev: Option<&View>, cache: &mut NodeCache| {
        engine
            .render_view(
                &plan,
                &fin,
                cache,
                &ViewRequest {
                    output: OutputId(0),
                    area,
                    focus: None,
                },
                prev,
                &NoMonitor,
            )
            .unwrap()
    };
    let a = view(Area::span(0, 10), None, &mut cache);
    let one = |k: usize| {
        let mut set = ChunkSet::empty(3);
        set.insert(k);
        Some(set)
    };
    assert_eq!(a.view.value().audio_valid(), one(0));
    let b = view(Area::span(2 * C + 1, 2 * C + 2), Some(&a.view), &mut cache);
    assert_eq!(b.view.value().audio_valid(), one(2));
    assert_eq!(b.changed, OutputChange::Samples(Spans::one(2 * C, 3 * C)));
    assert_eq!(b.view.value().resident_bytes(), C * 4);
    // 同じ窓をもう一度：何も描かず、前の表示のチャンクを共有する
    let c = view(Area::span(2 * C + 1, 2 * C + 2), Some(&b.view), &mut cache);
    assert_eq!(c.changed, OutputChange::Samples(Spans::new()));
    assert_eq!((c.area.drawn_chunks, c.area.reused_chunks), (0, 1));
}

/// 検証の直し（M13 段 1）：継ぎはぎで描くチャンクが 64 を超える区間に散らばっても（区間（`Spans`）に直すと外接の区間に
/// 畳まれる）、前の表示と共有するチャンクは描き直さず、量子化（TPDF。2 度当てると変わる）は描いたチャンクだけに当たる。
/// 値は `render` と同じで、`reused_chunks` は実際に共有した数。表示の値の有効なチャンク（`audio_valid`）も畳まずに報告する。
#[test]
fn scattered_patches_keep_the_shared_chunks() {
    use kura_engine::doc::{Dither, QUANTIZE_VERSION, Quantize};
    let engine = engine();
    let chunks = 131u64;
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: chunks * C,
        seed: 71,
    }
    .generate()
    .unwrap();
    let source = Digest([6; 32]);
    let fin = inputs_of(&input, source);
    // ゲイン → フェード（出力のノードはフェード、ゲインは上流の範囲を持つノード）
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 1)
        .unwrap();
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": 100, "fadeOut": 0, "curve": "linear" }),
        2,
    )
    .unwrap();
    g.outputs[0].quantize = Some(Quantize {
        v: QUANTIZE_VERSION,
        bits: 16,
        dither: Dither::Tpdf,
        seed: 3,
    });
    let plan = compile(&engine, &g, &input).unwrap();
    let (want, _) = render(&engine, &plan, &input, source).unwrap();
    // 前の表示：チャンク 1, 3, …, 127 の中の 1 標本ずつ（64 区間）
    let mut spans = Spans::new();
    for k in (1..128u64).step_by(2) {
        spans.push(k * C + 5, k * C + 6);
    }
    assert_eq!(spans.spans().len(), 64);
    let first_area = Area::samples(spans);
    for cap in [0, 1 << 30] {
        let mut cache = NodeCache::new(cap);
        let req = |area: Area| ViewRequest {
            output: OutputId(0),
            area,
            focus: None,
        };
        let first = engine
            .render_view(
                &plan,
                &fin,
                &mut cache,
                &req(first_area.clone()),
                None,
                &NoMonitor,
            )
            .unwrap();
        check_region(first.view.value(), &want, &first_area).unwrap();
        let odd = first.view.value().audio_valid().unwrap();
        assert_eq!(
            odd.count(),
            64,
            "上限 {cap}：前の表示の有効なチャンクを畳まずに報告する"
        );
        // 全体を頼む：共有するチャンク 64、描くチャンク 67（区間にすると 65 個で、畳まれる）
        let got = engine
            .render_view(
                &plan,
                &fin,
                &mut cache,
                &req(Area::All),
                Some(&first.view),
                &NoMonitor,
            )
            .unwrap();
        let what = format!("上限 {cap}：{:?}", got.area);
        check_region(got.view.value(), &want, &Area::All).unwrap_or_else(|m| panic!("{what}: {m}"));
        assert_eq!(got.area.reused_chunks, 64, "{what}");
        for k in odd.iter() {
            assert!(
                got.view
                    .value()
                    .shares_audio_chunk(first.view.value(), 0, k),
                "{what}：チャンク {k} を描き直した"
            );
        }
        // 出力のノード（フェード）は描くチャンクだけを描く（上流のゲインは畳んだ区間で描いてよい）
        assert!(
            got.area.drawn_chunks <= 67 + chunks,
            "{what}：描いたチャンクが多すぎる"
        );
        assert_eq!(
            got.view.value().audio_valid().map(|s| s.count()),
            Some(chunks as usize)
        );
    }
}

/// E38（M13 段 1 の C11）：トリムの終わりだけを動かす手（縮める・伸ばす）は、尻（とフェードアウトの窓）だけを報告し、頭の
/// チャンクは前の表示と共有する。値は `render` と同じ。
#[test]
fn trim_end_change_reports_the_tail() {
    let engine = engine();
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: 4 * C + 100,
        seed: 21,
    }
    .generate()
    .unwrap();
    let source = Digest::default();
    let fin = inputs_of(&input, source);
    let doc = |end: u64| {
        let mut g = Graph::new_single();
        g.push_linear("audio.trim", 1, json!({ "start": 300, "end": end }), 1)
            .unwrap();
        g.push_linear("audio.gain", 1, json!({ "db": -2.0 }), 2)
            .unwrap();
        g.push_linear(
            "audio.fade",
            1,
            json!({ "fadeIn": 0, "fadeOut": 500, "curve": "linear" }),
            3,
        )
        .unwrap();
        g
    };
    let req = ViewRequest::whole(OutputId(0), None);
    for cap in [0, 1 << 30] {
        let mut cache = NodeCache::new(cap);
        let mut end = 3 * C + 50;
        let plan = compile(&engine, &doc(end), &input).unwrap();
        let mut prev = engine
            .render_view(&plan, &fin, &mut cache, &req, None, &NoMonitor)
            .unwrap()
            .view;
        for next_end in [3 * C - 20, 3 * C + 500, 2 * C + 7] {
            let plan = compile(&engine, &doc(next_end), &input).unwrap();
            let got = engine
                .render_view(&plan, &fin, &mut cache, &req, Some(&prev), &NoMonitor)
                .unwrap();
            let (want, _) = render(&engine, &plan, &input, source).unwrap();
            let what = format!("上限 {cap}・終わり {end} → {next_end}");
            check_region(got.view.value(), &want, &Area::All)
                .unwrap_or_else(|m| panic!("{what}: {m}"));
            let (old_len, new_len) = (end - 300, next_end - 300);
            let lo = old_len.min(new_len) - 500;
            let OutputChange::Samples(s) = &got.changed else {
                panic!("{what}：全体を報告した（{:?}）", got.changed);
            };
            assert!(!s.is_empty(), "{what}");
            assert!(
                s.spans()
                    .iter()
                    .all(|(a, b)| *a >= lo / C * C && *b <= new_len),
                "{what}：尻の外を報告した {s:?}"
            );
            assert!(got.area.reused_chunks >= 1, "{what}: {:?}", got.area);
            assert!(
                got.view.value().shares_audio_chunk(prev.value(), 0, 0),
                "{what}"
            );
            (prev, end) = (got.view, next_end);
        }
    }
}
