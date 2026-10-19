//! E4 の本体（段 10。docs/editing.md §12・§17）：エディタの経路と一括の経路が、どの手のあとでも同じ値・同じ書き出しのバイト列・
//! 同じ警告になる。
//!
//! - エディタの経路：`EditSession` の手 → `Engine::compile_snapshot`（前の計画から差分で）→ `Engine::render_cached`（差分の
//!   適用を含む。3 スレッドのプール。キャッシュの上限は 0・4 KiB・64 KiB・1 GiB）。ときどき中止を挟む。
//! - 一括の経路：`Snapshot::to_canonical_json` → 別のエンジンで `load` → `compile` → `render`（キャッシュなし。1・16 スレッド）。
//! - 差分の計画は `compile(&to_graph())` と同じ計画（各ノードの形・写し方・手間・申告・鍵、出力の形、最後の使い手、指摘）。
//! - 実行器の差分の適用（E21）：描画の例を切れ目ごとに追記して描いた値が、全体を当て直したものとビット単位で同じ（取り消しの
//!   あとに別の筆を足しても）。
//! - セッションの鍵と compile の鍵は、同じ文書で同じ（指紋の材料のセッションの側）。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use kura_engine::analysis::Need;
use kura_engine::codec::CodecSet;
use kura_engine::doc::{
    Dither, Expand, FormatChoice, Graph, InputId, NodeId, OutputId, QUANTIZE_VERSION, Quantize,
};
use kura_engine::geom::{IPoint, IRect};
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::monitor::CancelAfter;
use kura_engine::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, spec,
};
use kura_engine::paint::PointList;
use kura_engine::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::session::{CommitMode, Edit, EditSession, HistoryLimits, Snapshot};
use kura_engine::shape::{Shape, ValueKind};
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::value::{LoopRange, PixelFormat, Slice, Value};
use kura_engine::{Digest, EngineError, Issue, IssueCode, Issues, NoMonitor, determinism};
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;
use serde_json::{Value as J, json};

// ---------------------------------------------------------------------------------------------
// 補助
// ---------------------------------------------------------------------------------------------

fn below(r: &mut ChaCha8Rng, n: u64) -> u64 {
    ((u128::from(r.next_u64()) * u128::from(n.max(1))) >> 64) as u64
}

fn chance(r: &mut ChaCha8Rng, percent: u64) -> bool {
    below(r, 100) < percent
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
}

/// 入力の種類（列ごとに 1 つ選ぶ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rgba,
    Indexed,
    Frames,
    Sheet,
    Audio,
}

const KINDS: [Kind; 5] = [
    Kind::Rgba,
    Kind::Indexed,
    Kind::Frames,
    Kind::Sheet,
    Kind::Audio,
];

impl Kind {
    fn input(self, seed: u64) -> InputGen {
        match self {
            Kind::Rgba => InputGen::Noise {
                w: 40,
                h: 30,
                fmt: PixelFormat::Rgba8,
                seed,
            },
            Kind::Indexed => InputGen::Noise {
                w: 24,
                h: 20,
                fmt: PixelFormat::Indexed8,
                seed,
            },
            Kind::Frames => InputGen::NoiseFrames {
                w: 12,
                h: 10,
                fmt: PixelFormat::Rgba8,
                count: 3,
                seed,
            },
            // 格子・スライス・ピボットの写し方にも届くように
            Kind::Sheet => InputGen::WithMeta {
                base: Box::new(InputGen::Sheet {
                    cols: 3,
                    rows: 2,
                    cell: 6,
                }),
                meta: MetaGen {
                    slices: vec![Slice {
                        name: "s".into(),
                        rect: IRect {
                            x: 2,
                            y: 1,
                            w: 9,
                            h: 7,
                        },
                        center: None,
                        pivot: Some(IPoint { x: 5, y: 6 }),
                    }],
                    ..MetaGen::default()
                },
            },
            Kind::Audio => InputGen::Sine {
                rate: 44_100,
                channels: 2,
                frames: 2_000,
                freq: 440,
                loop_range: Some(LoopRange {
                    start: 100,
                    end: 1_500,
                }),
            },
        }
    }

    fn value_kind(self) -> ValueKind {
        match self {
            Kind::Frames => ValueKind::Frames,
            Kind::Audio => ValueKind::Audio,
            _ => ValueKind::Image,
        }
    }
}

/// 列の入力（値・元のハッシュ）。
struct Input {
    value: Value,
    source: Digest,
}

impl Input {
    fn new(source_gen: &InputGen) -> Self {
        Self {
            value: source_gen.generate().unwrap(),
            source: kura_engine::testing::source_of(std::slice::from_ref(source_gen)).unwrap(),
        }
    }

    fn shapes(&self) -> BTreeMap<InputId, Shape> {
        BTreeMap::from([(InputId(0), self.value.shape())])
    }

    fn fingerprinted(&self) -> BTreeMap<InputId, FingerprintedInput> {
        BTreeMap::from([(
            InputId(0),
            FingerprintedInput::new(self.value.clone(), self.source).unwrap(),
        )])
    }

    fn plain(&self) -> BTreeMap<InputId, InputValue> {
        BTreeMap::from([(
            InputId(0),
            InputValue {
                value: self.value.clone(),
                source: self.source,
            },
        )])
    }
}

/// 足せる操作 1 つ（ID・版・引数の記述・例の引数・入力の種類）。
#[derive(Clone)]
struct MenuOp {
    id: &'static str,
    v: u16,
    schema: ParamSchema,
    examples: Vec<J>,
    accepts: &'static [ValueKind],
    lift: Lift,
}

impl MenuOp {
    fn fits(&self, kind: ValueKind) -> bool {
        self.accepts.contains(&kind)
            || (kind == ValueKind::Frames
                && self.lift == Lift::PerFrame
                && self.accepts.contains(&ValueKind::Image))
    }
}

fn menu(engine: &Engine) -> Vec<MenuOp> {
    engine
        .registry()
        .current()
        .into_iter()
        .map(|op| {
            let sig = op.signature();
            MenuOp {
                id: op.id(),
                v: op.version(),
                schema: op.schema(),
                examples: op
                    .examples()
                    .unwrap()
                    .into_iter()
                    .map(|e| e.params)
                    .collect(),
                accepts: sig.inputs[0].accepts,
                lift: sig.lift,
            }
        })
        .collect()
}

/// 列の入力で計画を作れる例だけを残した表（たいていの手が描ける文書になるように）。
fn fitted(menu: &[MenuOp], engine: &Engine, kind: Kind) -> Vec<MenuOp> {
    let shapes = Input::new(&kind.input(0)).shapes();
    menu.iter()
        .map(|m| MenuOp {
            examples: m
                .examples
                .iter()
                .filter(|p| {
                    let mut g = Graph::new_single();
                    g.push_linear(m.id, m.v, (*p).clone(), 0).unwrap();
                    engine.compile(&g, &shapes).is_ok()
                })
                .cloned()
                .collect(),
            ..m.clone()
        })
        .collect()
}

