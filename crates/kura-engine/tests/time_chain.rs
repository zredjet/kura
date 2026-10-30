//! 時間の写しの連鎖と逆の写し（M13 段 4。docs/editing.md §17 の E45）：`Plan::time_chain` が計画の順・形と合い、
//! `TimeMap::back`・`back_end` が `pos`・`pos_end` の逆になる（`Segments` は範囲の中で厳密、`Rate` は丸めの分まで）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::determinism;
use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::mapping::{Seg, TimeMap, map_audio_meta};
use kura_engine::render::Engine;
use kura_engine::shape::{Dim, Shape};
use kura_engine::value::{AudioMeta, LoopRange};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::json;

fn below(r: &mut ChaCha8Rng, n: u64) -> u64 {
    ((u128::from(r.next_u64()) * u128::from(n.max(1))) >> 64) as u64
}

fn audio(rate: u32, channels: u16, frames: u64) -> Shape {
    Shape::Audio {
        rate,
        channels,
        frames: Dim::Exact(frames),
    }
}

fn exact_frames(s: &Shape) -> u64 {
    match s {
        Shape::Audio {
            frames: Dim::Exact(n),
            ..
        } => *n,
        other => panic!("長さの決まった音声ではありません: {other:?}"),
    }
}

/// ランダムな線形の音声の文書（trim・resample・fade・gain・set_loop・mono。3 回に 1 回ほど外す）。長さは推論で追う。
fn random_doc(r: &mut ChaCha8Rng, input: &Shape) -> Graph {
    let mut g = Graph::new_single();
    let (mut rate, mut frames) = match input {
        Shape::Audio { rate, .. } => (*rate, exact_frames(input)),
        _ => unreachable!(),
    };
    let n = 1 + below(r, 6);
    for i in 0..n {
        let seed = i + 1;
        match below(r, 6) {
            0 if frames > 4 => {
                let start = below(r, frames / 2);
                let end = (below(r, 2) == 0).then(|| start + 1 + below(r, frames - start - 1));
                g.push_linear("audio.trim", 1, json!({ "start": start, "end": end }), seed)
                    .unwrap();
                if below(r, 3) != 0 {
                    frames = end.unwrap_or(frames) - start;
                }
            }
            1 => {
                let to = [8_000, 11_025, 16_000, 22_050, 44_100, 48_000][below(r, 6) as usize];
                g.push_linear("audio.resample", 1, json!({ "rate": to }), seed)
                    .unwrap();
                if below(r, 3) != 0 {
                    frames =
                        (u128::from(frames) * u128::from(to)).div_ceil(u128::from(rate)) as u64;
                    rate = to;
                }
            }
            2 => {
                let fin = below(r, frames / 2 + 1);
                g.push_linear(
                    "audio.fade",
                    1,
                    json!({ "fadeIn": fin, "fadeOut": 0, "curve": "linear" }),
                    seed,
                )
                .unwrap();
            }
            3 => {
                g.push_linear("audio.gain", 1, json!({ "db": -3.0 }), seed)
                    .unwrap();
            }
            4 if frames > 2 => {
                let s = below(r, frames - 1);
                g.push_linear(
                    "audio.set_loop",
                    1,
                    json!({ "loop": { "start": s, "end": s + 1 + below(r, frames - s - 1) } }),
                    seed,
                )
                .unwrap();
            }
            _ => {
                g.push_linear("audio.mono", 1, json!({}), seed).unwrap();
            }
        }
        if below(r, 3) == 0
            && let Some(last) = g.nodes.last_mut()
        {
            last.enabled = false;
        }
    }
    g
}

