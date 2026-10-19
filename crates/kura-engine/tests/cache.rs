//! ノードのキャッシュ（docs/editing.md §12、§17 の E1・E4・E24・E26、NFR-16・NFR-17）。
//!
//! キャッシュの道（`Engine::render_cached`）が、キャッシュなしの正（`Engine::render`）と同じ値・警告を出すこと、指紋が値を
//! 変えるものをすべて含み、変えないもの（ノード ID・JSON の書き方）を含まないこと、数え方・固定・追い出し・エンジンへの縛り。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::doc::{Graph, InputId, InputRole, InputSlot, NodeId, PortRef, Quantize};
use kura_engine::geom::IPoint;
use kura_engine::mapping::{SpaceMap, TimeMap};
use kura_engine::monitor::CancelAfter;
use kura_engine::op::{
    Applied, ApplyCx, DynExample, DynOp, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op,
    OpParams, ParamKind, ParamSchema, PortSpec, Registry, Signature, Unit, spec,
};
use kura_engine::paint::{Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool};
use kura_engine::render::cache::{
    CachedRender, Fingerprint, FingerprintedInput, NodeCache, Origin, RenderOptions,
    node_fingerprint,
};
use kura_engine::render::{Engine, InputValue, Plan, Rendered};
use kura_engine::shape::{Shape, ValueKind};
use kura_engine::testgen::InputGen;
use kura_engine::testing::{single_node_graph, source_of};
use kura_engine::value::{Image, ImageMeta, PixelFormat, Raster, Value};
use kura_engine::{Digest, EngineError, Issues, Limits, NoMonitor};
use serde::{Deserialize, Serialize};
use serde_json::json;

fn engine() -> Engine {
    Engine::new().unwrap()
}

fn shapes_of(inputs: &BTreeMap<InputId, FingerprintedInput>) -> BTreeMap<InputId, Shape> {
    inputs.iter().map(|(k, v)| (*k, v.value.shape())).collect()
}

fn one_input(value: Value, source: Digest) -> BTreeMap<InputId, FingerprintedInput> {
    BTreeMap::from([(InputId(0), FingerprintedInput::new(value, source).unwrap())])
}

/// キャッシュなしの正。
fn plain(
    engine: &Engine,
    plan: &Plan,
    inputs: &BTreeMap<InputId, FingerprintedInput>,
) -> Result<Rendered, EngineError> {
    let values = inputs
        .iter()
        .map(|(k, v)| {
            (
                *k,
                InputValue {
                    value: v.value.clone(),
                    source: v.source,
                },
            )
        })
        .collect();
    engine.render(plan, &values, &NoMonitor)
}

fn cached(
    engine: &Engine,
    plan: &Plan,
    inputs: &BTreeMap<InputId, FingerprintedInput>,
    cache: &mut NodeCache,
    verify: bool,
) -> Result<CachedRender, EngineError> {
    engine.render_cached(
        plan,
        inputs,
        cache,
        &RenderOptions {
            digests: true,
            focus: None,
            verify_hits: verify,
        },
        &NoMonitor,
    )
}

/// 出力のダイジェストと警告が、キャッシュなしの正と同じ。
fn assert_same(want: &Rendered, got: &CachedRender, what: &str) {
    let want_d: Vec<Digest> = want.outputs.iter().map(|o| o.digest).collect();
    let got_d: Vec<Digest> = got.outputs.iter().map(|o| o.digest.unwrap()).collect();
    assert_eq!(got_d, want_d, "{what}: ダイジェスト");
    assert_eq!(got.warnings, want.warnings, "{what}: 警告");
    // 値も同じ（ダイジェストは値から取り直したもの）
    for (w, g) in want.outputs.iter().zip(&got.outputs) {
        assert_eq!(g.value.digest().unwrap(), w.digest, "{what}: 値");
    }
}

/// 1・3・16 スレッドのプール（スレッド数と組で）。
fn pools() -> Vec<(usize, rayon::ThreadPool)> {
    [1, 3, 16]
        .into_iter()
        .map(|n| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .unwrap();
            (n, pool)
        })
        .collect()
}

fn each_example(mut f: impl FnMut(&Engine, &Arc<dyn DynOp>, &DynExample)) {
    let engine = engine();
    for op in engine.registry().all() {
        for ex in &op.examples().unwrap() {
            f(&engine, &op, ex);
        }
    }
}

/// E1・E4・NFR-16：登録されたすべての操作の例で、キャッシュなし・冷えた・温まった・上限 0・小さい上限の 5 通りの値と警告
/// （順も）が同じ。温まった描画は必ず当たる。温まったものは当たりを描き直して照らす（指紋の抜けを見つける）。
#[test]
fn every_example_is_the_same_with_and_without_the_cache() {
    let mut checked = 0;
    let pools = pools();
    each_example(|engine, op, ex| {
        let what = format!("{} v{} {}", op.id(), op.version(), ex.name);
        let input = ex.inputs[0].generate().unwrap();
        let inputs = one_input(input, source_of(&ex.inputs).unwrap());
        let g = single_node_graph(op.id(), op.version(), ex.params.clone()).unwrap();
        let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
        let want = plain(engine, &plan, &inputs).unwrap_or_else(|e| panic!("{what}: {e}"));
        let mut cache = NodeCache::new(1 << 30);
        let cold = cached(engine, &plan, &inputs, &mut cache, false).unwrap();
        assert_same(&want, &cold, &format!("{what}（冷えた）"));
        assert_eq!((cold.stats.hits, cold.stats.misses), (0, 1), "{what}");
        let warm = cached(engine, &plan, &inputs, &mut cache, true).unwrap();
        assert_same(&want, &warm, &format!("{what}（温まった）"));
        assert_eq!((warm.stats.hits, warm.stats.misses), (1, 0), "{what}");
        assert_eq!(warm.stats.digest_hits, 1, "{what}");
        for capacity in [0u64, 4 << 10] {
            let mut small = NodeCache::new(capacity);
            for round in 0..2 {
                let r = cached(engine, &plan, &inputs, &mut small, round == 1).unwrap();
                assert_same(
                    &want,
                    &r,
                    &format!("{what}（上限 {capacity}・{round} 回目）"),
                );
            }
            assert!(small.bytes() <= capacity, "{what}");
        }
        // 1・3・16 スレッドのプールの中でも、冷えた・温まったが同じ
        for (threads, pool) in &pools {
            let mut c = NodeCache::new(1 << 30);
            for round in 0..2 {
                let r = pool
                    .install(|| cached(engine, &plan, &inputs, &mut c, false))
                    .unwrap();
                assert_same(
                    &want,
                    &r,
                    &format!("{what}（{threads} スレッド・{round} 回目）"),
                );
            }
        }
        checked += 1;
    });
    assert!(checked > 80, "{checked}");
}