fn find<'m>(menu: &'m [MenuOp], id: &str) -> Option<&'m MenuOp> {
    menu.iter().find(|m| m.id == id)
}

fn random_color(r: &mut ChaCha8Rng, alpha: bool) -> J {
    let gray = chance(r, 30);
    let v = below(r, 256);
    let c: Vec<u64> = (0..3)
        .map(|_| if gray { v } else { below(r, 256) })
        .collect();
    if alpha {
        let a = if chance(r, 50) { 255 } else { below(r, 256) };
        json!([c[0], c[1], c[2], a])
    } else {
        json!([c[0], c[1], c[2]])
    }
}

/// 引数の記述に沿ったランダムな値（ときどき範囲の外や知らない項目も混ぜる。tests/random_docs.rs と同じ考え方）。
fn random_params(r: &mut ChaCha8Rng, schema: &ParamSchema, kind: Kind) -> J {
    let mut map = serde_json::Map::new();
    for spec in &schema.params {
        let v = match &spec.kind {
            ParamKind::Int { min, max, .. } if spec.unit == Unit::Samples => match below(r, 10) {
                0 => json!(*max + 1),
                1 => J::Null,
                _ => json!(*min + below(r, 2_500) as i64),
            },
            ParamKind::Int { min, max, .. } => {
                json!(*min - 1 + below(r, ((*max - *min) as u64).saturating_add(3)) as i64)
            }
            ParamKind::Float { min, max, .. } => {
                json!(*min - 0.5 + (max - min + 1.0) * (below(r, 1_000_001) as f64 / 1_000_000.0))
            }
            ParamKind::Bool => json!(chance(r, 50)),
            ParamKind::Enum { values } => {
                if chance(r, 8) {
                    json!("nonexistent")
                } else {
                    json!(values[below(r, values.len() as u64) as usize])
                }
            }
            ParamKind::Rect => json!({
                "x": below(r, 30) as i64 - 2, "y": below(r, 24) as i64 - 2,
                "w": below(r, 30), "h": below(r, 24),
            }),
            ParamKind::Point => {
                json!({ "x": below(r, 40) as i64 - 5, "y": below(r, 40) as i64 - 5 })
            }
            ParamKind::Color { alpha } => random_color(r, *alpha),
            ParamKind::Strokes => {
                J::Array((0..below(r, 3)).map(|_| paint_action(r, kind)).collect())
            }
            ParamKind::FrameRange => match below(r, 4) {
                0 => J::Null,
                _ => {
                    let start = below(r, 1_500);
                    json!({ "start": start, "end": start + 1 + below(r, 1_000) })
                }
            },
            _ => spec.default.clone(),
        };
        map.insert(spec.key.to_string(), v);
    }
    if chance(r, 4) {
        map.insert("unexpected".into(), json!(1));
    }
    J::Object(map)
}

/// 操作の引数：たいていは列の入力で描ける例の引数（描画は描ける動作）、ときどきランダム（範囲の外を含む）。
fn op_params(r: &mut ChaCha8Rng, op: &MenuOp, kind: Kind) -> J {
    if op.id == "paint.actions" && chance(r, 85) {
        return json!({ "actions": (0..below(r, 4)).map(|_| paint_action(r, kind)).collect::<Vec<_>>() });
    }
    if op.examples.is_empty() || chance(r, 8) {
        return random_params(r, &op.schema, kind);
    }
    op.examples[below(r, op.examples.len() as u64) as usize].clone()
}

/// 描画の動作 1 つ。たいていは入力の形式で描けるもの、ときどき描けないもの（読めない・推論で断る・実行で断る）。
fn paint_action(r: &mut ChaCha8Rng, kind: Kind) -> J {
    let (w, h, frames) = match kind {
        Kind::Rgba => (40, 30, 1),
        Kind::Indexed => (24, 20, 1),
        Kind::Frames => (12, 10, 3),
        Kind::Sheet => (18, 12, 1),
        Kind::Audio => (8, 8, 1),
    };
    // 10 回に 1 回ほどは描けない動作（ないフレーム・形式に合わない色・読めない種類・筆の大きさ 0）
    let bad = if chance(r, 10) { below(r, 4) } else { 9 };
    let frame = if bad == 0 {
        frames + below(r, 2)
    } else {
        below(r, frames)
    };
    let indexed = kind == Kind::Indexed;
    let color = if bad == 1 {
        // 形式に合わない色
        if indexed {
            json!({ "rgba": random_color(r, true) })
        } else {
            json!({ "index": below(r, 20) })
        }
    } else if indexed {
        json!({ "index": below(r, 17) })
    } else {
        json!({ "rgba": random_color(r, true) })
    };
    let point = |r: &mut ChaCha8Rng| IPoint {
        x: below(r, w + 8) as i64 - 4,
        y: below(r, h + 8) as i64 - 4,
    };
    if bad == 2 {
        // 読めない動作
        return json!({ "type": "smudge", "frame": 0 });
    }
    match below(r, 20) {
        0..=3 => json!({
            "type": "fill",
            "at": point(r),
            "color": color,
            "tolerance": if indexed { 0 } else { below(r, 60) },
            "connectivity": if chance(r, 50) { "four" } else { "eight" },
            "contiguous": chance(r, 70),
            "frame": frame,
        }),
        4 if !indexed => json!({
            "type": "stroke", "tool": "eraser", "color": J::Null, "blend": "replace",
            "brush": { "shape": "square", "size": 1 + below(r, 4) },
            "pixelPerfect": false, "frame": frame,
            "points": PointList(vec![point(r), point(r)]).pack(),
        }),
        _ => {
            let points: Vec<IPoint> = (0..1 + below(r, 5)).map(|_| point(r)).collect();
            json!({
                "type": "stroke",
                "tool": "pen",
                "color": color,
                "blend": if indexed || chance(r, 60) { "replace" } else { "over" },
                "brush": {
                    "shape": if chance(r, 50) { "square" } else { "circle" },
                    // ときどき 0（推論で断る）
                    "size": if bad == 3 { 0 } else { 1 + below(r, 6) },
                },
                "pixelPerfect": chance(r, 30),
                "frame": frame,
                "points": PointList(points).pack(),
            })
        }
    }
}

/// 小さな文書（0〜3 個の操作。入力に合う操作を多めに）。
fn random_graph(r: &mut ChaCha8Rng, menu: &[MenuOp], kind: Kind) -> Graph {
    let mut g = Graph::new_single();
    for _ in 0..below(r, 4) {
        let op = pick_op(r, menu, kind);
        let params = op_params(r, op, kind);
        g.push_linear(op.id, op.v, params, r.next_u64()).unwrap();
    }
    g
}

