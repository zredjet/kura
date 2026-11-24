//! M17 段 4：内部のポート（ノードの入力 0・出力）を区間で流す口（`Engine::stream_node_port`）と、その指紋
//! （`Engine::port_fingerprint`）。docs/editing.md §18.4（スペクトルの「前と後」）。
//!
//! - ports_stream_like_the_render：gain → fade → 外した gain → `audio.eq` → gain の鎖で、各ノードの入力・出力を区間（チャンクの
//!   途中から・チャンクをまたぐ・離れた 2 つ・最後の半端なチャンク）と全体で流した値が、そのノードまでの文書を全体で
//!   `render` した値とビット一致する（窓は `stream_windows` のもの・メタデータも同じ・窓の中で解き直さない）。
//! - downstream_of_the_table_waits_for_the_solve：`audio.eq`（状態の表）より下流のポートは、表がキャッシュにないと流さずに
//!   `Unsupported`（`missing_port_solves` に仕事が 1 つ）、上流のポートは流せる。`PresolveJob` で解いたあとは下流も流せる。
//! - port_fingerprints_match：`port_fingerprint` が `fingerprints` の `nodes`・入力の指紋と合い、そのノードで切った文書の出力の
//!   指紋とも同じ。外したノードの出力は入力と同じ指紋・同じ値。別のエンジンの計画は断る。
//! - regions_stream_with_fresh_caches：窓が 2 つになる長さで、区間ごとに新しいキャッシュ（解いた表は外の解析で入れる）で流し、
//!   範囲の描画を本当に通る（`region_nodes > 0`）こと、窓の中で解析を解かない（`analysis_solved` が増えない）こと、値が全体の
//!   描画と同じことを確かめる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::audio::stream::{AudioSink, StreamInfo, stream_windows};
use kura_engine::doc::{Graph, InputId, NodeId, OutputId};
use kura_engine::ops::audio::eq::{BandChannel, BandType, EqParams, Phase, band};
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::port::PortSide;
use kura_engine::render::presolve::{MissingSolves, PresolveJob};
use kura_engine::render::region::Spans;
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::testgen::InputGen;
use kura_engine::testing::stateful::engine as stateful_engine;
use kura_engine::value::AUDIO_CHUNK;
use kura_engine::{Digest, EngineError, Monitor, NoMonitor, Value};
use serde_json::json;

const C: u64 = AUDIO_CHUNK as u64;
/// 入力の長さ（4 チャンク。最後は半端）。
const FRAMES: u64 = 3 * C + 123;
/// 外したノードの位置（鎖の番号）。
const DISABLED: usize = 2;
/// `audio.eq` の位置（鎖の番号）。
const EQ: usize = 3;

fn noise(channels: u16, frames: u64, seed: u64) -> Value {
    InputGen::AudioNoise {
        rate: 8_000,
        channels,
        frames,
        seed,
    }
    .generate()
    .unwrap()
}

fn eq_ms() -> serde_json::Value {
    let mut mid = band(BandType::Bell, 300.0, 6.0, 2.0);
    mid.channel = BandChannel::Mid;
    let mut side = band(BandType::HighShelf, 1_500.0, -4.0, 0.7);
    side.channel = BandChannel::Side;
    serde_json::to_value(EqParams {
        bands: vec![mid, band(BandType::LowCut, 60.0, 0.0, 0.7), side],
        phase: Phase::Minimum,
        gain_db: -1.0,
    })
    .unwrap()
}

/// 鎖の操作（ID・引数）。`DISABLED` は外し、`EQ` は状態の表を申告する。
fn ops() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("audio.gain", json!({ "db": -3.0 })),
        (
            "audio.fade",
            json!({ "fadeIn": C / 2 + 17, "fadeOut": C + 5, "curve": "equalPower" }),
        ),
        ("audio.gain", json!({ "db": 9.0 })),
        ("audio.eq", eq_ms()),
        ("audio.gain", json!({ "db": 1.5 })),
    ]
}

/// 鎖の頭から `len` 個のノードの文書（種は位置で決まるので、どの長さでも同じノードは同じ指紋）。
fn doc(len: usize) -> Graph {
    let mut g = Graph::new_single();
    for (i, (op, p)) in ops().into_iter().take(len).enumerate() {
        g.push_linear(op, 1, p, i as u64 + 1).unwrap();
        if i == DISABLED {
            g.nodes[i].enabled = false;
        }
    }
    g
}

