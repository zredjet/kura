//! メモリの見積もりと予算（docs/editing.md §12、§17 の E25、NFR-17）。
//!
//! 見積もり（`Engine::estimate`）が、描いて得た値で数えた生きている値以上であること、操作の作業領域（持ち上げたノードは
//! フレームの形）と読み書きの作業領域を数えること、キャッシュに当たる分を引くこと。予算の照らし（`Engine::admit`・
//! `Engine::admit_cached`）が、全体を超える計画を描く前に断り（操作は 1 つも動かない）、一括は待ち（中止できる）、エディタは
//! 待たずにキャッシュを追い出して確かめ直すこと。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::budget::{Class, MemoryBudget, default_reserved};
use kura_engine::codec::CodecSet;
use kura_engine::doc::{
    Dither, Graph, InputId, Node, NodeId, OutputId, OutputSpec, PortRef, QUANTIZE_VERSION, Quantize,
};
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::monitor::{CancelAfter, CancelFlag};
use kura_engine::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamSchema, PortSpec, Registry, Signature, Work,
};
use kura_engine::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use kura_engine::render::estimate::{
    Admission, CacheLease, CacheProbe, EstimateSpec, MemoryEstimate, Sink, Source,
};
use kura_engine::render::{Engine, InputValue, Plan, Rendered};
use kura_engine::shape::{Dim, Shape, ValueKind};
use kura_engine::testgen::InputGen;
use kura_engine::testing::{single_node_graph, source_of};
use kura_engine::value::PixelFormat;
use kura_engine::{Digest, EngineError, Issues, Limits, NoMonitor, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

const MIB: u64 = 1 << 20;

fn engine() -> Engine {
    Engine::new().unwrap()
}

fn inputs_of(value: Value, source: Digest) -> BTreeMap<InputId, InputValue> {
    BTreeMap::from([(InputId(0), InputValue { value, source })])
}

fn shapes_of(inputs: &BTreeMap<InputId, InputValue>) -> BTreeMap<InputId, Shape> {
    inputs.iter().map(|(k, v)| (*k, v.value.shape())).collect()
}

fn fingerprinted(inputs: &BTreeMap<InputId, InputValue>) -> BTreeMap<InputId, FingerprintedInput> {
    inputs
        .iter()
        .map(|(k, v)| {
            (
                *k,
                FingerprintedInput::new(v.value.clone(), v.source).unwrap(),
            )
        })
        .collect()
}

/// 512 × 512 の RGBA8 の乱数（ちょうど 1 MiB）。
fn mib_image(seed: u64) -> Value {
    InputGen::Noise {
        w: 512,
        h: 512,
        fmt: PixelFormat::Rgba8,
        seed,
    }
    .generate()
    .unwrap()
}

fn node(id: u32, op: &str, params: serde_json::Value, from: PortRef, enabled: bool) -> Node {
    Node {
        id: NodeId(id),
        op: op.into(),
        v: 1,
        params,
        inputs: vec![from],
        seed: u64::from(id),
        enabled,
    }
}

fn at(id: u32) -> PortRef {
    PortRef::Node { id: NodeId(id) }
}

const INPUT: PortRef = PortRef::Input { id: InputId(0) };

/// ノードと出力（出力の ID・元・量子化）から入力 1 つの文書を作る。
fn graph(nodes: Vec<Node>, outputs: Vec<(u16, PortRef, Option<Quantize>)>) -> Graph {
    graph_with_inputs(Graph::new_single(), nodes, outputs)
}

/// `g` の入力のまま、ノードと出力を差し替える。
fn graph_with_inputs(
    mut g: Graph,
    nodes: Vec<Node>,
    outputs: Vec<(u16, PortRef, Option<Quantize>)>,
) -> Graph {
    let template = g.outputs[0].clone();
    g.next_id = nodes.iter().map(|n| n.id.0 + 1).max().unwrap_or(1);
    g.nodes = nodes;
    g.outputs = outputs
        .into_iter()
        .map(|(id, from, quantize)| OutputSpec {
            id: OutputId(id),
            from,
            quantize,
            ..template.clone()
        })
        .collect();
    g
}

/// 値の集まりが実際に持つバイト数（共有した実体は 1 度だけ）。
fn unique_bytes<'a>(values: impl IntoIterator<Item = &'a Value>) -> u64 {
    let mut counter = NodeCache::new(u64::MAX);
    for v in values {
        counter.register_external(v);
    }
    counter.bytes()
}

/// 描いた値で数えた、描く間に生きている値の最大（共有した実体は 1 度だけ）。
fn actual_peak(
    engine: &Engine,
    g: &Graph,
    plan: &Plan,
    inputs: &BTreeMap<InputId, InputValue>,
    count_inputs: bool,
) -> u64 {
    let (start, at) = actual_live(engine, g, plan, inputs, count_inputs);
    at.into_iter().fold(start, u64::max)
}

/// 描いた値で数えた、描き始めと各ノードを描いている間（そのノードの出力を含む）に生きている値のバイト数（共有した実体は
/// 1 度だけ）。値は、すべてのノードを出力にした文書を描いて得る（値は出力の数に依らない）。落とす時点は `plan`（元の文書の
/// 計画）の最後の使い手の表。入力は `count_inputs` のときだけ数える（数えないときも、入力と共有する実体は入力の分として引く）。
fn actual_live(
    engine: &Engine,
    g: &Graph,
    plan: &Plan,
    inputs: &BTreeMap<InputId, InputValue>,
    count_inputs: bool,
) -> (u64, Vec<u64>) {
    let template = g.outputs[0].clone();
    let mut all = g.clone();
    all.outputs = g
        .nodes
        .iter()
        .enumerate()
        .map(|(k, n)| OutputSpec {
            id: OutputId(k as u16),
            from: PortRef::Node { id: n.id },
            quantize: None,
            ..template.clone()
        })
        .collect();
    let all_plan = engine.compile(&all, &shapes_of(inputs)).unwrap();
    let rendered = engine.render(&all_plan, inputs, &NoMonitor).unwrap();
    let values: BTreeMap<NodeId, Value> = g
        .nodes
        .iter()
        .zip(rendered.outputs)
        .map(|(n, o)| (n.id, o.value))
        .collect();
    let input_values: Vec<Value> = inputs.values().map(|v| v.value.clone()).collect();
    let base = if count_inputs {
        0
    } else {
        unique_bytes(&input_values)
    };
    let mut live: BTreeMap<NodeId, Value> = BTreeMap::new();
    let start = unique_bytes(&input_values) - base;
    let mut at = Vec::with_capacity(g.nodes.len());
    for (i, n) in g.nodes.iter().enumerate() {
        live.insert(n.id, values[&n.id].clone());
        at.push(unique_bytes(input_values.iter().chain(live.values())) - base);
        for id in plan.released_after(i) {
            live.remove(id);
        }
    }
    (start, at)
}