/// 足す操作（たいていは列の入力で描ける例のある操作。画像では描画を多めに。ときどき合わない操作）。
fn pick_op<'m>(r: &mut ChaCha8Rng, menu: &'m [MenuOp], kind: Kind) -> &'m MenuOp {
    if kind != Kind::Audio && chance(r, 30) {
        return find(menu, "paint.actions").unwrap();
    }
    let fits: Vec<&MenuOp> = menu
        .iter()
        .filter(|m| m.fits(kind.value_kind()) && !m.examples.is_empty())
        .collect();
    if fits.is_empty() || chance(r, 5) {
        return &menu[below(r, menu.len() as u64) as usize];
    }
    fits[below(r, fits.len() as u64) as usize]
}

/// 今の文書の手 1 つ。
fn random_edit(r: &mut ChaCha8Rng, menu: &[MenuOp], g: &Graph, kind: Kind) -> Edit {
    let n = g.nodes.len() as u64;
    let any = |r: &mut ChaCha8Rng| g.nodes[below(r, n) as usize].clone();
    let paint: Vec<NodeId> = g
        .nodes
        .iter()
        .filter(|x| x.op == "paint.actions")
        .map(|x| x.id)
        .collect();
    match below(r, 20) {
        0..=6 if !paint.is_empty() => Edit::AppendItems {
            node: paint[below(r, paint.len() as u64) as usize],
            field: "actions".into(),
            items: (0..1 + below(r, 3))
                .map(|_| paint_action(r, kind))
                .collect(),
        },
        7..=9 if n > 0 => {
            let node = any(r);
            let op = find(menu, &node.op);
            match op.filter(|o| !o.schema.params.is_empty()) {
                Some(op) => {
                    let spec = &op.schema.params[below(r, op.schema.params.len() as u64) as usize];
                    let from = op_params(r, op, kind);
                    let value = from.get(spec.key).cloned().unwrap_or(J::Null);
                    Edit::SetParam {
                        node: node.id,
                        key: spec.key.into(),
                        value,
                    }
                }
                None => Edit::SetEnabled {
                    node: node.id,
                    enabled: chance(r, 50),
                },
            }
        }
        10 if n > 0 => {
            let node = any(r);
            let params = match find(menu, &node.op) {
                Some(op) => op_params(r, op, kind),
                None => json!({}),
            };
            Edit::SetParams {
                node: node.id,
                params,
            }
        }
        11 if n > 0 => Edit::RemoveNode { node: any(r).id },
        12 if n > 1 => Edit::MoveNode {
            node: any(r).id,
            to: below(r, n) as usize,
        },
        13 if n > 0 => Edit::SetEnabled {
            node: any(r).id,
            enabled: chance(r, 50),
        },
        14 => Edit::Replace {
            graph: random_graph(r, menu, kind),
        },
        15 if kind == Kind::Audio => Edit::SetOutput {
            output: OutputId(0),
            format: FormatChoice::Keep,
            quantize: if chance(r, 60) {
                Some(Quantize {
                    v: QUANTIZE_VERSION,
                    bits: [8, 16, 24][below(r, 3) as usize],
                    dither: if chance(r, 50) {
                        Dither::Tpdf
                    } else {
                        Dither::None
                    },
                    seed: below(r, 4),
                })
            } else {
                None
            },
            expand: Expand::One,
        },
        _ => {
            let op = pick_op(r, menu, kind);
            let params = op_params(r, op, kind);
            Edit::AddNode {
                at: below(r, n + 1) as usize,
                op: op.id.into(),
                v: if chance(r, 4) { 99 } else { op.v },
                params,
                seed: if chance(r, 50) {
                    Some(below(r, 5))
                } else {
                    None
                },
                extra_inputs: Vec::new(),
            }
        }
    }
}

/// セッションへの手 1 つ（確定・まとめる確定・仮の値・取り消し・やり直しなど）。編集したノード（あれば）を返す。
/// `broken`（今の文書の計画を作れない）なら、たいていは取り消す（描けない文書が続いて照合が空回りしないように）。
fn random_step(
    r: &mut ChaCha8Rng,
    engine: &Engine,
    menu: &[MenuOp],
    s: &mut EditSession,
    kind: Kind,
    broken: bool,
) -> Option<NodeId> {
    if broken && chance(r, 70) {
        if s.is_provisional() {
            s.cancel_preview();
        } else {
            s.undo();
        }
        return None;
    }
    let g = s.current().to_graph();
    let touched = |e: &Edit| match e {
        Edit::AppendItems { node, .. }
        | Edit::SetParam { node, .. }
        | Edit::SetParams { node, .. }
        | Edit::SetEnabled { node, .. } => Some(*node),
        _ => None,
    };
    let result = match below(r, 20) {
        0..=10 => {
            let e = random_edit(r, menu, &g, kind);
            let node = touched(&e);
            let mode = if chance(r, 30) {
                CommitMode::Merge
            } else {
                CommitMode::Step
            };
            s.commit(engine, e, mode).map(|_| node)
        }
        11 | 12 => {
            let e = random_edit(r, menu, &g, kind);
            let node = touched(&e);
            s.preview(engine, e).map(|_| node)
        }
        13 => Ok(s.cancel_preview().and(None)),
        14 => s
            .commit_preview(if chance(r, 50) {
                CommitMode::Merge
            } else {
                CommitMode::Step
            })
            .map(|_| None),
        15 => {
            s.seal();
            Ok(None)
        }
        16 | 17 => Ok(s.undo().and(None)),
        _ => Ok(s.redo().and(None)),
    };
    match result {
        Ok(node) => node,
        // 断った手は状態を変えない（内部の誤りは出ない）
        Err(EngineError::Invalid(_) | EngineError::Limit(_)) => None,
        Err(e) => panic!("セッションの手が内部の誤りになりました: {e}"),
    }
}

/// 計画の比べる中身：各ノードの（ID・推論・申告・鍵）、出力の形、最後の使い手、手間の合計、段。
type Summary = (
    Vec<(NodeId, Inferred, Vec<Need>, Option<Digest>)>,
    Vec<(OutputId, Shape)>,
    Vec<Vec<NodeId>>,
    u64,
    u8,
);

fn summary(p: &Plan) -> Summary {
    let ids = p.node_ids();
    (
        ids.iter()
            .map(|id| {
                (
                    *id,
                    p.node_inferred(*id).cloned().unwrap(),
                    p.node_needs(*id).unwrap().to_vec(),
                    p.node_key(*id),
                )
            })
            .collect(),
        p.output_shapes(),
        (0..ids.len())
            .map(|i| p.released_after(i).to_vec())
            .collect(),
        p.cost(),
        p.level(),
    )
}

