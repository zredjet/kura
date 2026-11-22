//! 適合テスト（docs/editing.md §17 の E1・E8・E9・E24・E25）。登録したすべての操作に、何もしなくても当たる。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use kura_engine::doc::{Graph, InputId};
use kura_engine::geom::IRect;
use kura_engine::monitor::{CancelAfter, CancelFlag};
use kura_engine::op::{DynExample, DynOp, Lift};
use kura_engine::render::{Engine, InputValue};
use kura_engine::testgen::InputGen;
use kura_engine::testing::{render_full, single_node_graph, source_of};
use kura_engine::value::{AudioMeta, Frames, Image, ImageMeta, PixelFormat, Raster, Value};
use kura_engine::{EngineError, IssueCode, NoMonitor};
use serde_json::json;

fn engine() -> Engine {
    Engine::new().expect("登録表が作れること（同じ操作・版が 2 つない）")
}

fn each_example(mut f: impl FnMut(&Engine, &Arc<dyn DynOp>, &DynExample)) {
    let engine = engine();
    for op in engine.registry().all() {
        let examples = op.examples().unwrap();
        assert!(!examples.is_empty(), "{} に例がありません", op.id());
        let mut names = BTreeSet::new();
        for ex in &examples {
            assert!(
                names.insert(ex.name),
                "{} の例の名前が重複しています（{}）",
                op.id(),
                ex.name
            );
            f(&engine, &op, ex);
        }
    }
}

fn graph(op: &Arc<dyn DynOp>, ex: &DynExample) -> Graph {
    single_node_graph(op.id(), op.version(), ex.params.clone()).unwrap()
}

fn render(
    engine: &Engine,
    graph: &Graph,
    input: &Value,
    ex: &DynExample,
) -> Result<Value, EngineError> {
    kura_engine::testing::render_with(
        engine,
        graph,
        input.clone(),
        source_of(&ex.inputs).unwrap(),
        &NoMonitor,
    )
}

#[test]
fn params_round_trip_and_match_the_schema() {
    each_example(|_, op, ex| {
        let parsed = op
            .parse(&ex.params)
            .unwrap_or_else(|i| panic!("{} {}: {i}", op.id(), ex.name));
        assert_eq!(
            op.params_json(parsed.as_ref()).unwrap(),
            ex.params,
            "{} {}: 往復で変わった",
            op.id(),
            ex.name
        );
        let keys: BTreeSet<String> = ex
            .params
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let schema_keys: BTreeSet<String> = op
            .schema()
            .params
            .iter()
            .map(|p| p.key.to_string())
            .collect();
        assert_eq!(
            keys,
            schema_keys,
            "{}: 引数の記述の key と serde のフィールド名が違います",
            op.id()
        );
        op.parse(&op.schema().defaults())
            .unwrap_or_else(|i| panic!("{}: 既定値が読めません: {i}", op.id()));
    });
}

/// E9：形の推論が実際の出力と合う（写し方の一致は実行器が確かめる）。E1：スレッド数を変えても同じダイジェスト。
#[test]
fn inference_matches_and_threads_do_not_matter() {
    each_example(|engine, op, ex| {
        let input = ex.inputs[0].generate().unwrap();
        let g = graph(op, ex);
        let plan = engine
            .compile(&g, &BTreeMap::from([(InputId(0), input.shape())]))
            .unwrap();
        let mut digests = Vec::new();
        for threads in [1, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let out = pool
                .install(|| render(engine, &g, &input, ex))
                .unwrap_or_else(|e| panic!("{} {}: {e}", op.id(), ex.name));
            let (_, inferred) = plan.output_shapes()[0];
            assert!(
                inferred.admits(&out.shape()),
                "{} {}: 推論 {inferred:?} と実際 {:?}",
                op.id(),
                ex.name,
                out.shape()
            );
            digests.push(out.digest().unwrap());
        }
        assert!(
            digests.windows(2).all(|w| w[0] == w[1]),
            "{} {}: スレッド数で結果が変わった",
            op.id(),
            ex.name
        );
    });
}