fn compile(engine: &Engine, g: &Graph, input: &Value) -> Plan {
    engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .unwrap()
}

fn fin(input: &Value) -> BTreeMap<InputId, FingerprintedInput> {
    BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest([3; 32])).unwrap(),
    )])
}

fn render(engine: &Engine, plan: &Plan, input: &Value) -> Value {
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source: Digest([3; 32]),
        },
    )]);
    engine
        .render(plan, &plain, &NoMonitor)
        .unwrap()
        .outputs
        .swap_remove(0)
        .value
}

/// 各ノードの（入力 0, 出力）の参照の値：そのノードまでの文書を全体で `render` したもの。
fn references(engine: &Engine, input: &Value) -> Vec<(Value, Value)> {
    let mut out = Vec::new();
    let mut prev = input.clone();
    for len in 1..=ops().len() {
        let v = render(engine, &compile(engine, &doc(len), input), input);
        out.push((prev, v.clone()));
        prev = v;
    }
    out
}

/// 流す区間（None は全体）。
fn spans_list() -> Vec<(&'static str, Option<Spans>)> {
    let mut two = Spans::one(5, 6);
    two.push(2 * C + 100, 3 * C + 50);
    vec![
        ("全体", None),
        (
            "チャンクの途中からまたぐ",
            Some(Spans::one(C / 2, C + C / 3)),
        ),
        ("境の 2 フレーム", Some(Spans::one(2 * C - 1, 2 * C + 1))),
        ("離れた 2 つ", Some(two)),
        (
            "最後の半端なチャンク",
            Some(Spans::one(3 * C + 7, 3 * C + 9)),
        ),
    ]
}

/// 流れを集める。
#[derive(Default)]
struct Collect {
    info: Option<StreamInfo>,
    windows: Vec<(u64, Vec<Vec<f32>>)>,
    finished: bool,
}

impl AudioSink for Collect {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        assert!(self.info.is_none(), "begin が 2 度");
        self.info = Some(info.clone());
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        _mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.windows
            .push((start, planes.iter().map(|p| p.to_vec()).collect()));
        Ok(())
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        self.finished = true;
        Ok(())
    }
}

/// 集めた流れが参照 `want` の `spans` の窓とビット一致する。
fn check(sink: &Collect, want: &Value, spans: Option<&Spans>, what: &str) {
    let Value::Audio(a) = want else {
        panic!("{what}：参照が音声ではありません");
    };
    let info = sink.info.as_ref().expect("begin がない");
    assert!(sink.finished, "{what}：finish がない");
    assert_eq!(info.frames, a.frames() as u64, "{what}：長さ");
    assert_eq!(info.rate, a.rate(), "{what}：レート");
    assert_eq!(info.channels, a.channels(), "{what}：チャンネル");
    assert_eq!(info.meta, a.meta, "{what}：メタデータ");
    let got: Vec<(u64, u64)> = sink
        .windows
        .iter()
        .map(|(s, p)| (*s, s + p[0].len() as u64))
        .collect();
    assert_eq!(got, stream_windows(info.frames, spans), "{what}：窓");
    if let Some(spans) = spans {
        for &(x, y) in spans.spans() {
            let y = y.min(info.frames);
            assert!(
                x >= y || got.iter().any(|(s, e)| *s <= x && y <= *e),
                "{what}：[{x}, {y}) を覆う窓がない"
            );
        }
    }
    for (s, planes) in &sink.windows {
        let e = s + planes[0].len() as u64;
        let w = a.read_planes(*s as usize, e as usize).unwrap();
        assert_eq!(planes.len(), w.len(), "{what}：チャンネル");
        for (ch, (p, q)) in planes.iter().zip(&w).enumerate() {
            if let Some(i) = p
                .iter()
                .zip(q)
                .position(|(x, y)| x.to_bits() != y.to_bits())
            {
                panic!(
                    "{what}：チャンネル {ch} のフレーム {} が違う（{} と {}）",
                    s + i as u64,
                    p[i],
                    q[i]
                );
            }
        }
    }
}