/// E1・NFR-16：スレッド数（1・3・16）で値が変わらない（キャッシュの道も、プールの中で描く。重いものだけ）。
#[test]
fn cached_renders_do_not_depend_on_threads() {
    let heavy = heavy_examples(6);
    for (op, ex) in &heavy {
        let engine = engine();
        let input = ex.inputs[0].generate().unwrap();
        let inputs = one_input(input, source_of(&ex.inputs).unwrap());
        let g = single_node_graph(op, 1.max(version_of(&engine, op)), ex.params.clone()).unwrap();
        let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
        let want = plain(&engine, &plan, &inputs).unwrap();
        for threads in [1, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let mut cache = NodeCache::new(1 << 30);
            for _ in 0..2 {
                let r = pool
                    .install(|| cached(&engine, &plan, &inputs, &mut cache, false))
                    .unwrap();
                assert_same(&want, &r, &format!("{op} {}・{threads} スレッド", ex.name));
            }
        }
    }
}

fn version_of(engine: &Engine, op: &str) -> u16 {
    engine.registry().current_version(op).unwrap()
}

/// 手間の大きい今の版の例（操作 ID と例）。
fn heavy_examples(n: usize) -> Vec<(&'static str, DynExample)> {
    let engine = engine();
    let mut all: Vec<(u64, &'static str, DynExample)> = Vec::new();
    for op in engine.registry().current() {
        for ex in op.examples().unwrap() {
            let input = ex.inputs[0].generate().unwrap();
            let g = single_node_graph(op.id(), op.version(), ex.params.clone()).unwrap();
            let plan = engine
                .compile(&g, &BTreeMap::from([(InputId(0), input.shape())]))
                .unwrap();
            all.push((plan.cost(), op.id(), ex));
        }
    }
    all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
    all.into_iter()
        .take(n)
        .map(|(_, id, ex)| (id, ex))
        .collect()
}

/// E1・NFR-16：大きさの違う 2 つのプール（4 と 12 スレッド）で、別々のキャッシュを持って同時に描いても同じ値になる。
#[test]
#[allow(clippy::disallowed_methods)]
fn two_pools_render_the_same() {
    let heavy = heavy_examples(4);
    let engine = engine();
    let cases: Vec<(Plan, BTreeMap<InputId, FingerprintedInput>, Rendered)> = heavy
        .iter()
        .map(|(op, ex)| {
            let input = ex.inputs[0].generate().unwrap();
            let inputs = one_input(input, source_of(&ex.inputs).unwrap());
            let g = single_node_graph(op, version_of(&engine, op), ex.params.clone()).unwrap();
            let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
            let want = plain(&engine, &plan, &inputs).unwrap();
            (plan, inputs, want)
        })
        .collect();
    let run = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let mut cache = NodeCache::new(1 << 30);
        let mut out = Vec::new();
        for _ in 0..2 {
            for (plan, inputs, _) in &cases {
                out.push(
                    pool.install(|| cached(&engine, plan, inputs, &mut cache, false))
                        .unwrap(),
                );
            }
        }
        out
    };
    let (a, b) = std::thread::scope(|s| {
        let a = s.spawn(|| run(4));
        let b = s.spawn(|| run(12));
        (a.join().unwrap(), b.join().unwrap())
    });
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        let want = &cases[i % cases.len()].2;
        assert_same(want, x, &format!("4 スレッド {i}"));
        assert_same(want, y, &format!("12 スレッド {i}"));
    }
}

// ---- 試験用の操作（版の違い・入力の順） ----

const IMAGE_IN: &[PortSpec] = &[PortSpec {
    name: "image",
    accepts: &[ValueKind::Image],
}];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddParams {
    add: i64,
}

impl OpParams for AddParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "add",
                ParamKind::Int {
                    min: 0,
                    max: 255,
                    step: 1,
                },
                Unit::None,
                json!(0),
            )],
        }
    }
}

fn add_to_image(input: &Value, add: u8, cx: &ApplyCx<'_>) -> Result<Applied, EngineError> {
    cx.check()?;
    let Value::Image(img) = input else {
        return Err(EngineError::Internal("画像ではありません".into()));
    };
    let px = img
        .px
        .derive(img.px.width(), img.px.height(), cx.mon, |r, out| {
            img.px.read_rect(r, out)?;
            for b in out.iter_mut() {
                *b = b.wrapping_add(add);
            }
            Ok(())
        })?;
    Ok(Applied::value(Value::Image(Arc::new(Image {
        px,
        meta: img.meta.clone(),
    }))))
}

fn same_shape(inputs: &[Shape]) -> Result<Inferred, Issues> {
    Ok(Inferred {
        shape: inputs[0],
        space: SpaceMap::Identity,
        time: TimeMap::Identity,
        cost: 1,
    })
}

/// `test.add` v1：画素に `add` を足す（旧版）。
struct AddV1;
/// `test.add` v2：画素に `add` の 2 倍を足す（今の版）。
struct AddV2;

