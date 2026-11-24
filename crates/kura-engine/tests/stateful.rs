//! M17 段 1：状態を運ぶ操作の実行器の道（docs/editing.md §18.1・§18.2）。試験の操作 `test.iir1`（登録しない。
//! `testing::stateful`）で、状態の表を申告した解析として流して解き、表から区間を描く核が、どの道（全体の描画・範囲の描画・
//! 表示の経路・流れ・保存）でも先頭から順に流した参照とビット一致することと、流し解きの数・見積もりを確かめる。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use kura_engine::doc::{Graph, InputId, OutputId};
use kura_engine::render::area::AreaRequest;
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::region::Area;
use kura_engine::render::view::ViewRequest;
use kura_engine::render::{Engine, InputValue, Plan};
use kura_engine::testgen::InputGen;
use kura_engine::testing::region::check_region;
use kura_engine::testing::stateful::{
    Iir1Params, OP_ID, engine as stateful_engine, reference, solves,
};
use kura_engine::value::{AUDIO_CHUNK, Audio, AudioMeta};
use kura_engine::{Digest, NoMonitor, Value};
use serde_json::json;

const C: u64 = AUDIO_CHUNK as u64;

fn engine() -> Engine {
    stateful_engine().unwrap()
}

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

fn audio(v: &Value) -> Arc<Audio> {
    match v {
        Value::Audio(a) => a.clone(),
        _ => panic!("音声ではありません"),
    }
}

/// 引数（b1 は乱数の文書の引数（0.01 刻み）と重ならない値にして、表を解いた回数が他のテストと混ざらないようにする）。
fn params(b0: f64, a1: f64, lookahead: u64, link: bool, grid: u64) -> Iir1Params {
    Iir1Params {
        b0,
        b1: 0.1234,
        a1,
        lookahead,
        link,
        grid,
        wet: 0.8,
        gain_db: -1.0,
    }
}

/// 試験の操作の鎖（`before` の操作を前に置く）。
fn chain(before: &[(&str, serde_json::Value)], iirs: &[Iir1Params]) -> Graph {
    let mut g = Graph::new_single();
    let mut seed = 1;
    for (op, p) in before {
        g.push_linear(op, 1, p.clone(), seed).unwrap();
        seed += 1;
    }
    for p in iirs {
        g.push_linear(OP_ID, 1, serde_json::to_value(p).unwrap(), seed)
            .unwrap();
        seed += 1;
    }
    g
}