/// ポートを流して集める。
#[allow(clippy::too_many_arguments)]
fn stream(
    engine: &Engine,
    plan: &Plan,
    input: &Value,
    cache: &mut NodeCache,
    node: NodeId,
    side: PortSide,
    spans: Option<&Spans>,
) -> Result<(Collect, u64), EngineError> {
    let mut sink = Collect::default();
    let stats = engine.stream_node_port(
        plan,
        &fin(input),
        cache,
        node,
        side,
        spans,
        &mut sink,
        &NoMonitor,
    )?;
    assert_eq!(
        stats.windows,
        sink.windows.len() as u64,
        "渡した窓の数と数えた窓の数"
    );
    Ok((sink, stats.area.presolved))
}

/// 仕事をすべて解く。
fn solve(engine: &Engine, plan: &Plan, input: &Value, cache: &mut NodeCache, m: MissingSolves) {
    let mut job = PresolveJob::new(m);
    let mut steps = 0;
    while !job.is_done() {
        job.step(engine, plan, &fin(input), cache, 2, &NoMonitor)
            .unwrap();
        steps += 1;
        assert!(steps < 1_000, "終わらない");
    }
}

fn sides() -> [PortSide; 2] {
    [PortSide::Input, PortSide::Output]
}

/// 各ノードの入力・出力を区間と全体で流した値が、そのノードまでの文書を全体で描いた値とビット一致する（表を解いたあと）。
#[test]
fn ports_stream_like_the_render() {
    let engine = stateful_engine().unwrap();
    let input = noise(2, FRAMES, 23);
    let g = doc(ops().len());
    let plan = compile(&engine, &g, &input);
    let refs = references(&engine, &input);
    let mut cache = NodeCache::new(512 << 20);
    let missing = engine
        .missing_solves(&plan, &fin(&input), &cache, OutputId(0))
        .unwrap();
    assert_eq!(missing.tasks.len(), 1, "audio.eq の表");
    solve(&engine, &plan, &input, &mut cache, missing);
    for (i, node) in g.nodes.iter().enumerate() {
        for side in sides() {
            let want = match side {
                PortSide::Input => &refs[i].0,
                PortSide::Output => &refs[i].1,
            };
            for (name, spans) in spans_list() {
                let what = format!("ノード {i}（{}）の {side:?}・{name}", node.op);
                let (sink, presolved) = stream(
                    &engine,
                    &plan,
                    &input,
                    &mut cache,
                    node.id,
                    side,
                    spans.as_ref(),
                )
                .unwrap_or_else(|e| panic!("{what}：{e}"));
                assert_eq!(presolved, 0, "{what}：窓の中で解き直した");
                check(&sink, want, spans.as_ref(), &what);
            }
        }
    }
    // 計画の出力（最後のノードの出力）は全体の文書の描画と同じ
    let last = g.nodes.last().unwrap().id;
    let (sink, _) = stream(
        &engine,
        &plan,
        &input,
        &mut cache,
        last,
        PortSide::Output,
        None,
    )
    .unwrap();
    check(&sink, &render(&engine, &plan, &input), None, "計画の出力");
}