fn assert_same_plan(a: &Result<Plan, Issues>, b: &Result<Plan, Issues>, at: &str) {
    match (a, b) {
        (Ok(a), Ok(b)) => assert_eq!(summary(a), summary(b), "{at}"),
        (Err(a), Err(b)) => assert_eq!(a, b, "{at}"),
        (a, b) => panic!(
            "{at}：計画を作れたかが違います（差分 {}・全体 {}）: {:?} / {:?}",
            a.is_ok(),
            b.is_ok(),
            a.as_ref().err(),
            b.as_ref().err()
        ),
    }
}

/// 描いた結果の比べる中身：出力ごとのダイジェストと固定のコーデックで書いたバイト列（書けなければその誤り）、警告。
#[derive(Debug, PartialEq)]
struct Outcome {
    outputs: Vec<(OutputId, Digest, Result<Vec<u8>, String>)>,
    warnings: Vec<Issue>,
}

fn encoded(codecs: &CodecSet, v: &Value) -> Result<Vec<u8>, String> {
    let format = match v {
        Value::Audio(_) => "wav",
        _ => "png",
    };
    codecs
        .encode_bytes(v, format, &json!({}), &NoMonitor)
        .map(|(bytes, _)| bytes)
        .map_err(|e| e.to_string())
}

fn outcome(
    codecs: &CodecSet,
    outputs: Vec<(OutputId, Value, Option<Digest>)>,
    warnings: Vec<Issue>,
) -> Outcome {
    Outcome {
        outputs: outputs
            .into_iter()
            .map(|(id, v, d)| {
                let digest = d.unwrap_or_else(|| v.digest().unwrap());
                (id, digest, encoded(codecs, &v))
            })
            .collect(),
        warnings,
    }
}

/// 数え上げ（列が偏って照合が空回りしていないことを確かめる）。
#[derive(Debug, Default)]
struct Counts {
    steps: u64,
    planned: u64,
    rendered: u64,
    render_errors: u64,
    canceled: u64,
    hits: u64,
    appended: u64,
}

// ---------------------------------------------------------------------------------------------
// E4 の本体
// ---------------------------------------------------------------------------------------------

/// 1 つの列：エディタの経路と一括の経路を、各手のあとに照らす。
#[allow(clippy::too_many_arguments)]
fn run_e4_sequence(
    seed: u64,
    steps: usize,
    capacity: u64,
    editor: &Engine,
    bulk: &Engine,
    menu: &[MenuOp],
    pools: &[rayon::ThreadPool; 3],
    codecs: &CodecSet,
    counts: &mut Counts,
) {
    let mut r = determinism::rng(0xe4_0000 + seed);
    let kind = KINDS[(seed % KINDS.len() as u64) as usize];
    let input = Input::new(&kind.input(seed));
    let (shapes, fp_inputs, plain) = (input.shapes(), input.fingerprinted(), input.plain());
    let menu = &fitted(menu, editor, kind);
    let mut s = EditSession::open(
        editor,
        &random_graph(&mut r, menu, kind),
        Digest([1; 32]),
        HistoryLimits::default(),
    )
    .unwrap();
    let mut cache = NodeCache::new(capacity);
    let mut prev: Option<Plan> = None;
    let mut focus = None;
    let mut broken = false;
    for step in 0..steps {
        if step > 0
            && let Some(n) = random_step(&mut r, editor, menu, &mut s, kind, broken)
        {
            focus = Some(n);
        }
        counts.steps += 1;
        let at = format!("列 {seed}（{kind:?}・上限 {capacity}）の手 {step}");
        let snap = s.current().clone();
        // エディタの経路の計画（前の計画から差分で）
        let planned = editor.compile_snapshot(&snap, &shapes, prev.as_ref());
        // 一括の経路の計画（正規の JSON から、別のエンジンで）
        let doc = bulk.load(&snap.to_canonical_json().unwrap()).unwrap();
        let bulk_plan = bulk.compile(&doc, &shapes);
        assert_same_plan(&planned, &bulk_plan, &at);
        broken = planned.is_err();
        let (Ok(plan), Ok(bulk_plan)) = (planned, bulk_plan) else {
            continue;
        };
        counts.planned += 1;
        let opts = RenderOptions {
            digests: chance(&mut r, 50),
            focus,
            verify_hits: false,
        };
        // ときどき中止を挟む（中止のあともキャッシュが正しいこと）
        if chance(&mut r, 12) {
            let mon = CancelAfter::new(below(&mut r, 6));
            match pools[1]
                .install(|| editor.render_cached(&plan, &fp_inputs, &mut cache, &opts, &mon))
            {
                Ok(_) => {}
                Err(EngineError::Canceled) => counts.canceled += 1,
                Err(EngineError::Internal(m)) => panic!("{at}：中止の描画が内部の誤り: {m}"),
                Err(_) => {}
            }
        }
        let ed = pools[1]
            .install(|| editor.render_cached(&plan, &fp_inputs, &mut cache, &opts, &NoMonitor));
        let b1 = pools[0].install(|| bulk.render(&bulk_plan, &plain, &NoMonitor));
        let b16 = pools[2].install(|| bulk.render(&bulk_plan, &plain, &NoMonitor));
        match (ed, b1, b16) {
            (Ok(ed), Ok(b1), Ok(b16)) => {
                counts.rendered += 1;
                counts.hits += ed.stats.hits;
                counts.appended += ed.stats.appended;
                let e = outcome(
                    codecs,
                    ed.outputs
                        .into_iter()
                        .map(|o| (o.id, o.value, o.digest))
                        .collect(),
                    ed.warnings,
                );
                let one = outcome(
                    codecs,
                    b1.outputs
                        .into_iter()
                        .map(|o| (o.id, o.value, Some(o.digest)))
                        .collect(),
                    b1.warnings,
                );
                assert_eq!(
                    e, one,
                    "{at}：エディタの経路と一括の経路（1 スレッド）が違います"
                );
                let sixteen: Vec<(OutputId, Digest)> =
                    b16.outputs.iter().map(|o| (o.id, o.digest)).collect();
                let want: Vec<(OutputId, Digest)> =
                    one.outputs.iter().map(|o| (o.0, o.1)).collect();
                assert_eq!(sixteen, want, "{at}：16 スレッドの一括の経路が違います");
                assert_eq!(b16.warnings, one.warnings, "{at}");
            }
            (Err(a), Err(b), Err(c)) => {
                counts.render_errors += 1;
                for e in [&a, &b, &c] {
                    assert!(
                        !matches!(e, EngineError::Internal(_)),
                        "{at}：内部の誤り: {e}"
                    );
                }
            }
            (a, b, c) => panic!(
                "{at}：描けたかが違います（エディタ {}・一括 1 {}・一括 16 {}）: {:?} {:?} {:?}",
                a.is_ok(),
                b.is_ok(),
                c.is_ok(),
                a.err(),
                b.err(),
                c.err()
            ),
        }
        prev = Some(plan);
    }
}