/// 各ノードの見積もりが、そのノードを描いている間に生きている値以上。
fn assert_nodes_cover(est: &MemoryEstimate, actual: &[u64], what: &str) {
    assert_eq!(est.nodes.len(), actual.len(), "{what}");
    for (n, a) in est.nodes.iter().zip(actual) {
        assert!(
            n.at >= *a,
            "{what}: ノード {} の見積もり {} < 実際 {a}",
            n.id.0,
            n.at
        );
    }
}

/// 推論で上限と照らす Indexed8 への作業領域の式（ops/image/convert.rs の `infer_work`。段 4 から凍結）。
fn convert_infer_formula(input: &Shape) -> u64 {
    let (frame_px, frames) = match input {
        Shape::Image { w, h, .. } => (w.max() * h.max(), 1),
        Shape::Frames { count, w, h, .. } => (w.max() * h.max(), count.max()),
        Shape::Audio { .. } => (0, 0),
    };
    frame_px * 8 + (frame_px * frames).min(1 << 32) * 32
}

/// 31（NFR-17）：登録されたすべての操作の例で、見積もりの描く間のピークが、描いて得た値で数えた生きている値（入力 ＋ 出力。
/// 共有は 1 度だけ）以上。作業領域は操作の申告（持ち上げたノードはフレームの形で、並列の数を掛けたもの）と申告した解析の
/// 結果。Indexed8 への convert の作業領域は推論の式以上。入力を呼び手が持つ（Held）ときは入力を数えない。
#[test]
fn estimate_bounds_every_example() {
    let engine = engine();
    let (mut checked, mut lifted, mut indexed, mut with_work, mut with_needs) = (0, 0, 0, 0, 0);
    for op in engine.registry().all() {
        for ex in &op.examples().unwrap() {
            let what = format!("{} v{} {}", op.id(), op.version(), ex.name);
            let input = ex.inputs[0].generate().unwrap();
            let in_shape = input.shape();
            let inputs = inputs_of(input, source_of(&ex.inputs).unwrap());
            let g = single_node_graph(op.id(), op.version(), ex.params.clone()).unwrap();
            let id = g.nodes[0].id;
            let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
            let out_shape = plan.node_shape(id).unwrap();
            let (start, live) = actual_live(&engine, &g, &plan, &inputs, true);
            let actual = live.iter().copied().fold(start, u64::max);
            let params = op.parse(&ex.params).unwrap();
            let is_lifted =
                op.signature().lift == Lift::PerFrame && in_shape.kind() == ValueKind::Frames;
            let (ins, out) = if is_lifted {
                lifted += 1;
                (
                    vec![in_shape.frame_image().unwrap()],
                    out_shape.frame_image().unwrap(),
                )
            } else {
                (vec![in_shape], out_shape)
            };
            let declared = op.work_bytes(params.as_ref(), &ins, &out).unwrap();
            let analyses: u64 = plan
                .node_needs(id)
                .unwrap()
                .iter()
                .map(|n| {
                    engine
                        .analyses()
                        .get(n.id, n.v)
                        .unwrap()
                        .estimate(&n.params, &in_shape)
                        .unwrap()
                        .bytes
                })
                .sum();
            if analyses > 0 {
                with_needs += 1;
            }
            for tasks in [1, 3, 16] {
                let est = engine
                    .estimate(&plan, &EstimateSpec::new(tasks).digests(true), None)
                    .unwrap();
                assert!(
                    est.render >= actual,
                    "{what}（{tasks} スレッド）: 見積もり {} < 実際 {actual}",
                    est.render
                );
                assert_nodes_cover(&est, &live, &format!("{what}（{tasks} スレッド）"));
                assert_eq!(est.peak, est.render, "{what}: 読み書きしない");
                assert_eq!((est.decode, est.encode), (0, 0), "{what}");
                let n = &est.nodes[0];
                assert_eq!(n.id, id);
                assert!(!n.hit);
                assert_eq!(n.value, out_shape.value_bytes(), "{what}");
                assert_eq!(n.work, declared.total(tasks) + analyses, "{what}");
                assert_eq!(
                    est.retained,
                    in_shape.value_bytes() + out_shape.value_bytes(),
                    "{what}"
                );
            }
            if declared.total(1) > 0 {
                with_work += 1;
            }
            if op.id() == "image.convert" && ex.params["format"] == "indexed8" {
                indexed += 1;
                let est = engine.estimate(&plan, &EstimateSpec::new(1), None).unwrap();
                assert!(
                    est.nodes[0].work >= convert_infer_formula(&in_shape),
                    "{what}: 作業領域 {} < 推論の式 {}",
                    est.nodes[0].work,
                    convert_infer_formula(&in_shape)
                );
            }
            // 呼び手が持つ入力は数えない（入力と共有する実体も入力の分）
            let held = engine
                .estimate(
                    &plan,
                    &EstimateSpec::new(1)
                        .digests(true)
                        .all_inputs(&plan, Source::Held),
                    None,
                )
                .unwrap();
            assert_eq!(held.retained, out_shape.value_bytes(), "{what}");
            let (_, live_held) = actual_live(&engine, &g, &plan, &inputs, false);
            assert_nodes_cover(&held, &live_held, &format!("{what}（入力を除く）"));
            let actual_held = actual_peak(&engine, &g, &plan, &inputs, false);
            assert!(
                held.render >= actual_held,
                "{what}: 見積もり {} < 実際 {actual_held}（入力を除く）",
                held.render
            );
            checked += 1;
        }
    }
    // 空回りしていない（例が 80 を超える。持ち上げ・作業領域・申告した解析・Indexed8 への convert を含む）
    assert!(checked > 80, "{checked}");
    assert!(lifted > 0 && with_work > 0 && with_needs > 0 && indexed > 0);
}

