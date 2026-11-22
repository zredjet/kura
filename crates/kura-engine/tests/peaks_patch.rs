//! 波形の段の小分けの部分更新（M13 段 4。docs/editing.md §17 の E46）：`PeaksPatch` に R のチャンクを `feed_chunk` と
//! 流れの口（`partial`）で何回にも分けて渡すと、`assemble` が新しい値を `compute` したものと同じ段を組む。
//!
//! - incremental_patch_equals_compute：ランダムな値・汚れの区間・長さの変化・窓の切り方（1・3・`STREAM_CHUNKS`）・
//!   `feed_chunk` と `partial` の混ぜ方で。`fresh`（前の段なし）も。R を受け切る前の `assemble` は `Internal`。
//! - partial_patcher_over_many_streams：計画の出力を `Engine::stream_audio` で窓ごとに何回にも分けて流しても同じ。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::analysis::AnalyzeCx;
use kura_engine::analysis::audio::peaks::{PEAK_BINS, Peaks, PeaksParams, PeaksPatch, compute};
use kura_engine::audio::stream::{AudioSink, STREAM_CHUNKS, StreamInfo};
use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::region::{Area, Spans};
use kura_engine::render::{Engine, InputValue};
use kura_engine::testgen::InputGen;
use kura_engine::value::{AUDIO_CHUNK, Audio, AudioMeta};
use kura_engine::{Digest, EngineError, Limits, NoMonitor, Value, determinism};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

const C: usize = AUDIO_CHUNK;

fn below(r: &mut ChaCha8Rng, n: usize) -> usize {
    ((u128::from(r.next_u64()) * n.max(1) as u128) >> 64) as usize
}

fn audio(planes: Vec<Vec<f32>>) -> Audio {
    Audio::new(
        8_000,
        planes.into_iter().map(Arc::new).collect(),
        AudioMeta::default(),
    )
    .unwrap()
}

fn peaks_of(a: &Audio, p: &PeaksParams) -> Peaks {
    let limits = Limits::default();
    compute(
        a,
        p,
        &AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        },
    )
    .unwrap()
}

fn random_params(r: &mut ChaCha8Rng) -> PeaksParams {
    let i = below(r, PEAK_BINS.len());
    let j = i + below(r, PEAK_BINS.len() - i);
    PeaksParams {
        finest: PEAK_BINS[i],
        coarsest: PEAK_BINS[j],
    }
}

/// `patch` の R のチャンクを、ランダムに `feed_chunk`（値から写さずに借りる）と流れの窓（1・3・`STREAM_CHUNKS` チャンク）に
/// 分けて渡す。流れは何回にも分け、窓の前に `begin` を呼ぶ。
fn feed_randomly(r: &mut ChaCha8Rng, patch: &mut PeaksPatch, new: &Audio) {
    let frames = new.frames();
    let todo: Vec<usize> = patch.missing().iter().collect();
    let mut i = 0;
    while i < todo.len() {
        if below(r, 2) == 0 {
            let k = todo[i];
            let data: Vec<_> = (0..new.channels())
                .map(|ch| new.chunk_data(ch, k).unwrap())
                .collect();
            let views: Vec<&[f32]> = data.iter().map(|d| &**d).collect();
            patch.feed_chunk(k, &views).unwrap();
            i += 1;
            continue;
        }
        // 続いたチャンクを窓にまとめて流す（窓の大きさは 1・3・STREAM_CHUNKS）
        let w = [1, 3, STREAM_CHUNKS][below(r, 3)];
        let mut j = i + 1;
        while j < todo.len() && j - i < w && todo[j] == todo[j - 1] + 1 {
            j += 1;
        }
        let (s, e) = (todo[i] * C, (todo[j - 1] * C + C).min(frames));
        let planes = new.read_planes(s, e).unwrap();
        let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        let mut sink = patch.partial();
        sink.begin(&StreamInfo {
            rate: new.rate(),
            channels: new.channels(),
            frames: frames as u64,
            meta: new.meta.clone(),
        })
        .unwrap();
        sink.feed(s as u64, &views, &NoMonitor).unwrap();
        sink.finish().unwrap();
        i = j;
    }
}