/// E4：ChaCha8 の編集の列（引数・追記・外す・取り消し・やり直し・置き換え・仮の値・中止）で、エディタの経路（差分の計画・
/// キャッシュ・差分の適用、3 スレッド）と一括の経路（別のエンジンで読み直して描く、1・16 スレッド）の値のダイジェスト・
/// 固定のコーデックで書いた PNG・WAV のバイト列・警告が、どの手のあとでも同じ。キャッシュの上限は 0・4 KiB・64 KiB・1 GiB。
#[test]
fn random_edit_sequences_match_the_bulk_path() {
    let editor = Engine::new().unwrap();
    let bulk = Engine::new().unwrap();
    let menu = menu(&editor);
    let pools = [pool(1), pool(3), pool(16)];
    let codecs = CodecSet::pinned();
    let mut counts = Counts::default();
    let capacities = [0u64, 4 << 10, 64 << 10, 1 << 30];
    for seed in 0..20u64 {
        let capacity = capacities[(seed / KINDS.len() as u64 % 4) as usize];
        run_e4_sequence(
            seed,
            30,
            capacity,
            &editor,
            &bulk,
            &menu,
            &pools,
            &codecs,
            &mut counts,
        );
    }
    eprintln!("E4 の数え上げ：{counts:?}");
    // 偏っていないこと：計画を作れた・描けた・描けなかった・当たり・差分の適用・中止のどれも起きる
    assert!(counts.planned * 2 > counts.steps, "{counts:?}");
    assert!(counts.rendered * 3 > counts.steps, "{counts:?}");
    assert!(counts.render_errors > 0, "{counts:?}");
    assert!(counts.hits > 50, "{counts:?}");
    assert!(counts.appended > 20, "{counts:?}");
    assert!(counts.canceled > 0, "{counts:?}");
}

// ---------------------------------------------------------------------------------------------
// 差分の計画と compile
// ---------------------------------------------------------------------------------------------

/// ランダムな編集の列で、差分の計画（`compile_snapshot`。前の計画から）が `compile(&to_graph())` と同じ。入力の大きさも
/// ときどき変え（形が変わったノードは推論だけやり直す道）、前の計画を渡さない場合・別のエンジンの計画を渡す場合も照らす。
#[test]
fn compile_snapshot_equals_compile() {
    let engine = Engine::new().unwrap();
    let other = Engine::new().unwrap();
    let menu = menu(&engine);
    let mut appended_steps = 0;
    for seed in 0..30u64 {
        let mut r = determinism::rng(0xc0_0000 + seed);
        let kind = KINDS[(seed % KINDS.len() as u64) as usize];
        let menu = fitted(&menu, &engine, kind);
        let mut s = EditSession::open(
            &engine,
            &random_graph(&mut r, &menu, kind),
            Digest([1; 32]),
            HistoryLimits::default(),
        )
        .unwrap();
        let mut size = 0u64;
        let mut prev: Option<Plan> = None;
        let mut broken = false;
        for step in 0..60 {
            let before = s.current().clone();
            random_step(&mut r, &engine, &menu, &mut s, kind, broken);
            if chance(&mut r, 10) {
                size = (size + 1) % 3;
            }
            let shapes = shapes_for(kind, size);
            let snap = s.current().clone();
            if snap
                .diff(&before)
                .nodes
                .iter()
                .any(|(_, c)| matches!(c, kura_engine::session::NodeChange::Appended { .. }))
            {
                appended_steps += 1;
            }
            let at = format!("列 {seed}（{kind:?}）の手 {step}");
            let full = engine.compile(&snap.to_graph(), &shapes);
            let inc = engine.compile_snapshot(&snap, &shapes, prev.as_ref());
            assert_same_plan(&inc, &full, &at);
            assert_same_plan(&engine.compile_snapshot(&snap, &shapes, None), &full, &at);
            broken = full.is_err();
            if let Some(p) = &prev {
                // 別のエンジンの計画は使わない（縛り）。結果は同じ
                let foreign = other.compile_snapshot(&snap, &shapes, Some(p));
                let theirs = other.compile(&snap.to_graph(), &shapes);
                assert_same_plan(&foreign, &theirs, &at);
            }
            if let Ok(p) = inc {
                prev = Some(p);
            }
        }
    }
    assert!(
        appended_steps > 30,
        "追記の手が少なすぎます（{appended_steps}）"
    );
}

/// 入力の種類の形（`size` で少し大きさを変える）。
fn shapes_for(kind: Kind, size: u64) -> BTreeMap<InputId, Shape> {
    let source_gen = match (kind.input(5), size) {
        (g, 0) => g,
        (InputGen::Noise { w, h, fmt, seed }, k) => InputGen::Noise {
            w: w + k as u32,
            h,
            fmt,
            seed,
        },
        (
            InputGen::NoiseFrames {
                w,
                h,
                fmt,
                count,
                seed,
            },
            k,
        ) => InputGen::NoiseFrames {
            w,
            h: h + k as u32,
            fmt,
            count,
            seed,
        },
        (
            InputGen::Sine {
                rate,
                channels,
                frames,
                freq,
                loop_range,
            },
            k,
        ) => InputGen::Sine {
            rate,
            channels,
            frames: frames + k * 100,
            freq,
            loop_range,
        },
        (g, _) => g,
    };
    BTreeMap::from([(InputId(0), source_gen.generate().unwrap().shape())])
}

/// 追記で、変わらない下流のノードは前の計画のノードを共有する（形が前と同じなら推論を打ち切る）。
#[test]
fn appended_plans_share_unchanged_nodes() {
    let engine = Engine::new().unwrap();
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    let paint = g
        .push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
        .unwrap();
    let last = g
        .push_linear("image.rotate90", 1, json!({ "turn": "cw" }), 0)
        .unwrap();
    let mut s = EditSession::open(&engine, &g, Digest([1; 32]), HistoryLimits::default()).unwrap();
    let shapes = Input::new(&Kind::Rgba.input(1)).shapes();
    let first = engine.compile_snapshot(s.current(), &shapes, None).unwrap();
    let mut r = determinism::rng(9);
    s.commit(
        &engine,
        Edit::AppendItems {
            node: paint,
            field: "actions".into(),
            items: vec![stroke_json(&mut r, 3)],
        },
        CommitMode::Step,
    )
    .unwrap();
    let second = engine
        .compile_snapshot(s.current(), &shapes, Some(&first))
        .unwrap();
    assert!(second.shares_node(&first, NodeId(1)));
    assert!(!second.shares_node(&first, paint));
    assert!(second.shares_node(&first, last));
    assert_eq!(
        summary(&second),
        summary(&engine.compile(&s.current().to_graph(), &shapes).unwrap())
    );
}