fn compile(engine: &Engine, g: &Graph, input: &Value) -> Plan {
    engine
        .compile(g, &BTreeMap::from([(InputId(0), input.shape())]))
        .unwrap()
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

fn fin(input: &Value) -> BTreeMap<InputId, FingerprintedInput> {
    BTreeMap::from([(
        InputId(0),
        FingerprintedInput::new(input.clone(), Digest([3; 32])).unwrap(),
    )])
}

/// 参照を鎖に順に当てる。
fn reference_chain(input: &Value, iirs: &[Iir1Params]) -> Value {
    let mut a = audio(input);
    for p in iirs {
        let planes = reference(p, &a).unwrap();
        a = Arc::new(
            Audio::new(
                a.rate(),
                planes.into_iter().map(Arc::new).collect(),
                a.meta.clone(),
            )
            .unwrap(),
        );
    }
    Value::Audio(a)
}

fn same_bits(a: &Value, b: &Value) -> bool {
    let (a, b) = (audio(a), audio(b));
    a.channels() == b.channels()
        && a.frames() == b.frames()
        && (0..a.channels()).all(|ch| {
            let mut x = vec![0f32; a.frames()];
            let mut y = vec![0f32; b.frames()];
            a.read(ch, 0, &mut x).unwrap();
            b.read(ch, 0, &mut y).unwrap();
            x.iter().zip(&y).all(|(p, q)| p.to_bits() == q.to_bits())
        })
}

/// 入れ子の鎖（状態を運ぶ操作 3 つ。格子 C・3·4,096・2C、先読み・リンク）：全体の描画（一括の道）・範囲の描画（キャッシュの上限
/// 0 と 1 GiB）・流れ・保存（WAV・FLAC・Ogg Vorbis。Ogg は 2 回の流し）のどれも、先頭から順に流した参照とビット一致する。
/// 表は描画ごとに 1 回ずつ解く（入れ子の流し解きの窓ごとに解き直さない。キャッシュがあれば 2 回目から解かない。保存は
/// Ogg Vorbis の 2 回の流しでも 1 回）。
#[test]
fn nested_chain_matches_the_reference() {
    let engine = engine();
    let input = noise(2, 3 * C + 17, 5);
    let iirs = [
        params(0.4, -0.7, 0, false, C),
        params(0.9, 0.3, 37, true, 3 * 4_096),
        params(-0.5, -0.2, 5, false, 2 * C),
    ];
    let g = chain(&[], &iirs);
    let plan = compile(&engine, &g, &input);
    let want = reference_chain(&input, &iirs);
    // 一括の道
    let got = render(&engine, &plan, &input);
    assert!(same_bits(&got, &want), "render が参照と違う");
    let counts = || iirs.iter().map(|p| solves(&p.state())).collect::<Vec<_>>();
    // 範囲の描画
    for cap in [0u64, 1 << 30] {
        let mut cache = NodeCache::new(cap);
        for (round, area) in [
            Area::span(C + 5, C + 6),
            Area::span(2 * C - 3, 2 * C + 100),
            Area::span(3 * C, 3 * C + 17),
        ]
        .into_iter()
        .enumerate()
        {
            let before = counts();
            let r = engine
                .render_area(
                    &plan,
                    &fin(&input),
                    &mut cache,
                    &AreaRequest {
                        output: OutputId(0),
                        area: area.clone(),
                    },
                    None,
                    &NoMonitor,
                )
                .unwrap();
            let what = format!("上限 {cap}・{round} 回目");
            check_region(&r.value, &want, &area).unwrap_or_else(|m| panic!("{what}: {m}"));
            assert_eq!((r.stats.fallbacks, r.stats.rebased), (0, 0), "{what}");
            let once = u64::from(!(cap > 0 && round > 0));
            let solved: Vec<u64> = counts().iter().zip(&before).map(|(a, b)| a - b).collect();
            assert_eq!(solved, [once; 3], "{what}: {:?}", r.stats);
        }
    }
    // 流れのダイジェスト
    let d = engine
        .digest_audio_output(
            &plan,
            &fin(&input),
            &mut NodeCache::new(0),
            OutputId(0),
            &NoMonitor,
        )
        .unwrap();
    assert_eq!(d, want.digest().unwrap());
    // 保存（流して書く）
    let codecs = kura_engine::codec::CodecSet::pinned();
    for (format, opts) in [
        ("wav", json!({ "float": true })),
        ("flac", json!({ "bits": 24 })),
        ("ogg", json!({})),
    ] {
        let (bytes, _) = codecs
            .encode_bytes(&want, format, &opts, &NoMonitor)
            .unwrap();
        let before = counts();
        let mut cache = NodeCache::new(0);
        let mut stream = engine
            .output_stream(&plan, &fin(&input), &mut cache, OutputId(0), &NoMonitor)
            .unwrap();
        let mut out = std::io::Cursor::new(Vec::new());
        codecs
            .encode_stream(&mut stream, format, &opts, &mut out, &NoMonitor)
            .unwrap();
        assert_eq!(out.into_inner(), bytes, "{format}");
        let solved: Vec<u64> = counts().iter().zip(&before).map(|(a, b)| a - b).collect();
        assert_eq!(solved, [1; 3], "{format}");
    }
}

/// 16 段の鎖：鎖の深さだけ流し解きが入れ子になる（再帰）が、デバッグのビルドのスタックで描け、見積もりも入れ子ごとに
/// 1 度ずつ数えて終わる（指数的に増えない）。値は参照と同じ。
#[test]
fn deep_chains_render_and_estimate() {
    let engine = engine();
    let input = noise(1, 2 * C + 5, 6);
    let iirs: Vec<Iir1Params> = (0..16)
        .map(|i| {
            params(
                0.3 + i as f64 / 40.0,
                -0.5,
                (i % 3) as u64,
                false,
                [C, 3 * 4_096, 2 * C][i % 3],
            )
        })
        .collect();
    let g = chain(&[("audio.gain", json!({ "db": -2.0 }))], &iirs);
    let plan = compile(&engine, &g, &input);
    let want = reference_chain(
        &render(
            &engine,
            &compile(
                &engine,
                &chain(&[("audio.gain", json!({ "db": -2.0 }))], &[]),
                &input,
            ),
            &input,
        ),
        &iirs,
    );
    let area = Area::span(C + 10, C + 20);
    let spec = EstimateSpec::new(4)
        .all_inputs(&plan, Source::Held)
        .area(OutputId(0), area.clone());
    let est = engine.estimate(&plan, &spec, None).unwrap();
    assert!(est.peak > 0);
    let before: Vec<u64> = iirs.iter().map(|p| solves(&p.state())).collect();
    let r = engine
        .render_area(
            &plan,
            &fin(&input),
            &mut NodeCache::new(0),
            &AreaRequest {
                output: OutputId(0),
                area: area.clone(),
            },
            None,
            &NoMonitor,
        )
        .unwrap();
    check_region(&r.value, &want, &area).unwrap();
    let solved: Vec<u64> = iirs
        .iter()
        .zip(&before)
        .map(|(p, b)| solves(&p.state()) - b)
        .collect();
    assert_eq!(solved, [1; 16]);
}

/// 表示の経路の 1 手：表に入れない引数（wet・gainDb）だけの手は表を解き直さない。状態に効く引数の手は 1 回解き直す。
#[test]
fn output_only_changes_reuse_the_table() {
    let engine = engine();
    let input = noise(2, 3 * C, 7);
    let trim = [("audio.trim", json!({ "start": 1_234, "end": 3 * C - 9 }))];
    let p0 = params(0.4, -0.6, 3, true, C);
    let req = ViewRequest {
        output: OutputId(0),
        area: Area::span(C, C + 4_000),
        focus: None,
    };
    let mut cache = NodeCache::new(1 << 30);
    let plan = compile(&engine, &chain(&trim, std::slice::from_ref(&p0)), &input);
    let before = solves(&p0.state());
    let first = engine
        .render_view(&plan, &fin(&input), &mut cache, &req, None, &NoMonitor)
        .unwrap();
    assert_eq!(solves(&p0.state()) - before, 1);
    let mut prev = first.view;
    for (p, expected) in [
        (
            Iir1Params {
                gain_db: 3.0,
                wet: 0.5,
                ..p0.clone()
            },
            0,
        ),
        (
            Iir1Params {
                b0: 0.7,
                ..p0.clone()
            },
            1,
        ),
    ] {
        let expected: u64 = expected;
        let g = chain(&trim, std::slice::from_ref(&p));
        let plan = compile(&engine, &g, &input);
        let want = render(&engine, &plan, &input);
        let before = solves(&p.state());
        let got = engine
            .render_view(
                &plan,
                &fin(&input),
                &mut cache,
                &req,
                Some(&prev),
                &NoMonitor,
            )
            .unwrap();
        check_region(got.view.value(), &want, &req.area).unwrap();
        assert_eq!(solves(&p.state()) - before, expected, "{:?}", got.area);
        prev = got.view;
    }
}

/// 上流の手が表示の外だけを変える（末尾のフェードアウト）：状態を運ぶ操作の表の入力は変わるが、表示の区間は前の値と共有
/// できるので、表を流し直さない（描くチャンクのないノードは解析を解かない）。値は `render` と同じ。状態を運ぶ操作が出力の
/// ノードのときと、後ろに gain が続くとき。
#[test]
fn changes_outside_the_view_do_not_solve() {
    let engine = engine();
    let input = noise(2, 4 * C, 8);
    let p = params(0.4, -0.6, 0, false, C);
    let fade = |out: u64| {
        (
            "audio.fade",
            json!({ "fadeIn": 0, "fadeOut": out, "curve": "linear" }),
        )
    };
    for tail in [false, true] {
        let doc = |out| {
            let mut g = chain(&[fade(out)], std::slice::from_ref(&p));
            if tail {
                g.push_linear("audio.gain", 1, json!({ "db": -3.0 }), 99)
                    .unwrap();
            }
            g
        };
        let req = ViewRequest {
            output: OutputId(0),
            area: Area::span(100, C / 2),
            focus: None,
        };
        let mut cache = NodeCache::new(1 << 30);
        let plan = compile(&engine, &doc(1_000), &input);
        let first = engine
            .render_view(&plan, &fin(&input), &mut cache, &req, None, &NoMonitor)
            .unwrap();
        let plan = compile(&engine, &doc(5_000), &input);
        let want = render(&engine, &plan, &input);
        let before = solves(&p.state());
        let got = engine
            .render_view(
                &plan,
                &fin(&input),
                &mut cache,
                &req,
                Some(&first.view),
                &NoMonitor,
            )
            .unwrap();
        check_region(got.view.value(), &want, &req.area).unwrap();
        assert_eq!(
            solves(&p.state()) - before,
            0,
            "後ろに gain {tail}: {:?}",
            got.area
        );
    }
}

/// 有限でない標本：0 としてフィルタに通し、その位置は元の値のまま（参照と同じ）。警告は全体の描画で 1 回。
#[test]
fn non_finite_samples_follow_the_rule() {
    use kura_engine::testgen::NonFiniteSample;
    let engine = engine();
    let input = InputGen::NonFinite {
        base: Box::new(InputGen::AudioNoise {
            rate: 8_000,
            channels: 2,
            frames: C + 300,
            seed: 9,
        }),
        at: vec![
            (0, 3, NonFiniteSample::Nan),
            (1, C - 1, NonFiniteSample::Inf),
            (1, C + 1, NonFiniteSample::NegInf),
        ],
    }
    .generate()
    .unwrap();
    let p = params(0.4, -0.6, 2, true, 1_000);
    let plan = compile(&engine, &chain(&[], std::slice::from_ref(&p)), &input);
    let plain = BTreeMap::from([(
        InputId(0),
        InputValue {
            value: input.clone(),
            source: Digest([3; 32]),
        },
    )]);
    let r = engine.render(&plan, &plain, &NoMonitor).unwrap();
    let want = reference_chain(&input, &[p]);
    assert!(same_bits(&r.outputs[0].value, &want));
    let codes: Vec<_> = r.warnings.iter().map(|w| w.code).collect();
    assert_eq!(codes, [kura_engine::IssueCode::NonFiniteSamples]);
}

/// 見積もりは、範囲の描画の中で解いた表を描画の終わりまで数える：表を解くノードより後ろの段（gain）を描く間の量に、表の
/// 大きさが入る（表は範囲の描画の中の表 `Presolved` が終わりまで持つ）。
#[test]
fn estimates_hold_the_tables() {
    use kura_engine::analysis::Analysis;
    use kura_engine::testing::stateful::Iir1State;
    let engine = engine();
    // 状態の大きい表（先読み 4,000・格子 100）を、範囲の描画の流し解きで解き、後ろに gain を置く
    let input = noise(1, 2 * C, 10);
    let big = params(0.4, -0.6, 4_000, false, 100);
    let mut g = chain(
        &[("audio.gain", json!({ "db": -2.0 }))],
        std::slice::from_ref(&big),
    );
    g.push_linear("audio.gain", 1, json!({ "db": -1.0 }), 99)
        .unwrap();
    let plan = compile(&engine, &g, &input);
    let table = Iir1State::estimate(&big.state(), &input.shape()).bytes;
    assert!(table > 40 << 20, "{table}");
    let spec = EstimateSpec::new(1)
        .all_inputs(&plan, Source::Held)
        .area(OutputId(0), Area::span(10, 20));
    let est = engine.estimate(&plan, &spec, None).unwrap();
    let last = est.nodes.last().unwrap();
    assert!(last.at >= table, "後ろの段 {} < 表 {table}", last.at);
}

/// 見積もりは、入れ子の流し解きで同じ表を何度も数えない：状態を運ぶ操作を k 段つないだ鎖（表 1 つ約 2.5 MB）の範囲の
/// 描画の見積もりは、段の数にほぼ比例して増える（上流の表を下流の入れ子ごとに数え直すと、深さで倍々になる。見直しで
/// 見つけた。M17 段 1）。
#[test]
fn nested_estimates_grow_linearly() {
    use kura_engine::analysis::Analysis;
    use kura_engine::testing::stateful::Iir1State;
    let engine = engine();
    let input = noise(1, 40 * C, 11);
    let p = |i: usize| params(0.3 + i as f64 / 50.0, -0.6, 8_000, false, C);
    let table = Iir1State::estimate(&p(0).state(), &input.shape()).bytes;
    assert!(table > 2 << 20, "{table}");
    let peaks: Vec<u64> = (1..=12)
        .map(|k| {
            let iirs: Vec<Iir1Params> = (0..k).map(p).collect();
            let g = chain(&[("audio.gain", json!({ "db": -2.0 }))], &iirs);
            let plan = compile(&engine, &g, &input);
            let spec = EstimateSpec::new(4)
                .all_inputs(&plan, Source::Held)
                .area(OutputId(0), Area::span(20 * C, 21 * C));
            engine.estimate(&plan, &spec, None).unwrap().peak
        })
        .collect();
    // 1 段ふやすごとの増え方は、表 1 つと窓（16 チャンク）の値の分くらいで、深さによらない（増え自体が段ごとに表の分ずつ
    // 大きくなると 2 乗。見直しで見つけた）
    let step = 2 * table + 2 * 16 * C * 4 * 4;
    let rises: Vec<u64> = peaks
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    for r in &rises {
        assert!(*r <= step, "{peaks:?}（1 段で {step} まで）");
    }
    let (first, last) = (rises[1], rises[rises.len() - 1]);
    assert!(
        last <= first + table / 2,
        "増えが深さで伸びる：{rises:?}（表 {table}）"
    );
}

/// 測り（M17 段 1。`--release -- --ignored --nocapture`、長さは `KURA_STATEFUL_SECS`（既定 300 秒））：48 kHz ステレオの合成の
/// 源で、trim → test.iir1 × k → gain の鎖の、表示 16 秒の最初の描画と、出力の全体のダイジェスト（保存と同じ流し）の時間・
/// 源を読んだ回数。試験の操作は 1 次の IIR なので、表の流しの重さは実物の EQ（段の数に比例）より小さい。実行器の入れ子の
/// 道の重さを見る。
#[test]
#[ignore = "測り（--release -- --ignored --nocapture）"]
fn stateful_chain_timings() {
    #![allow(clippy::disallowed_methods)]
    use kura_engine::audio::source::AudioSource;
    use kura_engine::render::cache::Fingerprint;
    use kura_engine::testing::source::SyntheticSource;
    const RATE: u64 = 48_000;
    let secs: u64 = std::env::var("KURA_STATEFUL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let frames = secs * RATE;
    let engine = engine();
    for k in 1..=4usize {
        let src = Arc::new(SyntheticSource::new(RATE as u32, 2, frames, 7, 64 << 20));
        let value = Value::Audio(Arc::new(
            Audio::from_source(src.clone(), AudioMeta::default()).unwrap(),
        ));
        let shape = value.shape();
        let inputs = BTreeMap::from([(
            InputId(0),
            FingerprintedInput {
                value,
                source: Digest([1; 32]),
                fingerprint: Fingerprint(Digest([2; 32])),
            },
        )]);
        let iirs: Vec<Iir1Params> = (0..k)
            .map(|i| params(0.4 + i as f64 / 10.0, -0.7, 0, false, C))
            .collect();
        let g = chain(
            &[(
                "audio.trim",
                json!({ "start": RATE + 123, "end": frames - RATE }),
            )],
            &iirs,
        );
        let plan = engine
            .compile(&g, &BTreeMap::from([(InputId(0), shape)]))
            .unwrap();
        let at = frames / 3;
        let req = ViewRequest {
            output: OutputId(0),
            area: Area::span(at, at + 16 * RATE),
            focus: None,
        };
        let total = frames.div_ceil(C);
        let mut cache = NodeCache::new(512 << 20);
        cache.register_external(&inputs[&InputId(0)].value);
        let t = std::time::Instant::now();
        engine
            .render_view(&plan, &inputs, &mut cache, &req, None, &NoMonitor)
            .unwrap();
        let view = t.elapsed().as_secs_f64();
        let reads_view = src.stats().reads;
        let mut cache = NodeCache::new(512 << 20);
        cache.register_external(&inputs[&InputId(0)].value);
        let before = src.stats().reads;
        let t = std::time::Instant::now();
        engine
            .digest_audio_output(&plan, &inputs, &mut cache, OutputId(0), &NoMonitor)
            .unwrap();
        let save = t.elapsed().as_secs_f64();
        let reads_save = src.stats().reads - before;
        eprintln!(
            "{secs} 秒・test.iir1 × {k}：表示 {view:.2} 秒（源 {:.1} 倍）／全体の流し {save:.2} 秒（源 {:.1} 倍）",
            reads_view as f64 / total as f64,
            reads_save as f64 / total as f64,
        );
    }
}