/// `audio.eq` より下流のポートは、表がキャッシュにないと流さずに断り、`PresolveJob` で解いたあとは流せる。上流のポートは
/// 表がなくても流せる。
#[test]
fn downstream_of_the_table_waits_for_the_solve() {
    let engine = stateful_engine().unwrap();
    let input = noise(2, FRAMES, 31);
    let g = doc(ops().len());
    let plan = compile(&engine, &g, &input);
    let refs = references(&engine, &input);
    let mut cache = NodeCache::new(512 << 20);
    let spans = Spans::one(C - 3, C + 3);
    let eq = g.nodes[EQ].id;
    for (i, node) in g.nodes.iter().enumerate() {
        for side in sides() {
            let what = format!("ノード {i}（{}）の {side:?}", node.op);
            let downstream = i > EQ || (i == EQ && side == PortSide::Output);
            let missing = engine
                .missing_port_solves(&plan, &fin(&input), &cache, node.id, side)
                .unwrap();
            assert!(!missing.unstreamable, "{what}");
            assert_eq!(
                missing.tasks.len(),
                usize::from(downstream),
                "{what}：解く仕事"
            );
            match stream(
                &engine,
                &plan,
                &input,
                &mut cache,
                node.id,
                side,
                Some(&spans),
            ) {
                Err(EngineError::Unsupported(_)) if downstream => {}
                Ok((sink, presolved)) if !downstream => {
                    assert_eq!(presolved, 0, "{what}");
                    let want = match side {
                        PortSide::Input => &refs[i].0,
                        PortSide::Output => &refs[i].1,
                    };
                    check(&sink, want, Some(&spans), &what);
                }
                Ok(_) => panic!("{what}：表がないのに流した"),
                Err(e) => panic!("{what}：{e}"),
            }
        }
    }
    // 断っても解いていない
    let missing = engine
        .missing_port_solves(&plan, &fin(&input), &cache, eq, PortSide::Output)
        .unwrap();
    assert_eq!(missing.tasks.len(), 1);
    assert_eq!(missing.tasks[0].node, eq);
    // 出力からの仕事と同じ鍵
    assert_eq!(
        missing.keys(),
        engine
            .missing_solves(&plan, &fin(&input), &cache, OutputId(0))
            .unwrap()
            .keys()
    );
    solve(&engine, &plan, &input, &mut cache, missing);
    for (i, node) in g.nodes.iter().enumerate().skip(EQ) {
        for side in sides() {
            let what = format!("解いたあとのノード {i}（{}）の {side:?}", node.op);
            assert!(
                engine
                    .missing_port_solves(&plan, &fin(&input), &cache, node.id, side)
                    .unwrap()
                    .tasks
                    .is_empty(),
                "{what}"
            );
            let (sink, presolved) = stream(&engine, &plan, &input, &mut cache, node.id, side, None)
                .unwrap_or_else(|e| panic!("{what}：{e}"));
            assert_eq!(presolved, 0, "{what}");
            let want = match side {
                PortSide::Input => &refs[i].0,
                PortSide::Output => &refs[i].1,
            };
            check(&sink, want, None, &what);
        }
    }
}

/// `port_fingerprint` が `fingerprints` の `nodes`・入力の指紋と合い、外したノードの出力は入力と同じ指紋・同じ値。
#[test]
fn port_fingerprints_match() {
    let engine = stateful_engine().unwrap();
    let input = noise(1, FRAMES, 47);
    let g = doc(ops().len());
    let plan = compile(&engine, &g, &input);
    let inputs = fin(&input);
    let fps = engine.fingerprints(&plan, &inputs).unwrap();
    let mut prev = inputs[&InputId(0)].fingerprint;
    for (i, node) in g.nodes.iter().enumerate() {
        let fp_in = engine
            .port_fingerprint(&plan, &inputs, node.id, PortSide::Input)
            .unwrap();
        let fp_out = engine
            .port_fingerprint(&plan, &inputs, node.id, PortSide::Output)
            .unwrap();
        assert_eq!(fp_in, prev, "ノード {i} の入力");
        assert_eq!(fp_out, fps.nodes[&node.id], "ノード {i} の出力");
        // そのノードで切った文書の出力の指紋と同じ（同じ指紋なら同じ値）
        let cut = compile(&engine, &doc(i + 1), &input);
        let cut_fps = engine.fingerprints(&cut, &inputs).unwrap();
        assert_eq!(fp_out, cut_fps.outputs[0].1, "ノード {i}：切った文書の出力");
        if i == DISABLED {
            assert_eq!(fp_out, fp_in, "外したノードの出力は入力と同じ指紋");
        } else {
            assert_ne!(fp_out, fp_in, "ノード {i}：有効なノードは指紋を変える");
        }
        prev = fp_out;
    }
    // 外したノードの出力は入力と同じ値（表のない上流なので、そのまま流せる）
    let off = g.nodes[DISABLED].id;
    let mut cache = NodeCache::new(512 << 20);
    let (a, _) = stream(
        &engine,
        &plan,
        &input,
        &mut cache,
        off,
        PortSide::Input,
        None,
    )
    .unwrap();
    let (b, _) = stream(
        &engine,
        &plan,
        &input,
        &mut cache,
        off,
        PortSide::Output,
        None,
    )
    .unwrap();
    assert_eq!(a.windows.len(), b.windows.len());
    for ((s, p), (t, q)) in a.windows.iter().zip(&b.windows) {
        assert_eq!(s, t);
        let bits = |v: &Vec<Vec<f32>>| -> Vec<Vec<u32>> {
            v.iter()
                .map(|c| c.iter().map(|x| x.to_bits()).collect())
                .collect()
        };
        assert!(bits(p) == bits(q), "外したノードの出力が入力と違う");
    }
    // 別のエンジンの計画は断る（ほかの口と揃える）
    let other = Engine::new().unwrap();
    let plain = doc(2);
    let foreign = compile(&other, &plain, &input);
    assert!(matches!(
        engine.port_fingerprint(&foreign, &inputs, plain.nodes[0].id, PortSide::Output),
        Err(EngineError::Internal(_))
    ));
    // 計画にないノードは断る
    for side in sides() {
        assert!(matches!(
            engine.port_fingerprint(&plan, &inputs, NodeId(999), side),
            Err(EngineError::Invalid(_))
        ));
        let mut sink = Collect::default();
        assert!(matches!(
            engine.stream_node_port(
                &plan,
                &inputs,
                &mut cache,
                NodeId(999),
                side,
                None,
                &mut sink,
                &NoMonitor
            ),
            Err(EngineError::Invalid(_))
        ));
    }
}