// ---------------------------------------------------------------------------------------------
// 実行器の差分の適用（E21）
// ---------------------------------------------------------------------------------------------

/// 描ける筆 1 本の JSON（40×30 の RGBA の画像の上を歩く）。
fn stroke_json(r: &mut ChaCha8Rng, size: u8) -> J {
    let pts = kura_engine::testing::bench::walk_stroke(r.next_u64(), 12, 40, 30, 5);
    serde_json::to_value(kura_engine::testing::bench::pen(
        pts,
        size,
        size == 1,
        [200, 30, (r.next_u32() % 256) as u8, 255],
    ))
    .unwrap()
}

fn is_indexed(v: &Value) -> bool {
    match v {
        Value::Image(img) => img.px.format() == PixelFormat::Indexed8,
        Value::Frames(fr) => fr
            .frames()
            .first()
            .is_some_and(|f| f.px.format() == PixelFormat::Indexed8),
        Value::Audio(_) => false,
    }
}

/// エディタの経路で 1 回描き、キャッシュなしで全体を描いたものと照らす。差分の適用で描いたノードの数を返す。
fn render_and_check(
    engine: &Engine,
    snap: &Snapshot,
    input: &Input,
    prev: &mut Option<Plan>,
    cache: &mut NodeCache,
    focus: NodeId,
    at: &str,
) -> u64 {
    let shapes = input.shapes();
    let plan = engine
        .compile_snapshot(snap, &shapes, prev.as_ref())
        .unwrap();
    let got = engine
        .render_cached(
            &plan,
            &input.fingerprinted(),
            cache,
            &RenderOptions {
                digests: true,
                focus: Some(focus),
                verify_hits: false,
            },
            &NoMonitor,
        )
        .unwrap();
    let full_plan = engine.compile(&snap.to_graph(), &shapes).unwrap();
    let want = engine
        .render(&full_plan, &input.plain(), &NoMonitor)
        .unwrap();
    let got_digests: Vec<Digest> = got.outputs.iter().map(|o| o.digest.unwrap()).collect();
    let want_digests: Vec<Digest> = want.outputs.iter().map(|o| o.digest).collect();
    assert_eq!(got_digests, want_digests, "{at}");
    assert_eq!(got.warnings, want.warnings, "{at}");
    *prev = Some(plan);
    got.stats.appended
}

/// E21 を実行器の経路で：描画の例の動作を、ランダムな切れ目ごとにセッションの追記で足し、差分の計画と差分の適用で描いた値が、
/// 全体を当て直したものとビット単位で同じ（警告も）。取り消してから別の筆を足しても同じ。前後に操作のある鎖でも同じ。
#[test]
fn appended_in_executor_equals_full() {
    let engine = Engine::new().unwrap();
    let op = engine
        .registry()
        .current()
        .into_iter()
        .find(|o| o.id() == "paint.actions")
        .unwrap();
    let mut appended = 0u64;
    let mut checked = 0u64;
    for (i, ex) in op.examples().unwrap().into_iter().enumerate() {
        let actions = ex.params["actions"].as_array().unwrap().clone();
        let input = Input::new(&ex.inputs[0]);
        for (round, chain) in [false, true, false].into_iter().enumerate() {
            let mut r = determinism::rng(0xe21_000 + i as u64 * 8 + round as u64);
            let k0 = below(&mut r, actions.len() as u64 + 1) as usize;
            let chain = chain && matches!(input.value, Value::Image(_));
            let mut g = Graph::new_single();
            if chain {
                g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
                    .unwrap();
            }
            let paint = g
                .push_linear("paint.actions", 1, json!({ "actions": actions[..k0] }), 0)
                .unwrap();
            if chain {
                g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
                    .unwrap();
            }
            let mut s =
                EditSession::open(&engine, &g, Digest([1; 32]), HistoryLimits::default()).unwrap();
            let mut cache = NodeCache::new(1 << 30);
            let mut prev = None;
            let at = |what: &str| format!("{}（{round}）{what}", ex.name);
            render_and_check(
                &engine,
                s.current(),
                &input,
                &mut prev,
                &mut cache,
                paint,
                &at("最初"),
            );
            let mut pos = k0;
            while pos < actions.len() {
                let cut = 1 + below(&mut r, (actions.len() - pos) as u64) as usize;
                s.commit(
                    &engine,
                    Edit::AppendItems {
                        node: paint,
                        field: "actions".into(),
                        items: actions[pos..pos + cut].to_vec(),
                    },
                    CommitMode::Step,
                )
                .unwrap();
                pos += cut;
                let used = render_and_check(
                    &engine,
                    s.current(),
                    &input,
                    &mut prev,
                    &mut cache,
                    paint,
                    &at(&format!("{pos} まで")),
                );
                assert_eq!(used, 1, "{}", at("差分の適用を使っていません"));
                appended += used;
                checked += 1;
            }
            // 取り消し（当たる）と、そのあとの別の筆（差分の適用）。トリムしたフレームの列などでも描ける筆にする
            if s.undo().is_some() {
                render_and_check(
                    &engine,
                    s.current(),
                    &input,
                    &mut prev,
                    &mut cache,
                    paint,
                    &at("取り消し"),
                );
                let color = if is_indexed(&input.value) {
                    json!({ "index": 1 })
                } else {
                    json!({ "rgba": [9, 9, 9, 255] })
                };
                let other = json!({
                    "type": "stroke", "tool": "pen", "color": color,
                    "blend": "replace", "brush": { "shape": "square", "size": 1 },
                    "pixelPerfect": false, "frame": 0,
                    "points": PointList(vec![IPoint { x: 1, y: 1 }, IPoint { x: 3, y: 2 }]).pack(),
                });
                s.commit(
                    &engine,
                    Edit::AppendItems {
                        node: paint,
                        field: "actions".into(),
                        items: vec![other],
                    },
                    CommitMode::Step,
                )
                .unwrap();
                let used = render_and_check(
                    &engine,
                    s.current(),
                    &input,
                    &mut prev,
                    &mut cache,
                    paint,
                    &at("取り消しのあとの別の筆"),
                );
                assert_eq!(
                    used,
                    1,
                    "{}",
                    at("取り消しのあとに差分の適用を使っていません")
                );
            }
        }
    }
    assert!(appended > 10 && checked > 10, "{appended} {checked}");
}

// ---------------------------------------------------------------------------------------------
// 鍵（指紋の材料のセッションの側）
// ---------------------------------------------------------------------------------------------

