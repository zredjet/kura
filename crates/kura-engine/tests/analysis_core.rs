//! 解析の核（docs/editing.md §11.1・§11.2）：エンジンの解析の口と申告の口の確かめ（段 8 の検証の指摘）。
//!
//! - 計れない低いレートの正規化（D1・S1）：`audio.loudness_gating` の見積もりがブロックを数えないので、段 6 で描けた
//!   文書が計画で断られず、描くと `LoudnessUnmeasurable` の警告で入力をそのまま返す。
//! - 型付きの `Engine::analyze` と `analyze_dyn` は同じ道で確かめる（X5）：同じ要求なら同じ結果・同じエラー。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::sync::Arc;

use kura_engine::analysis::audio::loudness_gating::LoudnessGating;
use kura_engine::analysis::audio::peaks::{PeaksAnalysis, PeaksParams};
use kura_engine::analysis::audio::silence::{SilenceAnalysis, SilenceParams};
use kura_engine::analysis::image::opaque_bounds::{OpaqueBounds, OpaqueBoundsParams};
use kura_engine::analysis::{Analysis, AnalysisOutput, AnalysisRequest, NoParams};
use kura_engine::digest::Digest;
use kura_engine::doc::InputId;
use kura_engine::geom::IRect;
use kura_engine::shape::{Dim, Shape};
use kura_engine::testgen::InputGen;
use kura_engine::testing::{render_full, single_node_graph};
use kura_engine::value::{Audio, AudioMeta};
use kura_engine::{Engine, EngineError, IssueCode, Limits, NoMonitor, Value};
use serde_json::json;

fn low_rate(frames: u64) -> Value {
    InputGen::Sine {
        rate: 8,
        channels: 1,
        frames,
        freq: 1,
        loop_range: None,
    }
    .generate()
    .unwrap()
}

/// D1・S1：段 6 で描けた低いレートの正規化の文書（`audio.normalize` v1）は、計画で断られない。検証で、8 Hz・
/// 1,300 万フレームのモノラルの見積もりが 4.42 GB になり、「結果が上限を超える見込み」で断られた（実際の結果は
/// 5 つの欄だけ）。描くと何も変えずに `LoudnessUnmeasurable` の警告を出す。
#[test]
fn normalize_on_unmeasurable_low_rates_is_planned_and_warns() {
    let engine = Engine::new().unwrap();
    let g = single_node_graph(
        "audio.normalize",
        1,
        json!({ "target": -23.0, "peakLimit": null }),
    )
    .unwrap();
    // 既定の上限で、検証が断られた形（と、計れる境の 1 つ下）。計画は形だけで作れる
    for (rate, frames) in [
        (8u32, 13_000_000u64),
        (100, 130_000_000),
        (414, 518_400_000),
        (3_363, 518_400_000),
    ] {
        let shapes = BTreeMap::from([(
            InputId(0),
            Shape::Audio {
                rate,
                channels: 1,
                frames: Dim::Exact(frames),
            },
        )]);
        let plan = engine
            .compile(&g, &shapes)
            .unwrap_or_else(|e| panic!("{rate} Hz・{frames} フレーム：{e}"));
        let needs = plan.node_needs(g.nodes[0].id).unwrap();
        assert_eq!(
            needs.iter().map(|n| (n.id, n.v)).collect::<Vec<_>>(),
            [("audio.loudness_gating", 1)]
        );
    }
    // 描く：検証の形そのもの（8 Hz・1,300 万フレーム、値はすべて 0.25。52 MB）と、値の上限を 1 MiB に絞った
    // エンジンでの 10 万フレーム（ブロックを数える見積もりなら 34 MB で断られる）
    let full = Value::Audio(Arc::new(
        Audio::new(
            8,
            vec![Arc::new(vec![0.25f32; 13_000_000])],
            AudioMeta::default(),
        )
        .unwrap(),
    ));
    let small = Engine::with_limits(Limits {
        max_value_bytes: 1 << 20,
        ..Limits::default()
    })
    .unwrap();
    let input = low_rate(100_000);
    for (engine, input) in [(&engine, &full), (&engine, &input), (&small, &input)] {
        let r = render_full(engine, &g, input.clone(), Digest::default(), &NoMonitor).unwrap();
        assert_eq!(&r.outputs[0].value, input);
        assert_eq!(
            r.warnings.iter().map(|w| w.code).collect::<Vec<_>>(),
            [IssueCode::LoudnessUnmeasurable]
        );
    }
    // 解析の口でも同じ（型付き・ID から）
    let typed = small
        .analyze::<LoudnessGating>(&input, &NoParams {}, &NoMonitor)
        .unwrap();
    assert!(!typed.measurable() && typed.blocks().is_empty());
    let dynamic = small
        .analyze_dyn(
            &AnalysisRequest::new("audio.loudness_gating"),
            &input,
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(dynamic.output, typed.to_canonical().unwrap());
}

/// 型付きの `analyze::<A>` と、同じ（ID, 版, 引数）の `analyze_dyn`。両方の結果（正規の形）かエラーが同じことを
/// 確かめて、型付きのほうを返す。
fn both<A: Analysis>(engine: &Engine, v: &Value, p: &A::Params) -> Result<Vec<u8>, EngineError>
where
    A::Params: Debug,
{
    let typed = engine
        .analyze::<A>(v, p, &NoMonitor)
        .and_then(|o| o.to_canonical());
    let req = AnalysisRequest {
        id: A::ID.into(),
        v: Some(A::VERSION),
        params: serde_json::to_value(p).unwrap(),
    };
    let dynamic = engine.analyze_dyn(&req, v, &NoMonitor).map(|a| a.output);
    match (&typed, &dynamic) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "{} {p:?}", A::ID),
        (Err(a), Err(b)) => {
            assert_eq!(a.code(), b.code(), "{} {p:?}", A::ID);
            assert_eq!(a.to_string(), b.to_string(), "{} {p:?}", A::ID);
        }
        _ => panic!("{} {p:?}：型付き {typed:?}、ID から {dynamic:?}", A::ID),
    }
    typed
}