/// 連鎖が計画の順・形と合う：ノードの数・順、各ノードの入力の形が前の出力、出力の形が推論の形、外したノードは
/// `Identity`、trim は `Segments`・resample は `Rate`（有効でレートが変わるとき）。順に写した位置を逆に戻すと元に近い。
#[test]
fn chain_follows_the_plan() {
    let engine = Engine::new().unwrap();
    let mut r = determinism::rng(0x45_7469_6d65);
    let mut seen = 0;
    for case in 0..200 {
        let rate = [8_000, 22_050, 44_100, 48_000][below(&mut r, 4) as usize];
        let input = audio(
            rate,
            1 + below(&mut r, 2) as u16,
            1 + below(&mut r, 400_000),
        );
        let g = random_doc(&mut r, &input);
        let Ok(plan) = engine.compile(&g, &BTreeMap::from([(InputId(0), input)])) else {
            continue;
        };
        seen += 1;
        let chain = plan
            .time_chain(OutputId(0))
            .expect("線形の文書は連鎖になる");
        assert_eq!(chain.len(), g.nodes.len(), "case {case}");
        let mut prev = input;
        for (link, node) in chain.iter().zip(&g.nodes) {
            assert_eq!(link.node, node.id, "case {case}");
            assert_eq!(link.op, node.op, "case {case}");
            assert_eq!(link.enabled, node.enabled, "case {case}");
            assert_eq!(link.input, prev, "case {case}");
            assert_eq!(Some(link.output), plan.node_shape(node.id), "case {case}");
            if !node.enabled {
                assert_eq!(link.time, TimeMap::Identity, "case {case}");
            }
            match (node.op.as_str(), node.enabled) {
                ("audio.trim", true) => {
                    assert!(matches!(link.time, TimeMap::Segments { .. }), "case {case}")
                }
                ("audio.resample", true) => {
                    let (Shape::Audio { rate: a, .. }, Shape::Audio { rate: b, .. }) =
                        (link.input, link.output)
                    else {
                        unreachable!()
                    };
                    if a != b {
                        assert_eq!(link.time, TimeMap::Rate { from: a, to: b }, "case {case}");
                    }
                }
                _ => {}
            }
            // 入力の範囲の中の位置を写して戻す：Segments は厳密、Rate は丸めの分まで
            let n = exact_frames(&link.input);
            for _ in 0..8 {
                let x = below(&mut r, n);
                if let Some(y) = link.time.pos(x) {
                    let back = link.time.back(y).expect("写せた位置は戻せる");
                    match link.time {
                        TimeMap::Rate { from, to } => assert!(
                            u128::from(back.abs_diff(x)) * 2 * u128::from(to)
                                <= u128::from(from) + u128::from(to),
                            "case {case}"
                        ),
                        _ => assert_eq!(back, x, "case {case}"),
                    }
                }
            }
            prev = link.output;
        }
        assert_eq!(prev, plan.output_shapes()[0].1, "case {case}");
    }
    assert!(seen > 100, "計画にできた文書が少なすぎます（{seen}）");

    // 出力が入力そのものなら空、知らない出力は None
    let plan = engine
        .compile(
            &Graph::new_single(),
            &BTreeMap::from([(InputId(0), audio(8_000, 1, 10))]),
        )
        .unwrap();
    assert_eq!(plan.time_chain(OutputId(0)), Some(Vec::new()));
    assert_eq!(plan.time_chain(OutputId(7)), None);
}

/// ランダムな重ならない区間の写し。
fn random_segments(r: &mut ChaCha8Rng) -> Vec<Seg> {
    let mut segs = Vec::new();
    let (mut src, mut dst) = (below(r, 1_000), below(r, 1_000));
    for _ in 0..1 + below(r, 4) {
        let len = 1 + below(r, 100_000);
        segs.push(Seg {
            src_start: src,
            src_end: src + len,
            dst_start: dst,
        });
        src += len + below(r, 5_000);
        dst += len + below(r, 2) * below(r, 5_000);
    }
    segs
}