/// 同じ文書で、セッションのノードの鍵（`NodeView::key`）と compile の鍵（`Plan::node_key`）が同じ。差分の計画の鍵と、それで
/// 求めた指紋（`Engine::fingerprints`）も、compile の計画と同じ。すべての操作の例と、ランダムな文書で照らす。
#[test]
fn session_keys_equal_compile_keys() {
    let engine = Engine::new().unwrap();
    let menu = menu(&engine);
    let mut docs: Vec<(Graph, InputGen)> = Vec::new();
    for op in engine.registry().current() {
        for ex in op.examples().unwrap() {
            let mut g = Graph::new_single();
            g.push_linear(op.id(), op.version(), ex.params.clone(), 7)
                .unwrap();
            // 16.0 と 16 のような書き方の違いは正規の形で同じになる
            docs.push((g, ex.inputs[0].clone()));
        }
    }
    let mut r = determinism::rng(0x006b_6579);
    for seed in 0..40u64 {
        let kind = KINDS[(seed % KINDS.len() as u64) as usize];
        docs.push((random_graph(&mut r, &menu, kind), kind.input(seed)));
    }
    let mut compared = 0;
    for (g, source_gen) in docs {
        let input = Input::new(&source_gen);
        let shapes = input.shapes();
        let s = EditSession::open(&engine, &g, Digest([1; 32]), HistoryLimits::default()).unwrap();
        let snap = s.current();
        let Ok(plan) = engine.compile(&snap.to_graph(), &shapes) else {
            continue;
        };
        let inc = engine.compile_snapshot(snap, &shapes, None).unwrap();
        for n in snap.nodes() {
            assert_eq!(Some(n.key), plan.node_key(n.id), "{g:?}");
            assert_eq!(Some(n.key), inc.node_key(n.id), "{g:?}");
            let node = g.nodes.iter().find(|x| x.id == n.id).unwrap();
            assert_eq!(engine.registry().doc_node_key(node).unwrap(), n.key);
            compared += 1;
        }
        let fps = input.fingerprinted();
        assert_eq!(
            engine.fingerprints(&plan, &fps).unwrap(),
            engine.fingerprints(&inc, &fps).unwrap()
        );
    }
    assert!(compared > 40, "{compared}");
}

// ---------------------------------------------------------------------------------------------
// 差分の適用の警告（docs/editing.md §12.4）
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TallyParams {
    items: Vec<u8>,
}

impl OpParams for TallyParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec("items", ParamKind::Strokes, Unit::None, json!([]))],
        }
    }
}

/// 試験用：追記だけの配列 `items` の各要素を、左上のタイルの最初のバイトに足していく。奇数の要素は番号つきの警告を、
/// 255 は同じ文言の警告を出す（差分の適用で組み直した警告の並びと重複の除き方が、全体と同じかを確かめる）。
/// 差分の適用・追記の読み込み・追記の推論を持つ。
struct Tally;

impl Tally {
    fn tally(
        v: &Value,
        items: &[u8],
        first: usize,
        cx: &mut ApplyCx<'_>,
    ) -> Result<Value, EngineError> {
        let Value::Image(img) = v else {
            return Err(EngineError::Internal("画像ではありません".into()));
        };
        let mut px = img.px.clone();
        for (k, item) in items.iter().enumerate() {
            cx.check()?;
            let b = &mut px.tile_mut(0, 0)[0];
            *b = b.wrapping_add(*item);
            if item % 2 == 1 {
                cx.warn(Issue::warning(
                    IssueCode::MetadataDropped,
                    format!("要素 {}", first + k),
                ));
            }
            if *item == 255 {
                cx.warn(Issue::warning(IssueCode::GainLimited, "飽和"));
            }
        }
        let mut img = (**img).clone();
        img.px = px;
        Ok(Value::Image(std::sync::Arc::new(img)))
    }
}

impl Op for Tally {
    const ID: &'static str = "test.tally";
    const VERSION: u16 = 1;
    type Params = TallyParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image],
            }],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }

    fn infer(p: &TallyParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: p.items.len() as u64,
        })
    }

    fn apply(
        p: &TallyParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        Ok(Applied::value(Self::tally(inputs[0], &p.items, 0, cx)?))
    }

    fn apply_appended(
        prev: &TallyParams,
        prev_out: &Value,
        p: &TallyParams,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        let rest = p.items.strip_prefix(prev.items.as_slice())?;
        Some(Self::tally(prev_out, rest, prev.items.len(), cx).map(Applied::value))
    }

    fn parse_appended(
        prev: &TallyParams,
        field: &str,
        items: &[&J],
    ) -> Option<Result<TallyParams, Issues>> {
        if field != "items" {
            return None;
        }
        let mut p = prev.clone();
        for item in items {
            match serde_json::from_value::<u8>((*item).clone()) {
                Ok(v) => p.items.push(v),
                Err(e) => return Some(Err(kura_engine::op::unreadable_params(Self::ID, &e))),
            }
        }
        Some(Ok(p))
    }

    fn infer_appended(
        prev: &TallyParams,
        prev_inferred: &Inferred,
        p: &TallyParams,
        _inputs: &[Shape],
        _cx: &InferCx<'_>,
    ) -> Option<Result<Inferred, Issues>> {
        p.items.strip_prefix(prev.items.as_slice())?;
        Some(Ok(Inferred {
            cost: p.items.len() as u64,
            ..prev_inferred.clone()
        }))
    }

    fn examples() -> Vec<Example<TallyParams>> {
        vec![Example {
            name: "tally",
            params: TallyParams {
                items: vec![1, 2, 255],
            },
            inputs: vec![InputGen::Noise {
                w: 8,
                h: 8,
                fmt: PixelFormat::Rgba8,
                seed: 1,
            }],
        }]
    }
}