/// 31：鎖・枝分かれ・外したノード・使われないノード・持ち上げ・量子化した出力でも、見積もりは生きている値以上。外した
/// ノードは入力 0 と同じ実体として数え直さない。
#[test]
fn estimate_bounds_chains() {
    let engine = engine();
    let flip = |axis: &str| json!({ "axis": axis });
    let rotate = json!({ "turn": "cw" });
    let resize = |px: u32, filter: &str| json!({ "width": { "px": px }, "height": "auto", "filter": filter });
    let indexed =
        json!({ "format": "indexed8", "colors": 16, "dither": "none", "matte": [255, 255, 255] });
    let photo = InputGen::Noise {
        w: 300,
        h: 200,
        fmt: PixelFormat::Rgba8,
        seed: 3,
    };
    let frames = InputGen::NoiseFrames {
        w: 64,
        h: 48,
        fmt: PixelFormat::Rgba8,
        count: 4,
        seed: 5,
    };
    let audio = InputGen::Sine {
        rate: 8_000,
        channels: 2,
        frames: 8_000,
        freq: 440,
        loop_range: None,
    };
    let q16 = Quantize {
        v: QUANTIZE_VERSION,
        bits: 16,
        dither: Dither::Tpdf,
        seed: 7,
    };
    let cases: Vec<(&str, InputGen, Graph)> = vec![
        (
            "鎖（外したノードと枝分かれの出力つき）",
            photo.clone(),
            graph(
                vec![
                    node(1, "image.flip", flip("horizontal"), INPUT, true),
                    node(2, "image.resize", resize(150, "lanczos3"), at(1), true),
                    node(3, "image.rotate90", rotate.clone(), at(2), false),
                    node(4, "image.convert", indexed.clone(), at(3), true),
                ],
                vec![(0, at(4), None), (1, at(1), None)],
            ),
        ),
        (
            "枝分かれと使われないノード",
            photo.clone(),
            graph(
                vec![
                    node(1, "image.flip", flip("horizontal"), INPUT, true),
                    node(2, "image.flip", flip("vertical"), INPUT, true),
                    node(3, "image.rotate90", rotate.clone(), at(2), true),
                    node(4, "image.flip", flip("vertical"), at(1), true),
                ],
                vec![(0, at(1), None), (1, at(3), None)],
            ),
        ),
        (
            "入力を直接外したノードと、入力をそのまま出す出力",
            photo.clone(),
            graph(
                vec![
                    node(1, "image.flip", flip("horizontal"), INPUT, false),
                    node(2, "image.rotate90", rotate.clone(), at(1), true),
                ],
                vec![(0, at(2), None), (1, INPUT, None)],
            ),
        ),
        (
            "フレームの列（持ち上げ）",
            frames,
            graph(
                vec![
                    node(1, "image.resize", resize(40, "bilinear"), INPUT, true),
                    node(2, "image.flip", flip("vertical"), at(1), true),
                    node(3, "image.convert", indexed, at(2), true),
                ],
                vec![(0, at(3), None)],
            ),
        ),
        (
            "音声と量子化した出力",
            audio,
            graph(
                vec![
                    node(1, "audio.gain", json!({ "db": -3.0 }), INPUT, true),
                    node(
                        2,
                        "audio.normalize",
                        json!({ "target": -16.0, "peakLimit": null }),
                        at(1),
                        true,
                    ),
                ],
                vec![(0, at(2), Some(q16)), (1, at(1), None)],
            ),
        ),
    ];
    for (what, input, g) in cases {
        let inputs = inputs_of(input.generate().unwrap(), Digest([1; 32]));
        let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
        for (count_inputs, src) in [(true, Source::Memory), (false, Source::Held)] {
            let spec = EstimateSpec::new(3).digests(true).all_inputs(&plan, src);
            let est = engine.estimate(&plan, &spec, None).unwrap();
            let (_, live) = actual_live(&engine, &g, &plan, &inputs, count_inputs);
            assert_nodes_cover(&est, &live, &format!("{what}（{src:?}）"));
            let actual = actual_peak(&engine, &g, &plan, &inputs, count_inputs);
            assert!(
                est.render >= actual,
                "{what}（{src:?}）: 見積もり {} < 実際 {actual}",
                est.render
            );
            // 外したノードは新しい値を数えない
            for n in &est.nodes {
                let enabled = g.nodes.iter().find(|x| x.id == n.id).unwrap().enabled;
                if !enabled {
                    assert_eq!((n.value, n.work), (0, 0), "{what}");
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoParams {}

impl OpParams for NoParams {
    fn schema() -> ParamSchema {
        ParamSchema { params: Vec::new() }
    }
}

/// `test.frame_work`：フレームに持ち上げる操作。値はそのまま返し、作業領域に入力と出力の形の値の大きさを申告する
/// （1 つだけ持つ分 ＝ 入力、仕事ごとの分 ＝ 出力）。持ち上げたノードの申告がフレームの形で求められるかの確かめ。
struct FrameWork;

impl Op for FrameWork {
    const ID: &'static str = "test.frame_work";
    const VERSION: u16 = 1;
    type Params = NoParams;
    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image],
            }],
            lift: Lift::PerFrame,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }
    fn infer(_: &NoParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: 1,
        })
    }
    fn apply(_: &NoParams, inputs: &[&Value], _: &mut ApplyCx<'_>) -> Result<Applied, EngineError> {
        Ok(Applied::value(inputs[0].clone()))
    }
    fn work_bytes(_: &NoParams, inputs: &[Shape], out: &Shape) -> Work {
        Work {
            shared: inputs[0].value_bytes(),
            per_task: out.value_bytes(),
        }
    }
    fn examples() -> Vec<Example<NoParams>> {
        Vec::new()
    }
}

/// 31：持ち上げたノードの作業領域は、フレームの画像の形（入力も出力も 1 フレーム）で求めて、並列の数を掛ける（フレームは
/// 順に当てるので 1 フレーム分）。持ち上げないノード（画像 1 枚）は、そのままの形。出力は、入力とタイルを共有していても
/// 新しい値として数える（多めに見積もる側）。
#[test]
fn lifted_nodes_declare_work_per_frame() {
    let mut r = Registry::empty();
    r.add_current::<FrameWork>().unwrap();
    let engine = Engine::with_registry(r, Limits::default());
    let g = single_node_graph("test.frame_work", 1, json!({})).unwrap();
    let frame = 16 * 8 * 4;
    for (what, input, count) in [
        (
            "フレームの列",
            InputGen::NoiseFrames {
                w: 16,
                h: 8,
                fmt: PixelFormat::Rgba8,
                count: 4,
                seed: 1,
            },
            4,
        ),
        (
            "画像",
            InputGen::Noise {
                w: 16,
                h: 8,
                fmt: PixelFormat::Rgba8,
                seed: 1,
            },
            1,
        ),
    ] {
        let inputs = inputs_of(input.generate().unwrap(), Digest::default());
        let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
        let est = engine.estimate(&plan, &EstimateSpec::new(3), None).unwrap();
        let n = &est.nodes[0];
        assert_eq!(n.work, frame + 3 * frame, "{what}");
        assert_eq!(n.value, count * frame, "{what}: 出力はフレームの列の全体");
        assert_eq!(est.render, 2 * count * frame + 4 * frame, "{what}");
        assert!(est.render >= actual_peak(&engine, &g, &plan, &inputs, true));
    }
}

/// 外したノード・入力をそのまま出す出力は、元の実体が生きている間は数え直さず、元を落としたあとも同じ実体として生かす。
#[test]
fn disabled_nodes_share_the_input() {
    let engine = engine();
    let inputs = inputs_of(mib_image(1), Digest::default());
    let flip = json!({ "axis": "horizontal" });
    // 1 → 2（外す）→ 3：1 は 2 のあとで落ちるが、2 が同じ実体を持つので 3 を描く間も生きている
    let g = graph(
        vec![
            node(1, "image.flip", flip.clone(), INPUT, true),
            node(2, "image.flip", flip.clone(), at(1), false),
            node(3, "image.flip", flip, at(2), true),
        ],
        vec![(0, at(3), None)],
    );
    let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
    let est = engine
        .estimate(
            &plan,
            &EstimateSpec::new(1).all_inputs(&plan, Source::Held),
            None,
        )
        .unwrap();
    let at_nodes: Vec<u64> = est.nodes.iter().map(|n| n.at).collect();
    assert_eq!(at_nodes, vec![MIB, MIB, 2 * MIB]);
    assert_eq!((est.render, est.retained), (2 * MIB, MIB));
    assert_eq!(est.render, actual_peak(&engine, &g, &plan, &inputs, false));
}