fn issue_code(r: &Result<Vec<u8>, EngineError>) -> IssueCode {
    match r {
        Err(EngineError::Invalid(issues)) => issues.0[0].code,
        other => panic!("Invalid ではない：{other:?}"),
    }
}

/// X5：型付きの `Engine::analyze` も、引数を正規の形にして記述の範囲を確かめる（`analyze_dyn`・`Need::of` と同じ道）。
/// 検証で、`PeaksParams { finest: 3, coarsest: 3 }` は型付きで `Internal`、ID からは `OutOfRange` になり、
/// `threshold: 255` の opaque_bounds は型付きだけ「全部透明」と答えた。
#[test]
fn typed_and_dynamic_analyses_check_the_same_way() {
    let engine = Engine::new().unwrap();
    let audio = InputGen::AudioNoise {
        rate: 8_000,
        channels: 1,
        frames: 1_000,
        seed: 1,
    }
    .generate()
    .unwrap();
    let image = InputGen::Sprite {
        w: 20,
        h: 10,
        rect: IRect::new(2, 2, 5, 5),
    }
    .generate()
    .unwrap();

    // 範囲の外の引数
    let r = both::<PeaksAnalysis>(
        &engine,
        &audio,
        &PeaksParams {
            finest: 3,
            coarsest: 3,
        },
    );
    assert_eq!(issue_code(&r), IssueCode::OutOfRange);
    let r = both::<OpaqueBounds>(&engine, &image, &OpaqueBoundsParams { threshold: 255 });
    assert_eq!(issue_code(&r), IssueCode::OutOfRange);
    let r = both::<SilenceAnalysis>(&engine, &audio, &SilenceParams { threshold_db: 6.0 });
    assert_eq!(issue_code(&r), IssueCode::OutOfRange);
    // 段の順が逆（範囲の中の値どうしの食い違い）
    let r = both::<PeaksAnalysis>(
        &engine,
        &audio,
        &PeaksParams {
            finest: 4_096,
            coarsest: 256,
        },
    );
    assert!(matches!(r, Err(EngineError::Invalid(_))), "{r:?}");
    // 引数と種類の両方がおかしければ、引数を先に断る（どちらの口も同じ順）
    let r = both::<OpaqueBounds>(&engine, &audio, &OpaqueBoundsParams { threshold: 255 });
    assert_eq!(issue_code(&r), IssueCode::OutOfRange);
    // 受け付けない種類
    let r = both::<OpaqueBounds>(&engine, &audio, &OpaqueBoundsParams { threshold: 0 });
    assert_eq!(issue_code(&r), IssueCode::BadInput);

    // 正しい引数は同じ結果
    assert!(both::<PeaksAnalysis>(&engine, &audio, &PeaksParams::default()).is_ok());
    assert!(
        both::<PeaksAnalysis>(
            &engine,
            &audio,
            &PeaksParams {
                finest: 256,
                coarsest: 1_024,
            },
        )
        .is_ok()
    );
    let ob = both::<OpaqueBounds>(&engine, &image, &OpaqueBoundsParams { threshold: 7 }).unwrap();
    assert!(String::from_utf8(ob).unwrap().contains(r#""union":{"#));
    // -0 は正規の形で 0 になる（どちらの口も同じ値で解く）
    assert!(
        both::<SilenceAnalysis>(&engine, &audio, &SilenceParams { threshold_db: -0.0 }).is_ok()
    );
    assert!(
        both::<SilenceAnalysis>(
            &engine,
            &audio,
            &SilenceParams {
                threshold_db: -60.0
            }
        )
        .is_ok()
    );

    // 上限：値が上限を超える（1,000 フレームで 4,000 バイト）
    let tiny = Engine::with_limits(Limits {
        max_value_bytes: 1_000,
        ..Limits::default()
    })
    .unwrap();
    let r = both::<PeaksAnalysis>(&tiny, &audio, &PeaksParams::default());
    assert!(matches!(&r, Err(EngineError::Limit(_))), "{r:?}");
    // 結果が上限を超える見込み（値は 200 フレームで 800 バイトに収まり、見積もりは 1,148 バイト）
    let short = InputGen::AudioNoise {
        rate: 48_000,
        channels: 1,
        frames: 200,
        seed: 2,
    }
    .generate()
    .unwrap();
    let r = both::<LoudnessGating>(&tiny, &short, &NoParams {});
    assert!(
        matches!(&r, Err(EngineError::Limit(m)) if m.contains("見込み")),
        "{r:?}"
    );
    assert!(both::<LoudnessGating>(&engine, &short, &NoParams {}).is_ok());
}