/// ランダムな値・汚れ・長さの変化・渡し方で、`assemble` が `compute` と同じ。`fresh` も同じ。受け切る前は `Internal`。
#[test]
fn incremental_patch_equals_compute() {
    let mut r = determinism::rng(46);
    for case in 0..48 {
        let channels = 1 + below(&mut r, 3);
        let lens = [1usize, 300, C - 1, C, C + 1, 2 * C + 37, 3 * C, 5 * C + 9];
        let n = lens[below(&mut r, lens.len())];
        let old: Vec<Vec<f32>> = (0..channels)
            .map(|_| {
                (0..n)
                    .map(|_| (r.next_u32() as f32 / u32::MAX as f32) * 2.2 - 1.1)
                    .collect()
            })
            .collect();
        let mut new = old.clone();
        let mut changed = Spans::new();
        for _ in 0..1 + below(&mut r, 3) {
            let s = below(&mut r, n);
            let e = (s + 1 + below(&mut r, 3_000)).min(n);
            for p in &mut new {
                for v in &mut p[s..e] {
                    *v = -*v * 0.5;
                }
            }
            changed.push(
                s.saturating_sub(below(&mut r, 2) * 500) as u64,
                e as u64 + below(&mut r, 2) as u64 * 700,
            );
        }
        match below(&mut r, 3) {
            0 => {
                let m = 1 + below(&mut r, n);
                for p in &mut new {
                    p.truncate(m);
                }
            }
            1 => {
                let extra = 1 + below(&mut r, C + 10);
                for p in &mut new {
                    p.extend((0..extra).map(|i| (i % 100) as f32 * 0.01 - 0.5));
                }
            }
            _ => {}
        }
        let (fa, fb) = (audio(old), audio(new));
        let p = random_params(&mut r);
        let want = peaks_of(&fb, &p);
        let before = Arc::new(peaks_of(&fa, &p));

        let mut patch = PeaksPatch::new(before.clone(), p, fb.frames() as u64, &changed);
        assert_eq!(patch.missing(), *patch.target(), "{case}");
        if !patch.missing().is_empty() {
            let err = patch.assemble(&NoMonitor).unwrap_err();
            assert!(matches!(err, EngineError::Internal(_)), "{case}: {err:?}");
        }
        feed_randomly(&mut r, &mut patch, &fb);
        assert!(patch.missing().is_empty(), "{case}");
        assert_eq!(
            patch.assemble(&NoMonitor).unwrap(),
            want,
            "{case}（{n} → {}・{p:?}）",
            fb.frames()
        );

        // 前の段なし
        let mut fresh = PeaksPatch::fresh(p, fb.rate(), channels as u16, fb.frames() as u64);
        assert_eq!(fresh.missing().count(), fb.frames().div_ceil(C), "{case}");
        feed_randomly(&mut r, &mut fresh, &fb);
        assert_eq!(fresh.assemble(&NoMonitor).unwrap(), want, "{case}: fresh");
    }

    // 形の違うチャンク・流れは断る
    let a = audio(vec![vec![0.25; 10]]);
    let p = PeaksParams::default();
    let mut patch = PeaksPatch::fresh(p, 8_000, 1, 10);
    assert!(patch.feed_chunk(0, &[&[0.0; 9]]).is_err());
    assert!(patch.feed_chunk(0, &[&[0.0; 10], &[0.0; 10]]).is_err());
    let mut sink = patch.partial();
    let bad = StreamInfo {
        rate: 8_000,
        channels: 1,
        frames: 11,
        meta: AudioMeta::default(),
    };
    assert!(sink.begin(&bad).is_err());
    assert!(sink.feed(5, &[&[0.0; 5]], &NoMonitor).is_err());
    patch.feed_chunk(0, &[&[0.25; 10]]).unwrap();
    assert_eq!(patch.assemble(&NoMonitor).unwrap(), peaks_of(&a, &p));
}