/// E24：操作の途中でも中止できる。入口の確認だけでなく、操作の中で中止を確かめていること。
#[test]
fn cancel_works_inside_operations() {
    each_example(|engine, op, ex| {
        let input = ex.inputs[0].generate().unwrap();
        let g = graph(op, ex);
        let counter = CancelAfter::new(0);
        kura_engine::testing::render_with(engine, &g, input.clone(), Default::default(), &counter)
            .unwrap();
        let calls = counter.calls();
        assert!(
            calls >= 2,
            "{} {}: 操作の中で中止を確かめていません（{calls} 回）",
            op.id(),
            ex.name
        );
        for k in 1..=calls.min(4) {
            let r = kura_engine::testing::render_with(
                engine,
                &g,
                input.clone(),
                Default::default(),
                &CancelAfter::new(k),
            );
            assert!(
                matches!(r, Err(EngineError::Canceled)),
                "{} {}: {k} 回目で中止できない",
                op.id(),
                ex.name
            );
        }
        let flag = CancelFlag::default();
        flag.cancel();
        assert!(matches!(
            kura_engine::testing::render_with(engine, &g, input, Default::default(), &flag),
            Err(EngineError::Canceled)
        ));
    });
}

/// フレームの列に持ち上げられる操作は、各フレームに当てた結果と同じになる。メタデータも単体の画像と同じに写る。
#[test]
fn per_frame_lift_matches_single_frames() {
    each_example(|engine, op, ex| {
        if op.signature().lift != Lift::PerFrame {
            return;
        }
        let input = ex.inputs[0].generate().unwrap();
        let Value::Image(img) = &input else { return };
        let frames = Value::Frames(Arc::new(
            Frames::full(
                vec![(img.px.clone(), 40), (img.px.clone(), 60)],
                img.meta.clone(),
                0,
            )
            .unwrap(),
        ));
        let g = graph(op, ex);
        let Value::Image(single) = render(engine, &g, &input, ex).unwrap() else {
            panic!()
        };
        let Value::Frames(out) = render(engine, &g, &frames, ex).unwrap() else {
            panic!("{}: フレームの列を返さない", op.id())
        };
        assert_eq!(out.frames().len(), 2);
        assert_eq!(
            out.meta,
            single.meta,
            "{} {}: メタデータの写しが違う",
            op.id(),
            ex.name
        );
        for (f, ms) in out.frames().iter().zip([40, 60]) {
            assert_eq!(f.px, single.px, "{} {}", op.id(), ex.name);
            assert_eq!(f.duration_ms, ms);
        }
    });
}

/// E8：未知の操作・新しすぎる版は読み込み（load）でも計画（compile）でもエラーになる。落ちない。
#[test]
fn unknown_ops_and_versions_are_rejected() {
    let engine = engine();
    let input = Value::Image(Arc::new(Image {
        px: Raster::filled(4, 4, PixelFormat::Gray8, None, &[0]).unwrap(),
        meta: ImageMeta::default(),
    }));
    let shapes = BTreeMap::from([(InputId(0), input.shape())]);
    let cases = [
        (
            single_node_graph("image.nonexistent", 1, json!({})).unwrap(),
            IssueCode::UnknownOp,
            true,
        ),
        (
            single_node_graph("image.flip", 999, json!({ "axis": "horizontal" })).unwrap(),
            IssueCode::UnknownVersion,
            true,
        ),
        (
            single_node_graph("image.flip", 0, json!({ "axis": "horizontal" })).unwrap(),
            IssueCode::UnknownVersion,
            true,
        ),
        (
            single_node_graph("image.flip", 1, json!({ "axis": "diagonal" })).unwrap(),
            IssueCode::OutOfRange,
            false,
        ),
        (
            single_node_graph("image.flip", 1, json!({ "axis": "horizontal", "extra": 1 }))
                .unwrap(),
            IssueCode::BadParams,
            false,
        ),
        (
            single_node_graph("audio.gain", 1, json!({ "db": 1.0 })).unwrap(),
            IssueCode::BadInput,
            false,
        ),
        (
            single_node_graph(
                "image.crop",
                1,
                json!({ "rect": { "x": i64::MAX, "y": 0, "w": 1, "h": 1 } }),
            )
            .unwrap(),
            IssueCode::OutOfRange,
            false,
        ),
    ];
    for (g, code, at_load) in cases {
        let issues = engine
            .compile(&g, &shapes)
            .err()
            .expect("計画にできないこと");
        assert_eq!(issues.0[0].code, code, "{issues:?}");
        assert!(issues.0[0].node.is_some());
        let loaded = engine.load(&g.to_canonical_json().unwrap());
        assert_eq!(loaded.is_err(), at_load, "load: {code:?}");
    }
}