impl Op for AddV1 {
    const ID: &'static str = "test.add";
    const VERSION: u16 = 1;
    type Params = AddParams;
    fn signature() -> Signature {
        Signature {
            inputs: IMAGE_IN,
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }
    fn infer(_: &AddParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        p: &AddParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        add_to_image(inputs[0], p.add as u8, cx)
    }
    fn examples() -> Vec<Example<AddParams>> {
        Vec::new()
    }
}

impl Op for AddV2 {
    const ID: &'static str = "test.add";
    const VERSION: u16 = 2;
    type Params = AddParams;
    fn signature() -> Signature {
        AddV1::signature()
    }
    fn infer(_: &AddParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        p: &AddParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        add_to_image(inputs[0], (p.add as u8).wrapping_mul(2), cx)
    }
    fn examples() -> Vec<Example<AddParams>> {
        Vec::new()
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

/// `test.first`：入力 2 つのうち 1 つ目を返す（入力の順が指紋に入ることの確かめ）。
struct First;

impl Op for First {
    const ID: &'static str = "test.first";
    const VERSION: u16 = 1;
    type Params = NoParams;
    fn signature() -> Signature {
        Signature {
            inputs: &[
                PortSpec {
                    name: "a",
                    accepts: &[ValueKind::Image],
                },
                PortSpec {
                    name: "b",
                    accepts: &[ValueKind::Image],
                },
            ],
            lift: Lift::None,
            numeric: Numeric::Exact,
            meta: MetaAccess::Map,
        }
    }
    fn infer(_: &NoParams, inputs: &[Shape], _: &InferCx<'_>) -> Result<Inferred, Issues> {
        same_shape(inputs)
    }
    fn apply(
        _: &NoParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        add_to_image(inputs[0], 0, cx)
    }
    fn examples() -> Vec<Example<NoParams>> {
        Vec::new()
    }
}

fn test_engine() -> Engine {
    let mut r = Registry::empty();
    r.add_legacy::<AddV1>().unwrap();
    r.add_current::<AddV2>().unwrap();
    r.add_current::<First>().unwrap();
    Engine::with_registry(r, Limits::default())
}

fn gray(w: u32, h: u32, v: u8) -> Value {
    Value::Image(Arc::new(Image {
        px: Raster::filled(w, h, PixelFormat::Gray8, None, &[v]).unwrap(),
        meta: ImageMeta::default(),
    }))
}

/// 1 つ目のノードの指紋（と計画）。
fn first_fp(
    engine: &Engine,
    g: &Graph,
    inputs: &BTreeMap<InputId, FingerprintedInput>,
) -> Fingerprint {
    let plan = engine.compile(g, &shapes_of(inputs)).unwrap();
    let fps = engine.fingerprints(&plan, inputs).unwrap();
    fps.nodes[&g.nodes[0].id]
}

/// E4・E26：指紋は、種・元・有効か・引数・実装の版・入力（値と順）・規則・段のどれを変えても変わる。ノード ID（継ぎ足しで
/// ずれるもの）と JSON の書き方（空白・キーの順）では変わらない。外したノードの指紋は入力 0 の指紋。
#[test]
fn fingerprint_components() {
    let engine = engine();
    let src = Digest([7; 32]);
    let inputs = one_input(gray(300, 20, 5), src);
    let flip = |axis: &str, seed: u64| {
        let mut g = Graph::new_single();
        g.push_linear("image.flip", 1, json!({ "axis": axis }), seed)
            .unwrap();
        g
    };
    let base = first_fp(&engine, &flip("horizontal", 5), &inputs);
    assert_eq!(base, first_fp(&engine, &flip("horizontal", 5), &inputs));
    // 種・引数
    assert_ne!(base, first_fp(&engine, &flip("horizontal", 6), &inputs));
    assert_ne!(base, first_fp(&engine, &flip("vertical", 5), &inputs));
    // 元（派生した種に入る）・入力の値
    assert_ne!(
        base,
        first_fp(
            &engine,
            &flip("horizontal", 5),
            &one_input(gray(300, 20, 5), Digest([8; 32]))
        )
    );
    assert_ne!(
        base,
        first_fp(
            &engine,
            &flip("horizontal", 5),
            &one_input(gray(300, 20, 6), src)
        )
    );
    // 外したノードは入力 0 の指紋
    let mut off = flip("horizontal", 5);
    off.nodes[0].enabled = false;
    let off_fp = first_fp(&engine, &off, &inputs);
    assert_ne!(base, off_fp);
    assert_eq!(off_fp, inputs[&InputId(0)].fingerprint);
    // ノード ID を変えても同じ
    let mut moved = flip("horizontal", 5);
    moved.nodes[0].id = NodeId(41);
    moved.outputs[0].from = PortRef::Node { id: NodeId(41) };
    moved.next_id = 42;
    assert_eq!(base, first_fp(&engine, &moved, &inputs));
    // 継ぎ足し（compose）で ID がずれても、同じ位置の同じノードは同じ
    let mut recipe = Graph::new_single();
    recipe
        .push_linear("image.flip", 1, json!({"axis": "vertical"}), 9)
        .unwrap();
    let composed = kura_engine::doc::compose(&flip("horizontal", 5), &recipe).unwrap();
    assert_ne!(composed.nodes[1].id, recipe.nodes[0].id);
    let plan = engine.compile(&composed, &shapes_of(&inputs)).unwrap();
    let fps = engine.fingerprints(&plan, &inputs).unwrap();
    assert_eq!(fps.nodes[&composed.nodes[0].id], base);
    let mut alone = flip("horizontal", 5);
    alone
        .push_linear("image.flip", 1, json!({"axis": "vertical"}), 9)
        .unwrap();
    let plan2 = engine.compile(&alone, &shapes_of(&inputs)).unwrap();
    assert_eq!(
        fps.nodes[&composed.nodes[1].id],
        engine.fingerprints(&plan2, &inputs).unwrap().nodes[&alone.nodes[1].id]
    );
    // JSON の書き方（空白・キーの順）では変わらない
    let raw = String::from_utf8(flip("horizontal", 5).to_canonical_json().unwrap()).unwrap();
    let spaced = raw.replace(',', " ,\n ").replace(':', " : ");
    let loaded = engine.load(spaced.as_bytes()).unwrap();
    assert_eq!(base, first_fp(&engine, &loaded, &inputs));
    let mut g = flip("horizontal", 5);
    g.nodes[0].params = serde_json::from_str(r#"{ "axis" : "horizontal" }"#).unwrap();
    assert_eq!(base, first_fp(&engine, &g, &inputs));

    // 実装の版（旧版 v1 と今の版 v2）
    let te = test_engine();
    let add = |v: u16| {
        let mut g = Graph::new_single();
        g.push_linear("test.add", v, json!({"add": 3}), 0).unwrap();
        g
    };
    let v1 = first_fp(&te, &add(1), &inputs);
    let v2 = first_fp(&te, &add(2), &inputs);
    assert_ne!(v1, v2);
    // 版で値も違う（指紋が違わなければ、旧版の値が今の版に当たってしまう）
    let p1 = te.compile(&add(1), &shapes_of(&inputs)).unwrap();
    let p2 = te.compile(&add(2), &shapes_of(&inputs)).unwrap();
    assert_ne!(
        plain(&te, &p1, &inputs).unwrap().outputs[0].digest,
        plain(&te, &p2, &inputs).unwrap().outputs[0].digest
    );
    // 同じ版を同じ値で描けば当たり、版を変えれば外れる
    let mut cache = NodeCache::new(1 << 30);
    cached(&te, &p1, &inputs, &mut cache, false).unwrap();
    assert_eq!(
        cached(&te, &p1, &inputs, &mut cache, true)
            .unwrap()
            .stats
            .hits,
        1
    );
    let r2 = cached(&te, &p2, &inputs, &mut cache, true).unwrap();
    assert_eq!(r2.stats.hits, 0);
    assert_same(&plain(&te, &p2, &inputs).unwrap(), &r2, "v2");

    // 入力の順
    let mut two = Graph::new_single();
    two.inputs.push(InputSlot {
        id: InputId(1),
        role: InputRole::Fixed {
            hash: Digest([1; 32]),
        },
        decode: Default::default(),
    });
    two.push_linear("test.first", 1, json!({}), 0).unwrap();
    two.nodes[0].inputs = vec![
        PortRef::Input { id: InputId(0) },
        PortRef::Input { id: InputId(1) },
    ];
    let both = BTreeMap::from([
        (
            InputId(0),
            FingerprintedInput::new(gray(30, 20, 1), src).unwrap(),
        ),
        (
            InputId(1),
            FingerprintedInput::new(gray(30, 20, 2), Digest([1; 32])).unwrap(),
        ),
    ]);
    let ab = first_fp(&te, &two, &both);
    let mut swapped = two.clone();
    swapped.nodes[0].inputs.reverse();
    assert_ne!(ab, first_fp(&te, &swapped, &both));

    // 規則・段（式の要素。計画は今の規則・等倍しか作らないので式で確かめる）
    let key = Digest([3; 32]);
    let ins = [Fingerprint(Digest([4; 32])), Fingerprint(Digest([5; 32]))];
    let f = node_fingerprint(&key, 1, 9, 0, &ins);
    assert_ne!(f, node_fingerprint(&key, 2, 9, 0, &ins));
    assert_ne!(f, node_fingerprint(&key, 1, 9, 1, &ins));
    assert_ne!(f, node_fingerprint(&key, 1, 8, 0, &ins));
    assert_ne!(f, node_fingerprint(&Digest([6; 32]), 1, 9, 0, &ins));
    assert_ne!(f, node_fingerprint(&key, 1, 9, 0, &[ins[1], ins[0]]));
    assert_ne!(f, node_fingerprint(&key, 1, 9, 0, &ins[..1]));
    assert_eq!(plan.level(), 0);
}

/// 量子化した出力の指紋は、量子化の引数と元で変わる（量子化の値はキャッシュで当たっても同じ）。
#[test]
fn quantized_outputs_have_their_own_fingerprint() {
    let engine = engine();
    let input_gen = InputGen::AudioNoise {
        rate: 44_100,
        channels: 2,
        frames: 4_000,
        seed: 5,
    };
    let input = input_gen.generate().unwrap();
    let doc = |seed: u64| {
        let mut g = Graph::new_single();
        g.push_linear("audio.gain", 1, json!({"db": -3.0}), 0)
            .unwrap();
        g.outputs[0].quantize = Some(Quantize {
            v: kura_engine::doc::QUANTIZE_VERSION,
            bits: 16,
            dither: kura_engine::doc::Dither::Tpdf,
            seed,
        });
        g
    };
    let inputs = one_input(input.clone(), Digest([2; 32]));
    let mut cache = NodeCache::new(1 << 30);
    let mut outs = Vec::new();
    for seed in [1, 2] {
        let plan = engine.compile(&doc(seed), &shapes_of(&inputs)).unwrap();
        let want = plain(&engine, &plan, &inputs).unwrap();
        for _ in 0..2 {
            let r = cached(&engine, &plan, &inputs, &mut cache, true).unwrap();
            assert_same(&want, &r, &format!("量子化の種 {seed}"));
        }
        let fps = engine.fingerprints(&plan, &inputs).unwrap();
        assert_ne!(fps.outputs[0].1, fps.nodes[&NodeId(1)]);
        outs.push(fps.outputs[0].1);
    }
    assert_ne!(outs[0], outs[1]);
    let other = one_input(input, Digest([3; 32]));
    let plan = engine.compile(&doc(1), &shapes_of(&other)).unwrap();
    assert_ne!(
        engine.fingerprints(&plan, &other).unwrap().outputs[0].1,
        outs[0]
    );
}

/// E4：警告を出す例は、当たっても同じ警告を同じ順に出す。警告を出すノードが当たり、後ろのノードが外れる鎖でも同じ。
#[test]
fn warnings_are_replayed_on_hits() {
    let mut with_warnings = 0;
    each_example(|engine, op, ex| {
        let input = ex.inputs[0].generate().unwrap();
        let inputs = one_input(input, source_of(&ex.inputs).unwrap());
        let g = single_node_graph(op.id(), op.version(), ex.params.clone()).unwrap();
        let shapes = shapes_of(&inputs);
        let plan = engine.compile(&g, &shapes).unwrap();
        let want = plain(engine, &plan, &inputs).unwrap();
        if want.warnings.is_empty() {
            return;
        }
        with_warnings += 1;
        let what = format!("{} {}", op.id(), ex.name);
        let mut cache = NodeCache::new(1 << 30);
        cached(engine, &plan, &inputs, &mut cache, false).unwrap();
        let warm = cached(engine, &plan, &inputs, &mut cache, false).unwrap();
        assert_eq!(warm.stats.hits, 1, "{what}");
        assert_same(&want, &warm, &what);
        // 後ろに外れるノードを足す：前のノードは当たって警告を再生し、後ろは描く
        let Some(tail) = tail_for(&plan.output_shapes()[0].1) else {
            return;
        };
        for (i, params) in tail.1.iter().enumerate() {
            let mut g2 = g.clone();
            g2.push_linear(tail.0, 1, params.clone(), 0).unwrap();
            let Ok(plan2) = engine.compile(&g2, &shapes) else {
                continue;
            };
            let want2 = plain(engine, &plan2, &inputs).unwrap();
            let r = cached(engine, &plan2, &inputs, &mut cache, false).unwrap();
            assert_eq!((r.stats.hits, r.stats.misses), (1, 1), "{what} {i}");
            assert_same(&want2, &r, &format!("{what}（後ろに {} {i}）", tail.0));
        }
    });
    assert!(with_warnings >= 3, "{with_warnings}");
}

/// 値の種類ごとに、後ろに足せる操作と引数（2 通り）。
fn tail_for(shape: &Shape) -> Option<(&'static str, Vec<serde_json::Value>)> {
    match shape {
        Shape::Image { .. } | Shape::Frames { .. } => Some((
            "image.flip",
            vec![json!({"axis": "horizontal"}), json!({"axis": "vertical"})],
        )),
        Shape::Audio { .. } => Some(("audio.gain", vec![json!({"db": -1.5}), json!({"db": 2.0})])),
    }
}

/// E24：途中（k 回目の確かめ）で止めると Canceled で、出力は出ない。そのあと、残った項目は描き直したものと一致し
/// （verify_hits）、次の描画はキャッシュなしと同じ。鎖は例のノードの後ろに 1 つ足したもの（前段の値が残りうる）。
#[test]
fn cancel_keeps_the_cache_consistent() {
    each_example(|engine, op, ex| {
        let what = format!("{} v{} {}", op.id(), op.version(), ex.name);
        let input = ex.inputs[0].generate().unwrap();
        let inputs = one_input(input, source_of(&ex.inputs).unwrap());
        let shapes = shapes_of(&inputs);
        let mut g = single_node_graph(op.id(), op.version(), ex.params.clone()).unwrap();
        let plan = engine.compile(&g, &shapes).unwrap();
        if let Some((tail, params)) = tail_for(&plan.output_shapes()[0].1) {
            let mut g2 = g.clone();
            g2.push_linear(tail, 1, params[0].clone(), 0).unwrap();
            if engine.compile(&g2, &shapes).is_ok() {
                g = g2;
            }
        }
        let plan = engine.compile(&g, &shapes).unwrap();
        let want = plain(engine, &plan, &inputs).unwrap();
        let opts = RenderOptions {
            digests: true,
            ..RenderOptions::default()
        };
        let counter = CancelAfter::new(0);
        engine
            .render_cached(
                &plan,
                &inputs,
                &mut NodeCache::new(1 << 30),
                &opts,
                &counter,
            )
            .unwrap();
        let calls = counter.calls();
        let ks: Vec<u64> = [1, 2, calls / 2, calls.saturating_sub(1), calls]
            .into_iter()
            .filter(|k| (1..=calls).contains(k))
            .collect();
        for k in ks {
            let mut cache = NodeCache::new(1 << 30);
            let r = engine.render_cached(&plan, &inputs, &mut cache, &opts, &CancelAfter::new(k));
            assert!(matches!(r, Err(EngineError::Canceled)), "{what}: {k} 回目");
            let again = cached(engine, &plan, &inputs, &mut cache, true)
                .unwrap_or_else(|e| panic!("{what}: {k} 回目のあと: {e}"));
            assert_same(&want, &again, &format!("{what}（{k} 回目で止めたあと）"));
        }
    });
}

/// 指紋の抜けを見つける口：わざと違う値を入れておくと、verify_hits が `Internal` を返す（照らさなければ、その値が当たる）。
/// 由来が `Exact` でない項目は当たりに使わない。
#[test]
fn verify_hits_detects_a_wrong_entry() {
    let engine = engine();
    let inputs = one_input(gray(300, 20, 5), Digest::default());
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({"axis": "horizontal"}), 0)
        .unwrap();
    let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
    let want = plain(&engine, &plan, &inputs).unwrap();
    let fp = engine.fingerprints(&plan, &inputs).unwrap().nodes[&NodeId(1)];
    let wrong = gray(300, 20, 6);
    // 由来が継ぎはぎ・部分の値は当たりに使わない
    for origin in [Origin::Patched, Origin::Partial] {
        let mut cache = NodeCache::new(1 << 30);
        assert!(cache.insert_raw(fp, wrong.clone(), origin));
        let r = cached(&engine, &plan, &inputs, &mut cache, false).unwrap();
        assert_eq!(r.stats.hits, 0);
        assert_same(&want, &r, "Exact でない項目");
    }
    let mut cache = NodeCache::new(1 << 30);
    cache.insert_raw(fp, wrong.clone(), Origin::Exact);
    let r = cached(&engine, &plan, &inputs, &mut cache, false).unwrap();
    assert_eq!(r.stats.hits, 1);
    assert_eq!(r.outputs[0].digest, Some(wrong.digest().unwrap()));
    match cached(&engine, &plan, &inputs, &mut cache, true) {
        Err(EngineError::Internal(m)) => assert!(m.contains("描き直した"), "{m}"),
        other => panic!("{other:?}"),
    }
}

/// 安全：1 つのキャッシュは 1 つのエンジンでだけ使う。別のエンジンの計画も断る。
#[test]
fn a_cache_is_bound_to_one_engine() {
    let a = engine();
    let b = engine();
    let inputs = one_input(gray(30, 20, 5), Digest::default());
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({"axis": "horizontal"}), 0)
        .unwrap();
    let plan_a = a.compile(&g, &shapes_of(&inputs)).unwrap();
    let plan_b = b.compile(&g, &shapes_of(&inputs)).unwrap();
    let mut cache = NodeCache::new(1 << 30);
    cached(&a, &plan_a, &inputs, &mut cache, false).unwrap();
    assert!(matches!(
        cached(&b, &plan_b, &inputs, &mut cache, false),
        Err(EngineError::Internal(_))
    ));
    // 計画を作ったエンジンでないと描けない（新しいキャッシュでも）
    assert!(matches!(
        cached(&b, &plan_a, &inputs, &mut NodeCache::new(1 << 30), false),
        Err(EngineError::Internal(_))
    ));
    // 捨てれば別のエンジンで使える
    cache.clear();
    assert!(cached(&b, &plan_b, &inputs, &mut cache, false).is_ok());
    // キャッシュなしの道は計画を縛らない（値も型も変えない）
    assert!(plain(&b, &plan_a, &inputs).is_ok());
}

fn stroke(x: i64, y: i64) -> serde_json::Value {
    serde_json::to_value(PaintAction::Stroke {
        tool: Tool::Pen,
        color: Some(PaintColor::Rgba([200, 10, 10, 255])),
        blend: Blend::Replace,
        brush: Brush {
            shape: BrushShape::Square,
            size: 3,
        },
        pixel_perfect: false,
        frame: 0,
        points: PointList(vec![IPoint { x, y }, IPoint { x: x + 5, y: y + 2 }]),
    })
    .unwrap()
}

/// NFR-17：共有した実体は 1 度だけ数える（同じ値を 100 回入れても 1 つ分、メタデータだけ変える操作は入力と共有、
/// 筆の値は触れたタイルの分だけ）。上限を超えたら、固定していないものを使った順に追い出し、固定（focus の入力と出力）は残る。
#[test]
fn shared_tiles_are_counted_once_and_pins_survive() {
    let engine = engine();
    let tile = 256 * 256 * 4;
    let rgba = |v: u8| {
        Value::Image(Arc::new(Image {
            px: Raster::filled(1024, 1024, PixelFormat::Rgba8, None, &[v, v, v, 255]).unwrap(),
            meta: ImageMeta::default(),
        }))
    };
    let input = rgba(9);
    let one = 16 * tile;
    let mut cache = NodeCache::new(1 << 30);
    for i in 0..100u8 {
        cache.insert_raw(Fingerprint(Digest([i; 32])), input.clone(), Origin::Exact);
    }
    assert_eq!(cache.bytes(), one);
    // メタデータだけ変える操作の出力は入力とタイルを共有する（入力を外の値として数える）
    let mut cache = NodeCache::new(1 << 30);
    assert_eq!(cache.register_external(&input), one);
    let inputs = one_input(input.clone(), Digest::default());
    for i in 0..20 {
        let mut g = Graph::new_single();
        g.push_linear("meta.set_pivot", 1, json!({"pivot": {"x": i, "y": 3}}), 0)
            .unwrap();
        let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
        cached(&engine, &plan, &inputs, &mut cache, false).unwrap();
    }
    assert_eq!(cache.len(), 20);
    assert_eq!(cache.bytes(), one);
    // 筆：触れたタイルの分だけ増える（1 本目は 1 タイル。2 本目を足した値は、1 本目の値に差分の適用で描くので、1 本目の
    // タイルを共有して 1 タイルだけ増える）
    let paint = |actions: Vec<serde_json::Value>| {
        let mut g = Graph::new_single();
        g.push_linear("paint.actions", 1, json!({ "actions": actions }), 0)
            .unwrap();
        engine.compile(&g, &shapes_of(&inputs)).unwrap()
    };
    let before = cache.bytes();
    cached(
        &engine,
        &paint(vec![stroke(10, 10)]),
        &inputs,
        &mut cache,
        false,
    )
    .unwrap();
    assert_eq!(cache.bytes(), before + tile);
    let second = cached(
        &engine,
        &paint(vec![stroke(10, 10), stroke(600, 600)]),
        &inputs,
        &mut cache,
        false,
    )
    .unwrap();
    assert_eq!(second.stats.appended, 1);
    assert_eq!(cache.bytes(), before + 2 * tile);

    // 追い出しと固定：反転 5 つの鎖（各ノードの値は 1 つ分）に、上限 3 つ分。focus を 2 番目にする
    let mut cache = NodeCache::new(3 * one);
    let mut g = Graph::new_single();
    for i in 0..5 {
        let axis = if i % 2 == 0 { "horizontal" } else { "vertical" };
        g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
            .unwrap();
    }
    let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
    let fps = engine.fingerprints(&plan, &inputs).unwrap();
    let fp = |i: usize| fps.nodes[&g.nodes[i].id];
    let want = plain(&engine, &plan, &inputs).unwrap();
    let focus = RenderOptions {
        digests: true,
        focus: Some(g.nodes[1].id),
        verify_hits: false,
    };
    let r = engine
        .render_cached(&plan, &inputs, &mut cache, &focus, &NoMonitor)
        .unwrap();
    assert_same(&want, &r, "focus つき");
    // 1・2 番目（focus の入力と出力）は固定で残り、3・4 番目は古い順に追い出され、5 番目（いちばん新しい）が残る
    let held: Vec<bool> = (0..5).map(|i| cache.contains(&fp(i))).collect();
    assert_eq!(held, vec![true, true, false, false, true]);
    assert_eq!(cache.pins().len(), 2);
    assert_eq!(cache.pinned_bytes(), 2 * one);
    assert!(cache.bytes() <= 3 * one);
    // 上限を下げても固定は残る
    assert_eq!(cache.shrink_to(0), 2 * one);
    assert!(cache.contains(&fp(0)) && cache.contains(&fp(1)) && !cache.contains(&fp(4)));
    // 次の描画は当たり 2・外れ 3 で、値はキャッシュなしと同じ
    let r = cached(&engine, &plan, &inputs, &mut cache, true).unwrap();
    assert_eq!((r.stats.hits, r.stats.misses), (2, 3));
    assert_same(&want, &r, "固定のあと");
    // focus を外すと固定もなくなる
    assert!(cache.pins().is_empty());
}

/// E26・NFR-11：normalize の目標だけを変えると、2 回目は解析を解かない（同じ入力の指紋・ID・版・引数の鍵で当たる）。
/// 値はキャッシュなしと同じ。
#[test]
fn analyses_are_reused_when_only_params_change() {
    let engine = engine();
    let input_gen = InputGen::Sine {
        rate: 48_000,
        channels: 2,
        frames: 48_000,
        freq: 1_000,
        loop_range: None,
    };
    let inputs = one_input(input_gen.generate().unwrap(), Digest::default());
    let doc = |target: f64| {
        let mut g = Graph::new_single();
        g.push_linear(
            "audio.normalize",
            1,
            json!({"target": target, "peakLimit": null}),
            0,
        )
        .unwrap();
        g
    };
    let mut cache = NodeCache::new(1 << 30);
    let mut solved = Vec::new();
    for target in [-16.0, -20.0, -16.0] {
        let plan = engine.compile(&doc(target), &shapes_of(&inputs)).unwrap();
        assert!(!plan.node_needs(NodeId(1)).unwrap().is_empty());
        let want = plain(&engine, &plan, &inputs).unwrap();
        let r = cached(&engine, &plan, &inputs, &mut cache, false).unwrap();
        assert_same(&want, &r, &format!("目標 {target}"));
        solved.push((r.stats.analysis_solved, r.stats.analysis_hits, r.stats.hits));
    }
    // 1 回目は解く。2 回目（目標だけ変えた）は解析が当たってノードは描く。3 回目はノードごと当たる
    assert_eq!(solved, vec![(1, 0, 0), (0, 1, 0), (0, 0, 1)]);
    // verify_hits なら当たった解析も解き直して照らす
    let plan = engine.compile(&doc(-18.0), &shapes_of(&inputs)).unwrap();
    let r = cached(&engine, &plan, &inputs, &mut cache, true).unwrap();
    assert_eq!((r.stats.analysis_hits, r.stats.verified), (1, 1));
    // 入力が変われば解析も外れる
    let other = one_input(
        InputGen::Sine {
            rate: 48_000,
            channels: 2,
            frames: 48_000,
            freq: 500,
            loop_range: None,
        }
        .generate()
        .unwrap(),
        Digest::default(),
    );
    let plan = engine.compile(&doc(-16.0), &shapes_of(&other)).unwrap();
    let r = cached(&engine, &plan, &other, &mut cache, false).unwrap();
    assert_eq!((r.stats.analysis_solved, r.stats.analysis_hits), (1, 0));
}

/// 出力のダイジェストを取らない描画（プレビュー）は、ダイジェストを返さない。取る描画は出力の指紋ごとに覚えて使い回す。
#[test]
fn digests_are_optional_and_remembered() {
    let engine = engine();
    let inputs = one_input(gray(300, 20, 5), Digest::default());
    let mut g = Graph::new_single();
    g.push_linear("image.flip", 1, json!({"axis": "horizontal"}), 0)
        .unwrap();
    let plan = engine.compile(&g, &shapes_of(&inputs)).unwrap();
    let mut cache = NodeCache::new(1 << 30);
    let preview = engine
        .render_cached(
            &plan,
            &inputs,
            &mut cache,
            &RenderOptions::default(),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(preview.outputs[0].digest, None);
    let a = cached(&engine, &plan, &inputs, &mut cache, false).unwrap();
    assert_eq!(a.stats.digest_hits, 0);
    let b = cached(&engine, &plan, &inputs, &mut cache, false).unwrap();
    assert_eq!(b.stats.digest_hits, 1);
    assert_eq!(a.outputs[0].digest, b.outputs[0].digest);
    assert_eq!(
        a.outputs[0].fingerprint,
        engine.fingerprints(&plan, &inputs).unwrap().outputs[0].1
    );
    // 前回の描画（差分の適用に使う入れ物）：入力・出力の指紋と規則を覚える
    let last = cache.last(NodeId(1)).unwrap();
    assert_eq!(last.output, a.outputs[0].fingerprint);
    assert_eq!(last.inputs, vec![inputs[&InputId(0)].fingerprint]);
    assert_eq!((last.rules, last.level), (kura_engine::doc::RULES, 0));
    assert_eq!(last.op.id(), "image.flip");
}

/// 速さ（ふだんは回さない。release で手で回す）：
/// `cargo test --release -p kura-engine --test cache bench_cache -- --ignored --nocapture`
///
/// - 帯に詰めたダイジェスト（今の `Value::digest`）と、行をタイルごとに足す旧い足し方（4096² RGBA8・24MP RGB8）
/// - 4096² RGBA8 の反転 20 個の鎖：キャッシュなし（ダイジェスト込み）・冷えた（ダイジェストなし／あり）・温まった（同じ）・
///   最後のノードだけを変えたとき・指紋だけ
#[test]
#[ignore]
#[allow(clippy::disallowed_methods, clippy::print_stderr)]
fn bench_cache() {
    use std::time::Instant;
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let time = |n: usize, f: &mut dyn FnMut()| {
        f();
        let mut t = Vec::new();
        for _ in 0..n {
            let s = Instant::now();
            f();
            t.push(s.elapsed().as_secs_f64() * 1000.0);
        }
        median(t)
    };
    for (w, h, fmt) in [
        (4096u32, 4096u32, PixelFormat::Rgba8),
        (6000, 4000, PixelFormat::Rgb8),
    ] {
        let v = InputGen::Noise { w, h, fmt, seed: 1 }.generate().unwrap();
        let Value::Image(img) = &v else { panic!() };
        let px = &img.px;
        // 旧い足し方：行ごとに、横のタイルの分を順に足す（値は同じ）
        let old = || {
            let mut b = blake3::Hasher::new();
            let bpp = px.bpp();
            for y in 0..px.height() {
                let ty = y / 256;
                for tx in 0..px.tiles_x() {
                    let tr = px.tile_rect(tx, ty);
                    let stride = tr.w as usize * bpp;
                    let start = (y - tr.y as u32) as usize * stride;
                    b.update(&px.tile(tx, ty)[start..start + stride]);
                }
            }
            b.finalize()
        };
        let t_old = time(5, &mut || {
            std::hint::black_box(old());
        });
        let t_new = time(5, &mut || {
            std::hint::black_box(v.digest().unwrap());
        });
        eprintln!(
            "ダイジェスト {w}×{h} {fmt:?}：行ごと {t_old:.1} ms・帯 {t_new:.1} ms（値 {} MB）",
            w as usize * h as usize * fmt.bytes_per_pixel() / 1_000_000
        );
    }
    let engine = engine();
    let input = InputGen::Noise {
        w: 4096,
        h: 4096,
        fmt: PixelFormat::Rgba8,
        seed: 2,
    }
    .generate()
    .unwrap();
    let inputs = one_input(input, Digest::default());
    let chain = |last: &str| {
        let mut g = Graph::new_single();
        for i in 0..19 {
            let axis = if i % 2 == 0 { "horizontal" } else { "vertical" };
            g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
                .unwrap();
        }
        g.push_linear("image.flip", 1, json!({ "axis": last }), 0)
            .unwrap();
        engine.compile(&g, &shapes_of(&inputs)).unwrap()
    };
    let plan = chain("horizontal");
    let plan2 = chain("vertical");
    let opts = |digests| RenderOptions {
        digests,
        ..RenderOptions::default()
    };
    let t_plain = time(3, &mut || {
        plain(&engine, &plan, &inputs).unwrap();
    });
    let t_cold_nd = time(3, &mut || {
        let mut c = NodeCache::new(4 << 30);
        engine
            .render_cached(&plan, &inputs, &mut c, &opts(false), &NoMonitor)
            .unwrap();
    });
    let t_cold_d = time(3, &mut || {
        let mut c = NodeCache::new(4 << 30);
        engine
            .render_cached(&plan, &inputs, &mut c, &opts(true), &NoMonitor)
            .unwrap();
    });
    let mut warm = NodeCache::new(4 << 30);
    engine
        .render_cached(&plan, &inputs, &mut warm, &opts(true), &NoMonitor)
        .unwrap();
    let t_warm_nd = time(20, &mut || {
        let r = engine
            .render_cached(&plan, &inputs, &mut warm, &opts(false), &NoMonitor)
            .unwrap();
        assert_eq!(r.stats.misses, 0);
    });
    let t_warm_d = time(20, &mut || {
        let r = engine
            .render_cached(&plan, &inputs, &mut warm, &opts(true), &NoMonitor)
            .unwrap();
        assert_eq!(r.stats.digest_hits, 1);
    });
    // 最後のノードを毎回新しい切り抜きにする（文書を作り直して compile する手間を含む。前の 19 個は当たる）
    let mut x = 0;
    let t_last = time(10, &mut || {
        x += 1;
        let mut g = Graph::new_single();
        for i in 0..19 {
            let axis = if i % 2 == 0 { "horizontal" } else { "vertical" };
            g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
                .unwrap();
        }
        g.push_linear(
            "image.crop",
            1,
            json!({"rect": {"x": x, "y": 0, "w": 1024, "h": 1024}}),
            0,
        )
        .unwrap();
        let p = engine.compile(&g, &shapes_of(&inputs)).unwrap();
        let r = engine
            .render_cached(&p, &inputs, &mut warm, &opts(false), &NoMonitor)
            .unwrap();
        assert_eq!((r.stats.hits, r.stats.misses), (19, 1));
    });
    // 10 番目（ピボットを置く）を毎回変えて、後ろの 10 個の反転を描き直す
    let mut px = 0;
    let t_mid = time(6, &mut || {
        px += 1;
        let mut g = Graph::new_single();
        for i in 0..20 {
            if i == 9 {
                g.push_linear("meta.set_pivot", 1, json!({"pivot": {"x": px, "y": 0}}), 0)
                    .unwrap();
            } else {
                let axis = if i % 2 == 0 { "horizontal" } else { "vertical" };
                g.push_linear("image.flip", 1, json!({ "axis": axis }), 0)
                    .unwrap();
            }
        }
        let p = engine.compile(&g, &shapes_of(&inputs)).unwrap();
        let r = engine
            .render_cached(&p, &inputs, &mut warm, &opts(false), &NoMonitor)
            .unwrap();
        assert_eq!(r.stats.misses, 11);
    });
    let _ = &plan2;
    let t_fp = time(20, &mut || {
        std::hint::black_box(engine.fingerprints(&plan, &inputs).unwrap());
    });
    eprintln!(
        "4096² RGBA8 の反転 20 個：キャッシュなし（ダイジェスト込み）{t_plain:.1} ms・冷えた（なし）{t_cold_nd:.1} ms・\
         冷えた（あり）{t_cold_d:.1} ms・温まった（なし）{t_warm_nd:.3} ms・温まった（あり）{t_warm_d:.3} ms・\
         最後を新しい切り抜きに（compile 込み）{t_last:.2} ms・10 番目を変えて 11 個を描き直す {t_mid:.1} ms・\
         指紋だけ {t_fp:.3} ms"
    );
}