/// `Segments`：範囲の中で `back(pos(x)) == x`、`back_end(pos_end(e)) == e`。写した先の区間の外は None。
#[test]
fn back_inverts_segments() {
    let mut r = determinism::rng(0x45_6261_636b);
    for case in 0..500 {
        let segs = random_segments(&mut r);
        let map = TimeMap::Segments { segs: segs.clone() };
        for s in &segs {
            for _ in 0..16 {
                let x = s.src_start + below(&mut r, s.src_end - s.src_start);
                let y = map.pos(x).unwrap();
                assert_eq!(map.back(y), Some(x), "case {case}");
                let e = x + 1;
                let ye = map.pos_end(e).unwrap();
                assert_eq!(map.back_end(ye), Some(e), "case {case}");
            }
            // 区間の終わりちょうど
            let end = map.pos_end(s.src_end).unwrap();
            assert_eq!(map.back_end(end), Some(s.src_end), "case {case}");
        }
        // どの区間にも入らない写した先の位置は None
        let last = segs.last().unwrap();
        let past = last.dst_start + (last.src_end - last.src_start);
        let max_dst = segs
            .iter()
            .map(|s| s.dst_start + (s.src_end - s.src_start))
            .max()
            .unwrap();
        if past == max_dst {
            assert_eq!(map.back(max_dst), None, "case {case}");
            assert_eq!(map.back_end(max_dst + 1), None, "case {case}");
        }
    }
    assert_eq!(TimeMap::Identity.back(17), Some(17));
    assert_eq!(TimeMap::Identity.back_end(17), Some(17));
}

/// `Rate { from, to }`：`back(pos(x))` は x から from/(2·to) + 1/2 まで（上げるレートでは厳密）。`Unknown`・`Reset` は None。
#[test]
fn rate_back_is_within_half_a_step() {
    let mut r = determinism::rng(0x45_7261_7465);
    let rates = [
        1, 3, 7, 8_000, 11_025, 22_050, 44_100, 48_000, 96_000, 192_000, 384_000,
    ];
    for case in 0..2_000 {
        let from = rates[below(&mut r, rates.len() as u64) as usize];
        let to = rates[below(&mut r, rates.len() as u64) as usize];
        let map = TimeMap::Rate { from, to };
        let x = below(&mut r, 1 << 32);
        let y = map.pos(x).unwrap();
        let back = map.back(y).unwrap();
        let diff = u128::from(back.abs_diff(x));
        assert!(
            diff * 2 * u128::from(to) <= u128::from(from) + u128::from(to),
            "case {case}: {from}→{to} x={x} y={y} back={back}"
        );
        if from <= to {
            assert_eq!(back, x, "case {case}: 上げるレートでは厳密");
        }
        assert_eq!(map.back_end(y), map.back(y), "case {case}");
    }
    for map in [TimeMap::Unknown, TimeMap::Reset] {
        assert_eq!(map.back(5), None);
        assert_eq!(map.back_end(5), None);
    }
}

/// `pos_end` はループの終わりの写し（`map_audio_meta`）と同じ。
#[test]
fn pos_end_matches_the_loop_mapping() {
    let mut r = determinism::rng(0x4565_6e64);
    for case in 0..500 {
        let map = if below(&mut r, 2) == 0 {
            TimeMap::Segments {
                segs: random_segments(&mut r),
            }
        } else {
            TimeMap::Rate {
                from: 1 + below(&mut r, 96_000) as u32,
                to: 1 + below(&mut r, 96_000) as u32,
            }
        };
        let (start, end) = match &map {
            TimeMap::Segments { segs } => {
                let s = segs[below(&mut r, segs.len() as u64) as usize];
                let a = s.src_start + below(&mut r, s.src_end - s.src_start);
                (a, a + 1 + below(&mut r, s.src_end - a))
            }
            _ => {
                let a = below(&mut r, 1 << 30);
                (a, a + 1 + below(&mut r, 1 << 20))
            }
        };
        let meta = AudioMeta {
            loop_range: Some(LoopRange { start, end }),
            ..AudioMeta::default()
        };
        let (out, _) = map_audio_meta(&meta, &map, u64::MAX);
        if let Some(l) = out.loop_range {
            assert_eq!(Some(l.end), map.pos_end(end), "case {case}");
            assert_eq!(Some(l.start), map.pos(start), "case {case}");
        }
    }
}