/// 外したノードは入力をそのまま通す。
#[test]
fn disabled_nodes_pass_through() {
    let engine = engine();
    let input = InputGen::Noise {
        w: 20,
        h: 10,
        fmt: PixelFormat::Rgba8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let mut g = single_node_graph(
        "image.crop",
        1,
        json!({ "rect": { "x": 1, "y": 1, "w": 3, "h": 3 } }),
    )
    .unwrap();
    g.nodes[0].enabled = false;
    let out = kura_engine::testing::render_with(
        &engine,
        &g,
        input.clone(),
        Default::default(),
        &NoMonitor,
    )
    .unwrap();
    assert_eq!(out, input);
}

/// E25：上限を超える入力・出力は推論の段階で断る。
#[test]
fn limits_are_checked_before_rendering() {
    let limits = kura_engine::Limits {
        max_dim: 16,
        ..kura_engine::Limits::default()
    };
    let engine = Engine::with_limits(limits).unwrap();
    let input = InputGen::Noise {
        w: 20,
        h: 10,
        fmt: PixelFormat::Gray8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let g = single_node_graph("image.flip", 1, json!({ "axis": "horizontal" })).unwrap();
    let issues = engine
        .compile(&g, &BTreeMap::from([(InputId(0), input.shape())]))
        .err()
        .unwrap();
    assert_eq!(issues.0[0].code, IssueCode::TooLarge);
}

/// 描くときの入力の値は、計画の形と上限に照らしてから使う（ヘッダの形と実際の値が食い違う場合）。
#[test]
fn render_checks_inputs_against_the_plan() {
    let engine = engine();
    let small = InputGen::Noise {
        w: 10,
        h: 10,
        fmt: PixelFormat::Gray8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let big = InputGen::Noise {
        w: 30,
        h: 30,
        fmt: PixelFormat::Gray8,
        seed: 1,
    }
    .generate()
    .unwrap();
    let g = single_node_graph("image.flip", 1, json!({ "axis": "horizontal" })).unwrap();
    let plan = engine
        .compile(&g, &BTreeMap::from([(InputId(0), small.shape())]))
        .unwrap();
    let with = |v: Value| {
        BTreeMap::from([(
            InputId(0),
            InputValue {
                value: v,
                source: Default::default(),
            },
        )])
    };
    assert!(matches!(
        engine.render(&plan, &with(big), &NoMonitor),
        Err(EngineError::Invalid(_))
    ));
    assert!(matches!(
        engine.render(&plan, &BTreeMap::new(), &NoMonitor),
        Err(EngineError::Invalid(_))
    ));
    assert!(engine.render(&plan, &with(small), &NoMonitor).is_ok());
}

/// 落としたメタデータの警告に、どのノードのものかが付く。写せないものは落とし、そのほかは運ぶ。
#[test]
fn warnings_and_carried_metadata() {
    let engine = engine();
    let sheet = InputGen::Sheet {
        cols: 4,
        rows: 2,
        cell: 16,
    }
    .generate()
    .unwrap();
    // ピボット (8, 16) が外に出る切り抜き
    let g = single_node_graph(
        "image.crop",
        1,
        json!({ "rect": IRect::new(20, 0, 16, 10) }),
    )
    .unwrap();
    let rendered = render_full(&engine, &g, sheet, Default::default(), &NoMonitor).unwrap();
    assert!(
        rendered
            .warnings
            .iter()
            .any(|w| w.code == IssueCode::MetadataDropped && w.node.is_some()),
        "{:?}",
        rendered.warnings
    );

    let Value::Image(img) = InputGen::Noise {
        w: 8,
        h: 8,
        fmt: PixelFormat::Rgb8,
        seed: 2,
    }
    .generate()
    .unwrap() else {
        panic!()
    };
    let mut img = Arc::unwrap_or_clone(img);
    img.meta.dpi = Some((300, 300));
    img.meta.text.insert("Author".into(), "kura".into());
    img.meta.icc = Some(Arc::new(vec![1, 2, 3]));
    let input = Value::Image(Arc::new(img.clone()));
    for g in [
        single_node_graph("image.crop", 1, json!({ "rect": IRect::new(1, 1, 4, 4) })).unwrap(),
        single_node_graph("image.flip", 1, json!({ "axis": "vertical" })).unwrap(),
    ] {
        let Value::Image(out) = kura_engine::testing::render_with(
            &engine,
            &g,
            input.clone(),
            Default::default(),
            &NoMonitor,
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            (out.meta.dpi, &out.meta.text, &out.meta.icc),
            (img.meta.dpi, &img.meta.text, &img.meta.icc)
        );
    }

    let Value::Audio(a) = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 100,
        seed: 3,
    }
    .generate()
    .unwrap() else {
        panic!()
    };
    let mut a = Arc::unwrap_or_clone(a);
    a.meta = AudioMeta {
        tags: [("title".to_string(), "se".to_string())].into(),
        ..AudioMeta::default()
    };
    let g = single_node_graph("audio.gain", 1, json!({ "db": -3.0 })).unwrap();
    let Value::Audio(out) = kura_engine::testing::render_with(
        &engine,
        &g,
        Value::Audio(Arc::new(a.clone())),
        Default::default(),
        &NoMonitor,
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(out.meta.tags, a.meta.tags);
}

/// すべての操作の引数が、どの層でも知らない項目を拒む（入れ子の点・矩形・比・格子・描画の動作も。文書の項目を黙って捨てない。ADR-143 (4)）。
#[test]
fn every_op_rejects_unknown_param_keys() {
    // 引数の JSON の中のすべてのオブジェクト（配列の中も、入れ子も）の場所
    fn objects(v: &serde_json::Value, path: Vec<String>, out: &mut Vec<Vec<String>>) {
        match v {
            serde_json::Value::Object(map) => {
                out.push(path.clone());
                for (k, child) in map {
                    let mut p = path.clone();
                    p.push(k.clone());
                    objects(child, p, out);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, child) in items.iter().enumerate() {
                    let mut p = path.clone();
                    p.push(i.to_string());
                    objects(child, p, out);
                }
            }
            _ => {}
        }
    }
    each_example(|_, op, ex| {
        let mut places = Vec::new();
        objects(&ex.params, Vec::new(), &mut places);
        for place in places {
            let mut raw = ex.params.clone();
            let mut at = &mut raw;
            for k in &place {
                at = match at {
                    serde_json::Value::Object(m) => m.get_mut(k).unwrap(),
                    serde_json::Value::Array(a) => a.get_mut(k.parse::<usize>().unwrap()).unwrap(),
                    _ => unreachable!(),
                };
            }
            at.as_object_mut()
                .unwrap()
                .insert("unexpectedKey".into(), json!(1));
            let issues = op.parse(&raw).err().unwrap_or_else(|| {
                panic!(
                    "{} {}：{} の知らない項目を受け付けた",
                    op.id(),
                    ex.name,
                    if place.is_empty() {
                        "引数".to_string()
                    } else {
                        place.join(".")
                    }
                )
            });
            assert_eq!(issues.0[0].code, IssueCode::BadParams, "{}", op.id());
        }
    });
}

/// 中身の違うフレームの列でも、各フレームに当てた結果と同じになる。
#[test]
fn per_frame_lift_matches_on_distinct_frames() {
    each_example(|engine, op, ex| {
        if op.signature().lift != Lift::PerFrame {
            return;
        }
        let input = ex.inputs[0].generate().unwrap();
        let Value::Image(img) = &input else { return };
        let (w, h, fmt) = (img.px.width(), img.px.height(), img.px.format());
        let frames = InputGen::NoiseFrames {
            w,
            h,
            fmt,
            count: 3,
            seed: 21,
        }
        .generate()
        .unwrap();
        let Value::Frames(fr) = &frames else { panic!() };
        let g = graph(op, ex);
        let Value::Frames(out) = render(engine, &g, &frames, ex).unwrap() else {
            panic!()
        };
        for (i, f) in fr.frames().iter().enumerate() {
            let single = Value::Image(Arc::new(Image {
                px: f.px.clone(),
                meta: ImageMeta::default(),
            }));
            let Value::Image(one) = render(engine, &g, &single, ex).unwrap() else {
                panic!()
            };
            assert_eq!(
                out.frames()[i].px,
                one.px,
                "{} {} フレーム {i}",
                op.id(),
                ex.name
            );
        }
    });
}

/// トリムしたフレームの列には持ち上げられない（計画の段階で断る。推論で分からなかったときは実行で断る。ADR-143 (1)）。
#[test]
fn lifting_refuses_trimmed_frames() {
    use kura_engine::geom::{IPoint, Size};
    use kura_engine::shape::{Dim, Fullness, Shape};
    use kura_engine::value::Frame;
    let engine = engine();
    let px = Raster::filled(2, 2, PixelFormat::Gray8, None, &[9]).unwrap();
    let trimmed = Value::Frames(Arc::new(
        Frames::new(
            vec![Frame {
                px,
                offset: IPoint { x: 1, y: 1 },
                duration_ms: 10,
            }],
            Size { w: 4, h: 4 },
            ImageMeta::default(),
            0,
        )
        .unwrap(),
    ));
    let g = single_node_graph("image.flip", 1, json!({ "axis": "horizontal" })).unwrap();
    let issues = engine
        .compile(&g, &BTreeMap::from([(InputId(0), trimmed.shape())]))
        .err()
        .unwrap();
    assert_eq!(issues.0[0].code, IssueCode::BadInput);
    // 推論では「分からない」とした計画に、実際はトリムした列を渡す
    let unknown = Shape::Frames {
        count: Dim::Exact(1),
        w: Dim::Exact(4),
        h: Dim::Exact(4),
        fmt: PixelFormat::Gray8,
        full: Fullness::Unknown,
    };
    let plan = engine
        .compile(&g, &BTreeMap::from([(InputId(0), unknown)]))
        .unwrap();
    let r = engine.render(
        &plan,
        &BTreeMap::from([(
            InputId(0),
            InputValue {
                value: trimmed,
                source: Default::default(),
            },
        )]),
        &NoMonitor,
    );
    assert!(matches!(r, Err(EngineError::Invalid(_))), "{r:?}");
}

/// メモリ上の文書（16.0・-0.0）と、保存して読み直した文書で、計画と結果が同じ（文書のハッシュが同じなら同じ引数）。
#[test]
fn in_memory_and_reloaded_documents_render_the_same() {
    let engine = engine();
    let input = InputGen::Noise {
        w: 64,
        h: 64,
        fmt: PixelFormat::Rgba8,
        seed: 3,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear(
        "image.crop",
        1,
        json!({ "rect": { "x": 16.0, "y": 16, "w": 32.0, "h": 32 } }),
        0,
    )
    .unwrap();
    let reloaded = engine.load(&g.to_canonical_json().unwrap()).unwrap();
    assert_eq!(g.digest().unwrap(), reloaded.digest().unwrap());
    let a = kura_engine::testing::render_with(
        &engine,
        &g,
        input.clone(),
        Default::default(),
        &NoMonitor,
    )
    .unwrap();
    let b = kura_engine::testing::render_with(
        &engine,
        &reloaded,
        input,
        Default::default(),
        &NoMonitor,
    )
    .unwrap();
    assert_eq!(a, b);
    let snd = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 100,
        seed: 3,
    }
    .generate()
    .unwrap();
    let mut g = Graph::new_single();
    g.push_linear("audio.gain", 1, json!({ "db": -0.0 }), 0)
        .unwrap();
    let reloaded = engine.load(&g.to_canonical_json().unwrap()).unwrap();
    let a =
        kura_engine::testing::render_with(&engine, &g, snd.clone(), Default::default(), &NoMonitor)
            .unwrap();
    let b =
        kura_engine::testing::render_with(&engine, &reloaded, snd, Default::default(), &NoMonitor)
            .unwrap();
    assert_eq!(a.digest().unwrap(), b.digest().unwrap());
}

/// 引数の表示の条件（docs/editing.md §7.2）：条件の引数は同じ記述の列挙で、値はその列挙の中にあり、自分自身でも条件付きの
/// 引数でもない（1 段だけ）。`image.convert` の色の数・ディザは Indexed8 のときだけ、下地の色はアルファを外す形式のときだけ。
#[test]
fn visibility_conditions_name_enum_params() {
    use kura_engine::op::ParamKind;
    let engine = engine();
    let mut conditioned = 0;
    for op in engine.registry().all() {
        let schema = op.schema();
        for p in &schema.params {
            let Some(vis) = p.visible else { continue };
            conditioned += 1;
            assert_ne!(
                vis.key,
                p.key,
                "{}.{}: 自分自身を条件にしています",
                op.id(),
                p.key
            );
            let target = schema
                .params
                .iter()
                .find(|q| q.key == vis.key)
                .unwrap_or_else(|| {
                    panic!("{}.{}: 条件の引数 {} がありません", op.id(), p.key, vis.key)
                });
            assert!(
                target.visible.is_none(),
                "{}.{}: 条件の引数 {} も条件付きです",
                op.id(),
                p.key,
                vis.key
            );
            let ParamKind::Enum { values } = &target.kind else {
                panic!(
                    "{}.{}: 条件の引数 {} が列挙ではありません",
                    op.id(),
                    p.key,
                    vis.key
                );
            };
            assert!(!vis.values.is_empty(), "{}.{}", op.id(), p.key);
            for v in vis.values {
                assert!(
                    values.contains(v),
                    "{}.{}: {v} は {} の値にありません",
                    op.id(),
                    p.key,
                    vis.key
                );
            }
        }
    }
    assert!(conditioned >= 3, "{conditioned}");
    let convert = engine
        .registry()
        .catalog()
        .into_iter()
        .find(|d| d.id == "image.convert")
        .unwrap();
    let when = |key: &str| {
        convert
            .schema
            .params
            .iter()
            .find(|p| p.key == key)
            .and_then(|p| p.visible)
            .map(|v| (v.key, v.values.to_vec()))
    };
    assert_eq!(when("format"), None);
    assert_eq!(when("colors"), Some(("format", vec!["indexed8"])));
    assert_eq!(when("dither"), Some(("format", vec!["indexed8"])));
    assert_eq!(
        when("matte"),
        Some(("format", vec!["gray8", "rgb8", "gray16", "rgb16"]))
    );
    // 記述の JSON（カタログ）：表示の条件は `visible`（なければ null）
    let json = serde_json::to_value(&convert.schema).unwrap();
    assert_eq!(json["params"][0]["visible"], serde_json::Value::Null);
    assert_eq!(
        json["params"][1]["visible"],
        json!({ "key": "format", "values": ["indexed8"] })
    );
}