/// 計画の出力を `Engine::stream_audio` で区間ごとに何回にも分けて流しても（表示の値のチャンクと混ぜても）、`compute` と同じ。
#[test]
fn partial_patcher_over_many_streams() {
    let engine = Engine::new().unwrap();
    let frames = (2 * STREAM_CHUNKS + 3) * C + 37;
    let input = InputGen::AudioNoise {
        rate: 8_000,
        channels: 2,
        frames: frames as u64,
        seed: 46,
    }
    .generate()
    .unwrap();
    let fin = BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest::default()).unwrap(),
    )]);
    let shapes = BTreeMap::from([(InputId(0), input.shape())]);
    let mut g = Graph::new_single();
    g.push_linear(
        "audio.fade",
        1,
        json!({ "fadeIn": 3 * C as u64 + 5, "fadeOut": 2_000, "curve": "linear" }),
        1,
    )
    .unwrap();
    g.push_linear("audio.gain", 1, json!({ "db": -2.5 }), 2)
        .unwrap();
    let plan = engine.compile(&g, &shapes).unwrap();
    let iv = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source: Digest::default(),
        },
    )]);
    let whole = engine.render(&plan, &iv, &NoMonitor).unwrap();
    let Value::Audio(out) = &whole.outputs[0].value else {
        panic!()
    };
    let mut r = determinism::rng(4646);
    for case in 0..6 {
        let p = random_params(&mut r);
        let want = peaks_of(out, &p);
        let mut patch = PeaksPatch::fresh(p, out.rate(), 2, out.frames() as u64);
        // 先にランダムなチャンクを値から渡す（表示の値のつもり）
        for k in 0..out.chunk_count() {
            if below(&mut r, 4) == 0 {
                let data: Vec<_> = (0..2).map(|ch| out.chunk_data(ch, k).unwrap()).collect();
                let views: Vec<&[f32]> = data.iter().map(|d| &**d).collect();
                patch.feed_chunk(k, &views).unwrap();
            }
        }
        // 残りを、残りの区間の前から 1〜3 窓ずつ流す
        let mut streams = 0;
        while !patch.missing().is_empty() {
            let spans = patch.missing_spans();
            let (a, _) = spans.spans()[0];
            let take = (1 + below(&mut r, 3)) as u64 * (STREAM_CHUNKS * C) as u64;
            let part = Spans::one(a, (a + take).min(out.frames() as u64));
            engine
                .stream_audio(
                    &plan,
                    &fin,
                    &mut NodeCache::new(u64::MAX),
                    OutputId(0),
                    Some(&part),
                    &mut patch.partial(),
                    &NoMonitor,
                )
                .unwrap();
            streams += 1;
            assert!(streams < 64, "{case}: 終わりません");
        }
        assert_eq!(patch.assemble(&NoMonitor).unwrap(), want, "{case}: {p:?}");
    }
    // 区間の変わった手のあと：前の段を共有して、推論の区間だけを流す
    let mut g2 = g.clone();
    g2.nodes[1].params = json!({ "db": -2.5 });
    g2.nodes[0].params = json!({ "fadeIn": C as u64, "fadeOut": 2_000, "curve": "linear" });
    let plan2 = engine.compile(&g2, &shapes).unwrap();
    let changed = match &engine.delta(&plan, &fin, &plan2, &fin).unwrap().outputs[0].1 {
        Area::Samples(s) => s.clone(),
        _ => Spans::one(0, frames as u64),
    };
    let new = engine.render(&plan2, &iv, &NoMonitor).unwrap();
    let Value::Audio(new) = &new.outputs[0].value else {
        panic!()
    };
    let p = PeaksParams::default();
    let mut patch = PeaksPatch::new(
        Arc::new(peaks_of(out, &p)),
        p,
        new.frames() as u64,
        &changed,
    );
    assert!(patch.target().count() < out.chunk_count(), "{changed:?}");
    let spans = patch.missing_spans();
    engine
        .stream_audio(
            &plan2,
            &fin,
            &mut NodeCache::new(u64::MAX),
            OutputId(0),
            Some(&spans),
            &mut patch.partial(),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(patch.assemble(&NoMonitor).unwrap(), peaks_of(new, &p));
}