/// 窓が 2 つになる長さ（17 チャンク余り）。
const LONG_FRAMES: u64 = 17 * C + 123;

/// 解いた表（`needed` の鍵）を外の解析として入れた新しいキャッシュ。
fn fresh_with_tables(
    base: &NodeCache,
    needed: &[(kura_engine::render::cache::Fingerprint, u64)],
) -> NodeCache {
    let mut cache = NodeCache::new(512 << 20);
    for (key, bytes) in needed {
        let out = base.peek_analysis(key).expect("解いた表");
        cache.register_external_analysis(*key, out, *bytes);
    }
    cache
}

/// 窓が 2 つになる長さで、区間ごとに新しいキャッシュで流す：範囲の描画を通り、窓の中で解析を解かず、値は全体の描画と同じ。
#[test]
fn regions_stream_with_fresh_caches() {
    let engine = stateful_engine().unwrap();
    let input = noise(2, LONG_FRAMES, 53);
    let g = doc(ops().len());
    let plan = compile(&engine, &g, &input);
    let refs = references(&engine, &input);
    let mut base = NodeCache::new(512 << 20);
    let missing = engine
        .missing_solves(&plan, &fin(&input), &base, OutputId(0))
        .unwrap();
    let needed = missing.needed.clone();
    solve(&engine, &plan, &input, &mut base, missing);
    let mut two = Spans::one(3 * C + 5, 3 * C + 9);
    two.push(16 * C - 40, 16 * C + 40);
    let spans = [
        ("窓の境をまたぐ", Spans::one(16 * C - 5, 16 * C + 5)),
        ("チャンクの途中から", Spans::one(C / 2, C + C / 3)),
        ("離れた 2 つ", two),
        ("最後の半端なチャンク", Spans::one(17 * C + 7, 17 * C + 9)),
    ];
    for (i, node) in g.nodes.iter().enumerate() {
        for side in sides() {
            let want = match side {
                PortSide::Input => &refs[i].0,
                PortSide::Output => &refs[i].1,
            };
            for (name, spans) in &spans {
                let what = format!("ノード {i}（{}）の {side:?}・{name}", node.op);
                let mut cache = fresh_with_tables(&base, &needed);
                let before = cache.stats();
                let mut sink = Collect::default();
                let stats = engine
                    .stream_node_port(
                        &plan,
                        &fin(&input),
                        &mut cache,
                        node.id,
                        side,
                        Some(spans),
                        &mut sink,
                        &NoMonitor,
                    )
                    .unwrap_or_else(|e| panic!("{what}：{e}"));
                // 計画の入力そのもの（最初のノードの入力）は描くノードがない
                if !(i == 0 && side == PortSide::Input) {
                    assert!(stats.area.region_nodes > 0, "{what}：範囲の描画を通らない");
                }
                assert_eq!(stats.area.presolved, 0, "{what}：流し解いた");
                assert_eq!(
                    cache.stats().since(&before).analysis_solved,
                    0,
                    "{what}：窓の中で解析を解いた"
                );
                check(&sink, want, Some(spans), &what);
            }
        }
    }
}