/// 読む・書くの作業領域：読む段は「読み終えた入力 ＋ 読んでいる値 ＋ max(デコーダの作業領域, ダイジェストの帯)」、書く段は
/// 「入力 ＋ 出力 ＋ 書いたバイト列（値の 1 倍）＋ エンコーダの作業領域」。形式で選ぶ口と、決まったコーデックで読み書きする口は同じ。
#[test]
fn reading_and_writing_are_counted() {
    let engine = engine();
    let codecs = CodecSet::pinned();
    let shape = Shape::Image {
        w: Dim::Exact(640),
        h: Dim::Exact(480),
        fmt: PixelFormat::Rgb8,
    };
    let g = single_node_graph("image.rotate90", 1, json!({ "turn": "cw" })).unwrap();
    let plan = engine
        .compile(&g, &BTreeMap::from([(InputId(0), shape)]))
        .unwrap();
    let out = plan.node_shape(g.nodes[0].id).unwrap();
    let v = shape.value_bytes();
    let opts = json!({ "compression": "fast" });
    let spec = EstimateSpec::new(2)
        .digests(true)
        .input(
            InputId(0),
            Source::Decode {
                codecs: &codecs,
                format: "jpeg",
            },
        )
        .output(
            OutputId(0),
            Sink::Encode {
                codecs: &codecs,
                format: "png",
                opts: &opts,
            },
        );
    let est = engine.estimate(&plan, &spec, None).unwrap();
    let decode_work = codecs.decode_work_bytes("jpeg", &shape);
    assert!(decode_work > v, "JPEG の読みは値の 1 倍より多い");
    assert_eq!(est.decode, v + decode_work.max(v.min(4 * MIB)));
    let encode_work = codecs.encode_work_bytes("png", &out, &opts);
    assert!(encode_work > 0);
    assert_eq!(est.retained, v + out.value_bytes());
    assert_eq!(est.encode, est.retained + out.value_bytes() + encode_work);
    // 読む段の作業領域は描く・書く間も、描く段で返した分は書く間も残るとみなす（持ち越し）
    assert_eq!(est.carried_decode, decode_work.max(v.min(4 * MIB)));
    assert_eq!(est.carried_render, est.render - est.retained);
    assert!(est.carried_render > 0, "ダイジェストの帯");
    assert_eq!(
        est.peak,
        est.decode
            .max(est.render + est.carried_decode)
            .max(est.encode + est.carried_decode + est.carried_render)
    );
    assert_eq!(
        est.peak,
        est.encode + est.carried_decode + est.carried_render,
        "書く段がいちばん重い"
    );
    // 決まったデコーダ・エンコーダで読み書きしても同じ（ここではその形式の唯一のもの）
    let decoder = codecs.decoders_for("jpeg")[0].clone();
    let encoder = codecs.encoder("png", &out).unwrap().clone();
    let fixed = EstimateSpec::new(2)
        .digests(true)
        .input(
            InputId(0),
            Source::Decoder {
                decoder: decoder.as_ref(),
                format: "jpeg",
            },
        )
        .output(
            OutputId(0),
            Sink::Encoder {
                encoder: encoder.as_ref(),
                format: "png",
                opts: &opts,
            },
        );
    let by_codec = engine.estimate(&plan, &fixed, None).unwrap();
    assert_eq!(
        by_codec.decode,
        v + decoder.work_bytes("jpeg", &shape).max(v.min(4 * MIB))
    );
    assert_eq!(by_codec.encode, est.encode);
    // 書かない出力・読まない入力は数えない（持ち越しもない）
    let plain = engine.estimate(&plan, &EstimateSpec::new(2), None).unwrap();
    assert_eq!(
        (
            plain.decode,
            plain.encode,
            plain.carried_decode,
            plain.carried_render
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(plain.peak, plain.render);
    // 2 つ目の入力を読む間は、1 つ目の値と、1 つ目の読みの持ち越しも数える
    let mut two = Graph::new_single();
    let mut second = two.inputs[0].clone();
    second.id = InputId(1);
    second.role = kura_engine::doc::InputRole::Fixed {
        hash: Digest([9; 32]),
    };
    two.inputs.push(second);
    let two = graph_with_inputs(
        two,
        vec![node(
            1,
            "image.rotate90",
            json!({ "turn": "cw" }),
            INPUT,
            true,
        )],
        vec![
            (0, at(1), None),
            (1, PortRef::Input { id: InputId(1) }, None),
        ],
    );
    let small = Shape::Image {
        w: Dim::Exact(64),
        h: Dim::Exact(32),
        fmt: PixelFormat::Rgba8,
    };
    let plan = engine
        .compile(
            &two,
            &BTreeMap::from([(InputId(0), shape), (InputId(1), small)]),
        )
        .unwrap();
    let both = EstimateSpec::new(1).all_inputs(
        &plan,
        Source::Decode {
            codecs: &codecs,
            format: "jpeg",
        },
    );
    let est = engine.estimate(&plan, &both, None).unwrap();
    let small_work = codecs
        .decode_work_bytes("jpeg", &small)
        .max(small.value_bytes());
    let first = decode_work.max(v.min(4 * MIB));
    assert_eq!(est.carried_decode, first + small_work);
    assert_eq!(est.carried_render, 0, "書かない");
    assert_eq!(est.peak, est.decode.max(est.render + est.carried_decode));
    assert_eq!(
        est.decode,
        (v + first).max(v + first + small.value_bytes() + small_work)
    );
}

/// 読む入力のファイルの長さを渡すと、読む段の作業領域はデコーダの `file_work_bytes`（ファイル全体を読んで持つ aseprite は
/// 長さを足す）。渡さなければ `work_bytes`（aseprite はファイルを値の大きさとみなす）。長さを見ないデコーダ（JPEG）は同じ。
#[test]
fn file_lengths_reach_the_decoder_estimate() {
    let engine = engine();
    let codecs = CodecSet::pinned();
    let shape = Shape::Frames {
        count: Dim::Exact(4),
        w: Dim::Exact(256),
        h: Dim::Exact(128),
        fmt: PixelFormat::Rgba8,
        full: kura_engine::shape::Fullness::Full,
    };
    let g = single_node_graph("image.flip", 1, json!({ "axis": "horizontal" })).unwrap();
    let plan = engine
        .compile(&g, &BTreeMap::from([(InputId(0), shape)]))
        .unwrap();
    let v = shape.value_bytes();
    let canvas = 256 * 128 * 4;
    let decode = |format, len: Option<u64>| {
        let mut spec = EstimateSpec::new(1).input(
            InputId(0),
            Source::Decode {
                codecs: &codecs,
                format,
            },
        );
        if let Some(len) = len {
            spec = spec.input_len(InputId(0), len);
        }
        engine.estimate(&plan, &spec, None).unwrap().decode
    };
    let len = 50 * MIB;
    // 値 ＋ ファイル ＋ 持っておくセル（値まで）＋ キャンバス 5 枚
    assert_eq!(
        codecs.decode_file_work_bytes("aseprite", &shape, len),
        len + v + 5 * canvas
    );
    assert_eq!(decode("aseprite", Some(len)), v + len + v + 5 * canvas);
    assert_eq!(decode("aseprite", None), v + v + v + 5 * canvas);
    // 決まったデコーダで読むときも長さを使う
    let decoder = codecs.decoders_for("aseprite")[0].clone();
    let fixed = EstimateSpec::new(1)
        .input(
            InputId(0),
            Source::Decoder {
                decoder: decoder.as_ref(),
                format: "aseprite",
            },
        )
        .input_len(InputId(0), len);
    assert_eq!(
        engine.estimate(&plan, &fixed, None).unwrap().decode,
        v + len + v + 5 * canvas
    );
    // 長さを見ないデコーダは変わらない
    let image = Shape::Image {
        w: Dim::Exact(640),
        h: Dim::Exact(480),
        fmt: PixelFormat::Rgb8,
    };
    assert_eq!(
        codecs.decode_file_work_bytes("jpeg", &image, len),
        codecs.decode_work_bytes("jpeg", &image)
    );
    // 読まない入力（メモリにある値・呼び手が持つ値）には長さは効かない
    let held = EstimateSpec::new(1)
        .input(InputId(0), Source::Held)
        .input_len(InputId(0), len);
    assert_eq!(engine.estimate(&plan, &held, None).unwrap().decode, 0);
}

/// キャッシュに当たるノード（`render_cached` が描かないもの）は、値も作業領域も数えない。外れたノードは数える。量子化した
/// 出力も、当たれば数えない。
#[test]
fn cache_hits_are_not_counted() {
    let engine = engine();
    let inputs = inputs_of(mib_image(2), Digest([2; 32]));
    let fins = fingerprinted(&inputs);
    let chain = |last: u32| {
        graph(
            vec![
                node(
                    1,
                    "image.flip",
                    json!({ "axis": "horizontal" }),
                    INPUT,
                    true,
                ),
                node(2, "image.rotate90", json!({ "turn": "cw" }), at(1), true),
                node(
                    3,
                    "image.resize",
                    json!({ "width": { "px": last }, "height": "auto", "filter": "lanczos3" }),
                    at(2),
                    true,
                ),
            ],
            vec![(0, at(3), None)],
        )
    };
    let plan = engine.compile(&chain(256), &shapes_of(&inputs)).unwrap();
    let spec = EstimateSpec::new(3).all_inputs(&plan, Source::Held);
    let mut cache = NodeCache::new(1 << 30);
    let cold = engine
        .estimate(
            &plan,
            &spec,
            Some(CacheProbe {
                cache: &cache,
                inputs: &fins,
            }),
        )
        .unwrap();
    assert_eq!(cold, engine.estimate(&plan, &spec, None).unwrap());
    assert!(cold.nodes.iter().all(|n| !n.hit));
    engine
        .render_cached(
            &plan,
            &fins,
            &mut cache,
            &RenderOptions::default(),
            &NoMonitor,
        )
        .unwrap();
    let warm = engine
        .estimate(
            &plan,
            &spec,
            Some(CacheProbe {
                cache: &cache,
                inputs: &fins,
            }),
        )
        .unwrap();
    assert!(
        warm.nodes
            .iter()
            .all(|n| n.hit && n.value == 0 && n.work == 0)
    );
    assert_eq!((warm.render, warm.retained), (0, 0));
    // 最後のノードだけを変える：前の 2 つは当たり、最後は外れ
    let changed = engine.compile(&chain(200), &shapes_of(&inputs)).unwrap();
    let est = engine
        .estimate(
            &changed,
            &spec,
            Some(CacheProbe {
                cache: &cache,
                inputs: &fins,
            }),
        )
        .unwrap();
    let hits: Vec<bool> = est.nodes.iter().map(|n| n.hit).collect();
    assert_eq!(hits, vec![true, true, false]);
    let last = &est.nodes[2];
    assert_eq!(
        last.value,
        changed.node_shape(NodeId(3)).unwrap().value_bytes()
    );
    assert!(last.work > 0, "平滑なリサイズは作業領域を申告する");
    assert_eq!(est.render, last.value + last.work);
    // 量子化した出力：描いたあとは当たる
    let audio = inputs_of(
        InputGen::Sine {
            rate: 8_000,
            channels: 1,
            frames: 4_000,
            freq: 300,
            loop_range: None,
        }
        .generate()
        .unwrap(),
        Digest([3; 32]),
    );
    let afins = fingerprinted(&audio);
    let q = Quantize {
        v: QUANTIZE_VERSION,
        bits: 8,
        dither: Dither::Tpdf,
        seed: 1,
    };
    let g = graph(
        vec![node(1, "audio.gain", json!({ "db": -2.0 }), INPUT, true)],
        vec![(0, at(1), Some(q))],
    );
    let plan = engine.compile(&g, &shapes_of(&audio)).unwrap();
    let spec = EstimateSpec::new(1).all_inputs(&plan, Source::Held);
    let bytes = plan.output_shapes()[0].1.value_bytes();
    let cold = engine
        .estimate(
            &plan,
            &spec,
            Some(CacheProbe {
                cache: &cache,
                inputs: &afins,
            }),
        )
        .unwrap();
    assert_eq!(cold.retained, 2 * bytes, "ノードの値と量子化した値");
    engine
        .render_cached(
            &plan,
            &afins,
            &mut cache,
            &RenderOptions::default(),
            &NoMonitor,
        )
        .unwrap();
    let warm = engine
        .estimate(
            &plan,
            &spec,
            Some(CacheProbe {
                cache: &cache,
                inputs: &afins,
            }),
        )
        .unwrap();
    assert_eq!((warm.render, warm.retained), (0, 0));
    // 別のエンジンの計画でキャッシュを引くのは内部の誤り
    let other = Engine::new().unwrap();
    assert!(matches!(
        other.estimate(
            &plan,
            &spec,
            Some(CacheProbe {
                cache: &cache,
                inputs: &afins
            })
        ),
        Err(EngineError::Internal(_))
    ));
}

/// 予算を確保してから描く（一括の流れ）。確保できなければ描かない。
fn admit_and_render(
    engine: &Engine,
    plan: &Plan,
    spec: &EstimateSpec<'_>,
    inputs: &BTreeMap<InputId, InputValue>,
    budget: &Arc<MemoryBudget>,
    class: Class,
    mon: &dyn kura_engine::Monitor,
) -> Result<Rendered, EngineError> {
    let admission = engine.admit(plan, spec, budget, class, mon)?;
    assert_eq!(admission.lease.bytes(), admission.estimate.peak);
    engine.render(plan, inputs, mon)
}

fn resize_plan(engine: &Engine, inputs: &BTreeMap<InputId, InputValue>) -> Plan {
    let g = single_node_graph(
        "image.resize",
        1,
        json!({ "width": { "px": 300 }, "height": "auto", "filter": "lanczos3" }),
    )
    .unwrap();
    engine.compile(&g, &shapes_of(inputs)).unwrap()
}

/// 32（E25）：見積もりが予算の全体を超える計画は、どちらのクラスでも見積もりの段階で Limit になり、何も確保せず、操作は
/// 1 つも動かない（中止の確かめの数え上げが 0 回）。足りる予算なら描けて、数え上げも働く。エディタの確保でも、全体を超える
/// 計画はキャッシュも確保も変えずに Limit。
#[test]
fn over_budget_plans_fail_before_rendering() {
    let engine = engine();
    let inputs = inputs_of(mib_image(4), Digest([4; 32]));
    let plan = resize_plan(&engine, &inputs);
    let spec = EstimateSpec::new(3).digests(true);
    let est = engine.estimate(&plan, &spec, None).unwrap();
    let small = MemoryBudget::new(est.peak - 1);
    for class in [Class::Interactive, Class::Bulk] {
        let mon = CancelAfter::new(0);
        let r = admit_and_render(&engine, &plan, &spec, &inputs, &small, class, &mon);
        assert!(matches!(r, Err(EngineError::Limit(_))), "{class:?}: {r:?}");
        assert_eq!(mon.calls(), 0, "{class:?}: 操作が動いた");
        assert_eq!(small.used().unwrap(), 0);
    }
    // ちょうど足りる予算なら描ける（中止の確かめが呼ばれる）。描き終えて落とすと返る
    let enough = MemoryBudget::new(est.peak);
    let mon = CancelAfter::new(0);
    let r = admit_and_render(&engine, &plan, &spec, &inputs, &enough, Class::Bulk, &mon).unwrap();
    assert!(mon.calls() > 0);
    let want = engine.render(&plan, &inputs, &NoMonitor).unwrap();
    assert_eq!(r.outputs[0].digest, want.outputs[0].digest);
    assert_eq!(enough.used().unwrap(), 0);
    // エディタ：全体を超える計画は、キャッシュの中身・上限と確保を変えずに Limit
    let fins = fingerprinted(&inputs);
    let held_spec = EstimateSpec::new(3).all_inputs(&plan, Source::Held);
    let held_est = engine.estimate(&plan, &held_spec, None).unwrap();
    let budget = MemoryBudget::new(held_est.peak - 1);
    let mut cache = NodeCache::new(0);
    let mut held = CacheLease::open(&budget, &mut cache, held_est.peak / 2).unwrap();
    let before = (cache.len(), cache.capacity(), cache.bytes(), held.bytes());
    let r = engine.admit_cached(&plan, &held_spec, &fins, &mut cache, &mut held);
    assert!(matches!(r, Err(EngineError::Limit(_))), "{r:?}");
    assert_eq!(
        (cache.len(), cache.capacity(), cache.bytes(), held.bytes()),
        before
    );
}

/// 編集の流れの部品：入力 1 MiB をキャッシュの外の値として数え、上限 4 MiB のキャッシュを開く（予算 10 MiB）。
struct Editor {
    engine: Engine,
    budget: Arc<MemoryBudget>,
    inputs: BTreeMap<InputId, InputValue>,
    fins: BTreeMap<InputId, FingerprintedInput>,
    cache: NodeCache,
    held: CacheLease,
}

impl Editor {
    fn open() -> Self {
        let engine = engine();
        let budget = MemoryBudget::new(10 * MIB);
        let inputs = inputs_of(mib_image(5), Digest([5; 32]));
        let fins = fingerprinted(&inputs);
        let mut cache = NodeCache::new(0);
        assert_eq!(cache.register_external(&inputs[&InputId(0)].value), MIB);
        let held = CacheLease::open(&budget, &mut cache, 4 * MIB).unwrap();
        assert_eq!((held.bytes(), cache.capacity()), (4 * MIB, 4 * MIB));
        Self {
            engine,
            budget,
            inputs,
            fins,
            cache,
            held,
        }
    }

    /// 1 MiB の値を 2 つ作る 2 段の鎖（`op` は image.flip か image.rotate90）。
    fn plan(&self, op: &str, params: [serde_json::Value; 2]) -> Plan {
        let [a, b] = params;
        let g = graph(
            vec![node(1, op, a, INPUT, true), node(2, op, b, at(1), true)],
            vec![(0, at(2), None)],
        );
        self.engine.compile(&g, &shapes_of(&self.inputs)).unwrap()
    }

    fn spec(&self, plan: &Plan) -> EstimateSpec<'static> {
        EstimateSpec::new(3).all_inputs(plan, Source::Held)
    }

    /// キャッシュの項目の数・上限・大きさと、キャッシュの分の確保。
    fn state(&self) -> (usize, u64, u64, u64) {
        (
            self.cache.len(),
            self.cache.capacity(),
            self.cache.bytes(),
            self.held.bytes(),
        )
    }

    fn admit(&mut self, plan: &Plan) -> Result<MemoryEstimate, EngineError> {
        let spec = self.spec(plan);
        self.engine
            .admit_cached(plan, &spec, &self.fins, &mut self.cache, &mut self.held)
    }

    /// 確保してから描き、キャッシュなしの正と同じことを確かめて、確保を戻す。
    fn render(&mut self, plan: &Plan, focus: Option<NodeId>) -> MemoryEstimate {
        let standing = self.budget.usage().unwrap().standing;
        let est = self.admit(plan).unwrap();
        // 描く間に伸ばした分は、持ち続ける対話（一括の駐車の条件）に入れない
        assert!(self.budget.usage().unwrap().standing <= standing);
        let r = self
            .engine
            .render_cached(
                plan,
                &self.fins,
                &mut self.cache,
                &RenderOptions {
                    digests: true,
                    focus,
                    verify_hits: false,
                },
                &NoMonitor,
            )
            .unwrap();
        let want = self.engine.render(plan, &self.inputs, &NoMonitor).unwrap();
        assert_eq!(r.outputs[0].digest, Some(want.outputs[0].digest));
        // 描いている間も、キャッシュが持つ量は確保の中
        assert!(self.cache.bytes() <= self.held.bytes());
        self.held.settle(&mut self.cache).unwrap();
        assert_eq!(
            self.held.bytes(),
            self.cache.capacity().max(self.cache.bytes())
        );
        // 戻した確保は、手の合間も持ち続ける対話
        assert_eq!(self.budget.usage().unwrap().standing, self.held.bytes());
        est
    }
}

fn flips() -> [serde_json::Value; 2] {
    [
        json!({ "axis": "horizontal" }),
        json!({ "axis": "vertical" }),
    ]
}

fn rotations() -> [serde_json::Value; 2] {
    [json!({ "turn": "cw" }), json!({ "turn": "cw" })]
}

/// E25：エディタは待たない。一括が予算の残りを使っていて足りなければ、キャッシュの固定していないものを古い順に追い出して
/// （キャッシュの分の確保も縮めて）もう一度確かめ、通れば描く。値はキャッシュなしと同じ。描いたあとは確保を戻し、一括が
/// 返せば上限も望む量に戻る。
#[test]
fn interactive_evicts_the_cache_and_passes() {
    let mut ed = Editor::open();
    let a = ed.plan("image.flip", flips());
    let est = ed.render(&a, None);
    assert_eq!(est.peak, 2 * MIB);
    assert_eq!(ed.cache.bytes(), 3 * MIB, "入力 ＋ 2 つの値");
    let a_fps = ed.engine.fingerprints(&a, &ed.fins).unwrap();
    // 一括が予算の残り（10 − 4）をすべて使う
    let bulk = ed
        .budget
        .try_acquire(6 * MIB, Class::Bulk)
        .unwrap()
        .unwrap();
    let evicted = ed.cache.stats().evicted;
    let b = ed.plan("image.rotate90", rotations());
    let est = ed.render(&b, None);
    assert_eq!(est.peak, 2 * MIB);
    assert!(ed.cache.stats().evicted > evicted, "追い出した");
    assert!(!ed.cache.contains(&a_fps.nodes[&NodeId(1)]), "古いものから");
    assert_eq!(ed.budget.used().unwrap(), 10 * MIB);
    // 一括が返したら、上限は望む量に戻る
    drop(bulk);
    ed.held.settle(&mut ed.cache).unwrap();
    assert_eq!(ed.cache.capacity(), 4 * MIB);
    assert_eq!(ed.held.bytes(), 4 * MIB);
}

/// E25：固定した値（編集中のノードの入力・出力）と入力でキャッシュが埋まっていて、一括が残りを使っていれば、追い出しても
/// 足りないので Limit（待たない。描かない）。見積もりそのものは予算の全体に収まる。一括が返せば通る。
#[test]
fn interactive_fails_when_pinned_values_fill_the_budget() {
    let mut ed = Editor::open();
    let a = ed.plan("image.flip", flips());
    ed.render(&a, Some(NodeId(2)));
    assert_eq!(ed.cache.pins().len(), 2, "編集中のノードの入力と出力");
    assert_eq!(ed.cache.bytes(), 3 * MIB);
    let bulk = ed
        .budget
        .try_acquire(6 * MIB, Class::Bulk)
        .unwrap()
        .unwrap();
    let b = ed.plan("image.rotate90", rotations());
    let before = ed.state();
    let r = ed.admit(&b);
    assert!(matches!(r, Err(EngineError::Limit(_))), "{r:?}");
    // 追い出せるものがないので、キャッシュ（項目・上限・大きさ）も確保も変えない（§12.10）
    assert_eq!(
        ed.state(),
        before,
        "固定したものは追い出さず、上限と確保も戻す"
    );
    assert!(ed.engine.estimate(&b, &ed.spec(&b), None).unwrap().peak <= ed.budget.total());
    assert_eq!(ed.budget.used().unwrap(), 10 * MIB);
    drop(bulk);
    ed.render(&b, None);
}

/// E25（§12.10）：固定していないものを追い出しても足りずに Limit になるとき、追い出した値は戻らないが、キャッシュの上限と
/// 確保は元に戻す（Limit のあとに settle を呼ばなくても、次の描画が下がった上限で値を覚えられなくならない）。予算の使用量も
/// 変えない。一括が返せば、同じ計画が通る。
#[test]
fn interactive_limit_after_eviction_restores_the_capacity_and_lease() {
    let mut ed = Editor::open();
    let a = ed.plan("image.flip", flips());
    ed.render(&a, None);
    assert_eq!((ed.cache.len(), ed.cache.bytes()), (2, 3 * MIB));
    let bulk = ed
        .budget
        .try_acquire(6 * MIB, Class::Bulk)
        .unwrap()
        .unwrap();
    // 出力 3.5 MiB（入力は外の値なので数えない）。キャッシュを外の値（1 MiB）まで追い出しても 1 ＋ 3.5 > 4
    let g = graph(
        vec![node(
            1,
            "image.resize",
            json!({ "width": { "px": 1024 }, "height": { "px": 896 }, "filter": "nearest" }),
            INPUT,
            true,
        )],
        vec![(0, at(1), None)],
    );
    let big = ed.engine.compile(&g, &shapes_of(&ed.inputs)).unwrap();
    let est = ed.engine.estimate(&big, &ed.spec(&big), None).unwrap();
    assert_eq!(est.peak, 7 * MIB / 2);
    let used = ed.budget.usage().unwrap();
    let (capacity, held) = (ed.cache.capacity(), ed.held.bytes());
    let evicted = ed.cache.stats().evicted;
    let r = ed.admit(&big);
    assert!(matches!(r, Err(EngineError::Limit(_))), "{r:?}");
    assert_eq!(
        ed.cache.stats().evicted,
        evicted + 2,
        "固定していない 2 つは追い出した"
    );
    assert_eq!((ed.cache.len(), ed.cache.bytes()), (0, MIB), "外の値は残る");
    assert_eq!(
        (ed.cache.capacity(), ed.held.bytes()),
        (capacity, held),
        "上限と確保は元に戻す"
    );
    assert_eq!(ed.budget.usage().unwrap(), used);
    // 上限が戻っているので、settle を挟まずに次の描画で値を覚えられる（一括がまだ持っているので、上限は 4 − ピーク 2 ＝
    // 2 MiB まで下がり、外の値 1 MiB と値 1 つ。上限が 0.5 MiB のまま残ると、1 つも覚えられない）
    ed.render(&a, None);
    assert_eq!(ed.cache.len(), 1);
    drop(bulk);
    ed.render(&big, None);
}

/// E25：対話の確保（キャッシュなし）は待たない：一括が持っていて足りなければ、その場で Limit。
#[test]
fn interactive_never_waits_for_the_budget() {
    let engine = engine();
    let inputs = inputs_of(mib_image(6), Digest([6; 32]));
    let plan = resize_plan(&engine, &inputs);
    let spec = EstimateSpec::new(1);
    let est = engine.estimate(&plan, &spec, None).unwrap();
    let budget = MemoryBudget::new(est.peak + MIB);
    let _bulk = budget.try_acquire(2 * MIB, Class::Bulk).unwrap().unwrap();
    let r = engine.admit(&plan, &spec, &budget, Class::Interactive, &NoMonitor);
    assert!(matches!(r, Err(EngineError::Limit(_))), "{r:?}");
    assert_eq!(budget.used().unwrap(), 2 * MIB);
}

/// 待っている一括の数が `n` になるまで待つ。
fn wait_for_waiting(b: &MemoryBudget, n: usize) {
    while b.usage().unwrap().waiting != n {
        std::thread::yield_now();
    }
}

/// E25：収まる一括は、ほかの一括が返すまで待って通り、描いた値はふつうに描いたものと同じ。
#[test]
fn bulk_waits_and_passes() {
    let engine = engine();
    let inputs = inputs_of(mib_image(7), Digest([7; 32]));
    let plan = resize_plan(&engine, &inputs);
    let spec = EstimateSpec::new(3).digests(true);
    let est = engine.estimate(&plan, &spec, None).unwrap();
    let budget = MemoryBudget::new(est.peak + MIB);
    let hold = budget.acquire(2 * MIB, Class::Bulk, &NoMonitor).unwrap();
    let want = engine.render(&plan, &inputs, &NoMonitor).unwrap();
    #[allow(clippy::disallowed_methods)]
    let got = std::thread::scope(|sc| {
        let h = sc.spawn(|| {
            admit_and_render(
                &engine,
                &plan,
                &spec,
                &inputs,
                &budget,
                Class::Bulk,
                &NoMonitor,
            )
        });
        wait_for_waiting(&budget, 1);
        drop(hold);
        h.join().unwrap()
    })
    .unwrap();
    assert_eq!(got.outputs[0].digest, want.outputs[0].digest);
    assert_eq!(budget.used().unwrap(), 0);
}

/// E24・E25：予算を待っている一括を中止すると Canceled で、使用量も待ちの列も変わらない（描かない）。
#[test]
fn cancel_while_waiting_for_the_budget() {
    let engine = engine();
    let inputs = inputs_of(mib_image(8), Digest([8; 32]));
    let plan = resize_plan(&engine, &inputs);
    let spec = EstimateSpec::new(3);
    let est = engine.estimate(&plan, &spec, None).unwrap();
    let budget = MemoryBudget::new(est.peak + MIB);
    let _hold = budget.acquire(2 * MIB, Class::Bulk, &NoMonitor).unwrap();
    let flag = CancelFlag::default();
    #[allow(clippy::disallowed_methods)]
    let r: Result<Admission, EngineError> = std::thread::scope(|sc| {
        let h = sc.spawn(|| engine.admit(&plan, &spec, &budget, Class::Bulk, &flag));
        wait_for_waiting(&budget, 1);
        flag.cancel();
        h.join().unwrap()
    });
    assert!(matches!(r, Err(EngineError::Canceled)), "{r:?}");
    let usage = budget.usage().unwrap();
    assert_eq!((usage.bulk, usage.waiting), (2 * MIB, 0));
}

/// キャッシュの分の確保：開くときに予算が空いていなければ、空いている分まで上限を下げる。入力だけで予算に収まらなければ
/// 開けない（Limit）。
#[test]
fn cache_lease_takes_what_is_free() {
    let budget = MemoryBudget::new(10 * MIB);
    let bulk = budget.try_acquire(7 * MIB, Class::Bulk).unwrap().unwrap();
    let mut cache = NodeCache::new(0);
    cache.register_external(&mib_image(9));
    let mut held = CacheLease::open(&budget, &mut cache, 4 * MIB).unwrap();
    assert_eq!(
        (held.bytes(), cache.capacity(), held.want()),
        (3 * MIB, 3 * MIB, 4 * MIB)
    );
    drop(bulk);
    held.settle(&mut cache).unwrap();
    assert_eq!((held.bytes(), cache.capacity()), (4 * MIB, 4 * MIB));
    // 入力（外の値）が予算の空きを超える
    let small = MemoryBudget::new(MIB / 2);
    let mut cache = NodeCache::new(0);
    cache.register_external(&mib_image(10));
    assert!(matches!(
        CacheLease::open(&small, &mut cache, MIB),
        Err(EngineError::Limit(_))
    ));
}

/// 段 10 の確認（予算の駐車）：エディタのキャッシュの確保（`CacheLease`。開いている間ずっと持つ対話）が入力で取り置きを
/// 超えて居続けるとき（対話 64・取り置き 50・予算 100）、「予算 − 居続ける対話」を超える一括（45）は駐車し、後ろの収まる
/// 一括（エディタを開いたままの保存 1）を、待たない確保でも待つ確保でも塞がない。エディタを閉じると 45 は通る。
#[test]
fn an_editor_lease_over_the_reservation_does_not_block_a_save() {
    let budget = MemoryBudget::new(100 * MIB);
    let reserved = default_reserved(budget.total());
    assert_eq!(reserved, 50 * MIB);
    budget.set_reserved(reserved).unwrap();
    // 入力 64 MiB（4096² の Rgba8）を外の値として数え、キャッシュの上限は取り置きの量で開く
    let input = InputGen::Noise {
        w: 4096,
        h: 4096,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let mut cache = NodeCache::new(0);
    assert_eq!(cache.register_external(&input), 64 * MIB);
    let held = CacheLease::open(&budget, &mut cache, reserved).unwrap();
    let u = budget.usage().unwrap();
    assert_eq!((u.interactive, u.standing), (64 * MIB, 64 * MIB));
    // 失敗したときに待ちっぱなしにならないよう、45 は中止できるようにしておく
    let big = CancelFlag::default();
    #[allow(clippy::disallowed_methods)]
    let (parked, try_save, save, r45) = std::thread::scope(|sc| {
        // 45：予算 − 取り置き（50）以下だが、予算 − 居続ける対話（36）を超える
        let h45 = sc.spawn(|| {
            budget
                .acquire(45 * MIB, Class::Bulk, &big)
                .map(|l| l.bytes())
        });
        wait_for_waiting(&budget, 1);
        let parked = budget.usage().unwrap().parked;
        let try_save = budget
            .try_acquire(MIB, Class::Bulk)
            .unwrap()
            .map(|l| l.bytes());
        // 塞がれていたら、約 1 秒で中止して見張る
        let save = budget
            .acquire(MIB, Class::Bulk, &CancelAfter::new(500))
            .map(|l| l.bytes());
        if try_save.is_none() || save.is_err() {
            big.cancel();
            return (parked, try_save, save, h45.join().unwrap());
        }
        // エディタを閉じる（確保を返し、取り置きを外す）と 45 が通る
        drop(held);
        budget.set_reserved(0).unwrap();
        (parked, try_save, save, h45.join().unwrap())
    });
    assert_eq!(parked, 1, "45 は駐車する");
    assert_eq!(
        try_save,
        Some(MIB),
        "待たない保存が、駐車しない 45 に塞がれた"
    );
    assert_eq!(save.unwrap(), MIB, "待つ保存が、駐車しない 45 に塞がれた");
    assert_eq!(r45.unwrap(), 45 * MIB);
    assert_eq!(budget.used().unwrap(), 0);
}