/// 差分の適用で描いたノードの警告は、全体を当て直したときと同じ並び・同じ重複の除き方になる（前の操作の警告 ＋ 足した分の
/// 操作の警告 ＋ 写しの警告）。読めない要素の追記も、全体を読んだときと同じ指摘。キャッシュを照らす描画（`verify_hits`）は、
/// 差分の適用も全体で描き直して照らす。
#[test]
fn appended_warnings_follow_the_full_order() {
    let mut registry = kura_engine::op::Registry::empty();
    registry.add_current::<Tally>().unwrap();
    registry
        .add_current::<kura_engine::ops::image::flip::Flip>()
        .unwrap();
    let engine = Engine::with_registry(registry, kura_engine::Limits::default());
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    let node = g
        .push_linear("test.tally", 1, json!({ "items": [3, 4] }), 0)
        .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    let input = Input::new(&InputGen::Noise {
        w: 300,
        h: 20,
        fmt: PixelFormat::Rgba8,
        seed: 2,
    });
    let mut s = EditSession::open(&engine, &g, Digest([1; 32]), HistoryLimits::default()).unwrap();
    let mut cache = NodeCache::new(1 << 30);
    let mut prev = None;
    let mut r = determinism::rng(0x7a11);
    let (mut appended, mut verified) = (0, 0);
    for step in 0..40 {
        let items: Vec<J> = (0..1 + below(&mut r, 3))
            .map(|_| match below(&mut r, 12) {
                0 => json!(255),
                1 => json!(256),
                _ => json!(below(&mut r, 9)),
            })
            .collect();
        let bad = items.iter().any(|v| v.as_u64() == Some(256));
        let at = format!("手 {step}");
        let result = s.commit(
            &engine,
            Edit::AppendItems {
                node,
                field: "items".into(),
                items,
            },
            CommitMode::Step,
        );
        assert!(result.is_ok(), "{at}");
        let snap = s.current().clone();
        let shapes = input.shapes();
        let inc = engine.compile_snapshot(&snap, &shapes, prev.as_ref());
        let full = engine.compile(&snap.to_graph(), &shapes);
        assert_same_plan(&inc, &full, &at);
        if bad {
            assert!(inc.is_err(), "{at}");
            s.undo();
            continue;
        }
        let plan = inc.unwrap();
        let got = engine
            .render_cached(
                &plan,
                &input.fingerprinted(),
                &mut cache,
                &RenderOptions {
                    digests: true,
                    focus: Some(node),
                    verify_hits: step % 3 == 0,
                },
                &NoMonitor,
            )
            .unwrap();
        let want = engine
            .render(&full.unwrap(), &input.plain(), &NoMonitor)
            .unwrap();
        assert_eq!(got.warnings, want.warnings, "{at}");
        assert_eq!(
            got.outputs[0].digest.unwrap(),
            want.outputs[0].digest,
            "{at}"
        );
        assert!(!got.warnings.is_empty(), "{at}");
        appended += got.stats.appended;
        verified += got.stats.verified;
        prev = Some(plan);
    }
    assert!(appended > 20 && verified > 5, "{appended} {verified}");
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlainTallyParams {
    items: Vec<u8>,
}

impl OpParams for PlainTallyParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            // 配列だが追記だけの配列（Strokes）ではない
            params: vec![spec(
                "items",
                ParamKind::PaletteEntries,
                Unit::None,
                json!([]),
            )],
        }
    }
}

/// 試験用：`test.tally` と同じ ID・版・値で、`items` を追記だけの配列にしない実装（登録表の違うエンジンを混ぜたときの確かめ）。
struct PlainTally;

impl Op for PlainTally {
    const ID: &'static str = "test.tally";
    const VERSION: u16 = 1;
    type Params = PlainTallyParams;

    fn signature() -> Signature {
        Tally::signature()
    }

    fn infer(p: &PlainTallyParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: p.items.len() as u64,
        })
    }

    fn apply(
        p: &PlainTallyParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        Ok(Applied::value(Tally::tally(inputs[0], &p.items, 0, cx)?))
    }

    fn examples() -> Vec<Example<PlainTallyParams>> {
        Vec::new()
    }
}

/// §12.4：セッションのノードの鍵は、セッションの登録表の追記だけの配列の扱い（`Registry::append_field`）で求めたもの。
/// `compile_snapshot` は、計画を作るエンジンの登録表と扱いが食い違うノードの鍵を求め直す（セッションの鍵を使わない）ので、
/// 鍵・指紋・計画は、そのエンジンで `compile(&to_graph())` したものと同じ。追記だけの配列とふつうの引数の両方の向きで、
/// 64 個の切れ目をまたぐ配列と、追記したあとの前の計画からの差分の計画でも照らす。
#[test]
fn session_keys_follow_the_registry_of_the_engine() {
    let make = |log: bool| {
        let mut registry = kura_engine::op::Registry::empty();
        if log {
            registry.add_current::<Tally>().unwrap();
        } else {
            registry.add_current::<PlainTally>().unwrap();
        }
        registry
            .add_current::<kura_engine::ops::image::flip::Flip>()
            .unwrap();
        Engine::with_registry(registry, kura_engine::Limits::default())
    };
    let (log, plain) = (make(true), make(false));
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
        .unwrap();
    let items: Vec<u8> = (0..70u8).map(|i| i % 7 * 2).collect();
    let node = g
        .push_linear("test.tally", 1, json!({ "items": items }), 0)
        .unwrap();
    g.push_linear("image.flip", 1, json!({ "axis": "horizontal" }), 0)
        .unwrap();
    let input = Input::new(&InputGen::Noise {
        w: 300,
        h: 20,
        fmt: PixelFormat::Rgba8,
        seed: 6,
    });
    let shapes = input.shapes();
    let fps = input.fingerprinted();
    for (what, session_engine, plan_engine) in [
        (
            "追記だけの配列のセッション・ふつうの引数のエンジン",
            &log,
            &plain,
        ),
        (
            "ふつうの引数のセッション・追記だけの配列のエンジン",
            &plain,
            &log,
        ),
    ] {
        let mut s = EditSession::open(
            session_engine,
            &g,
            Digest([1; 32]),
            HistoryLimits::default(),
        )
        .unwrap();
        let snap = s.current().clone();
        let full = plan_engine.compile(&snap.to_graph(), &shapes).unwrap();
        let inc = plan_engine.compile_snapshot(&snap, &shapes, None).unwrap();
        let session_key = snap.nodes().find(|n| n.id == node).unwrap().key;
        assert_ne!(
            Some(session_key),
            full.node_key(node),
            "{what}：登録表で鍵が違う（照らす意味がある）"
        );
        assert_same_plan(&Ok(inc.clone()), &Ok(full.clone()), what);
        assert_eq!(
            plan_engine.fingerprints(&inc, &fps).unwrap(),
            plan_engine.fingerprints(&full, &fps).unwrap(),
            "{what}"
        );
        // 鍵の食い違わないノード（反転）は、セッションの鍵のまま
        assert_eq!(
            snap.nodes().find(|n| n.id == g.nodes[0].id).unwrap().key,
            inc.node_key(g.nodes[0].id).unwrap(),
            "{what}"
        );
        // 追記したあとの差分の計画（前の計画から）も同じ
        if std::ptr::eq(session_engine, &log) {
            s.commit(
                session_engine,
                Edit::AppendItems {
                    node,
                    field: "items".into(),
                    items: vec![json!(3), json!(4)],
                },
                CommitMode::Step,
            )
            .unwrap();
            let snap2 = s.current().clone();
            let full2 = plan_engine.compile(&snap2.to_graph(), &shapes);
            let inc2 = plan_engine.compile_snapshot(&snap2, &shapes, Some(&inc));
            assert_same_plan(&inc2, &full2, what);
            assert_eq!(
                plan_engine.fingerprints(&inc2.unwrap(), &fps).unwrap(),
                plan_engine.fingerprints(&full2.unwrap(), &fps).unwrap(),
                "{what}"
            );
        }
    }
}
