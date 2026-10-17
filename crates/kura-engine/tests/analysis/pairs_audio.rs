//! E26 の音声の分：意味の対・対のない解析・偽の値の作り方・核の経路の登録（`tests/analysis_ops.rs` が読む）。
//! 音声の解析を足す班は、ここに行を足す。対の検査は行（`Pair`）を受け取り、行の版の操作を当て（`Pair::apply`）、
//! 解析は登録表の（ID, 版）で引く（`Pair::analyze`・`analyze_at`）。補助に使うほかの操作は今の版で当てる。

use std::sync::Arc;

use kura_engine::analysis::audio::FrameSpan;
use kura_engine::analysis::audio::loop_points::LoopAnalysis;
use kura_engine::analysis::audio::loudness::{LoudnessAnalysis, Unmeasured};
use kura_engine::analysis::audio::loudness_gating::LoudnessGating;
use kura_engine::analysis::audio::silence::{SilenceAnalysis, SilenceParams};
use kura_engine::analysis::{Analysis, NoParams};
use kura_engine::audio::render_all;
use kura_engine::digest::Digest;
use kura_engine::op::Registry;
use kura_engine::ops::audio::gain::GainKernel;
use kura_engine::ops::audio::normalize::Normalize;
use kura_engine::render::Engine;
use kura_engine::testgen::{InputGen, MetaGen};
use kura_engine::testing::{render_full, single_node_graph};
use kura_engine::value::{Audio, AudioMeta, LoopRange};
use kura_engine::{IssueCode, NoMonitor, Value};
use rand_chacha::rand_core::Rng;
use serde_json::json;

use super::{FakeField, KernelOf, Pair, Perturb, analyze_at, current_op, err};

/// 意味の対（操作の版・解析の版ごとの行）。申告のある行は、操作が読む欄（`reads`）を書く。
pub const PAIRS: &[Pair] = &[
    Pair {
        op: "audio.normalize",
        op_v: 1,
        analysis: "audio.loudness_gating",
        analysis_v: 1,
        // 利得はブロックのパワーと「計れるか」から解き（`gain_to`）、上限はトゥルーピークで抑える。積分ラウドネスと
        // サンプルピークの欄は読まない
        reads: &["blocks", "measurable", "truePeak"],
        check: normalize_uses_the_gating_measurement,
    },
    Pair {
        op: "audio.normalize",
        op_v: 1,
        analysis: "audio.loudness",
        analysis_v: 1,
        reads: &[],
        check: normalize_matches_the_displayed_loudness,
    },
    Pair {
        op: "audio.trim",
        op_v: 1,
        analysis: "audio.silence",
        analysis_v: 1,
        reads: &[],
        check: trimming_to_the_content_leaves_no_silence,
    },
    Pair {
        op: "audio.set_loop",
        op_v: 1,
        analysis: "audio.loop",
        analysis_v: 1,
        reads: &[],
        check: set_loop_is_read_back,
    },
];

/// 対のない解析（どの操作とも意味の対を持たないもの）と理由。
pub const UNPAIRED: &[(&str, &str)] = &[(
    "audio.peaks",
    "波形の表示だけ（操作と対にならない）。段の一致・16 ビットの格子・バイナリの並びは analysis::audio::peaks と tests/audio_analysis.rs で確かめる",
)];

/// 偽の値の作り方（操作が申告する解析ごとに、結果の欄ごと。JSON の結果は欄の一覧が結果の欄と同じであること）。
/// バイナリの結果の解析を申告させるときは、型付きの値を変える `Perturb::Typed` で欄ごとに足す。
pub const FAKES: &[(&str, &[FakeField])] = &[(
    "audio.loudness_gating",
    &[
        FakeField {
            field: "integrated",
            perturb: Perturb::Json(louder_integrated),
        },
        FakeField {
            field: "samplePeak",
            perturb: Perturb::Json(higher_sample_peak),
        },
        FakeField {
            field: "truePeak",
            perturb: Perturb::Json(higher_true_peak),
        },
        FakeField {
            field: "blocks",
            perturb: Perturb::Json(louder_blocks),
        },
        FakeField {
            field: "measurable",
            perturb: Perturb::Json(flip_measurable),
        },
    ],
)];

/// 小数（`Real`）を +1 する。有限でない（"nan"・"-inf"）なら −20 にする。
fn bump_real(v: &mut serde_json::Value) {
    *v = match v.as_f64() {
        Some(x) => json!(x + 1.0),
        None => json!(-20.0),
    };
}

fn louder_integrated(out: &mut serde_json::Value) {
    bump_real(&mut out["integrated"]);
}

fn higher_sample_peak(out: &mut serde_json::Value) {
    bump_real(&mut out["samplePeak"]);
}

/// トゥルーピークを +1 dB（上限の効く例で利得が 1 dB 下がる）。
fn higher_true_peak(out: &mut serde_json::Value) {
    bump_real(&mut out["truePeak"]);
}

/// ブロックのパワーを 4 倍（+6 dB）にする（0 と有限でないものは 1 に）。ブロックがなければ 1 つ足す。
fn louder_blocks(out: &mut serde_json::Value) {
    let blocks = out["blocks"].as_array_mut().unwrap();
    if blocks.is_empty() {
        blocks.push(json!(1.0));
        return;
    }
    for b in blocks.iter_mut() {
        *b = match b.as_f64() {
            Some(v) if v > 0.0 => json!(v * 4.0),
            _ => json!(1.0),
        };
    }
}

fn flip_measurable(out: &mut serde_json::Value) {
    let m = out["measurable"].as_bool().unwrap();
    out["measurable"] = json!(!m);
}

/// 解析を申告して核を持つ音声の操作の、核の経路を照らす登録（`KernelOf` で包んだその操作だけの登録表）。
/// 核のある音声の操作に申告を足したら、ここに行を足す（ないと構造の網が失敗する）。
pub fn kernel_registry(op: &str, v: u16) -> Option<Result<Registry, String>> {
    let mut r = Registry::empty();
    let added = match (op, v) {
        ("audio.normalize", 1) => r.add_current::<KernelOf<Normalize>>(),
        _ => return None,
    };
    Some(added.map(|()| r))
}

/// 正規化（目標・ピークの上限）が、`audio.loudness_gating` の計測から解いた利得（`gain_to` と上限の抑え）を
/// `audio.gain` でかけたものとビット単位で同じ。計れなければ入力のまま（警告つき）。
fn check_normalize(
    engine: &Engine,
    p: &Pair,
    input: &Value,
    target: f64,
    peak_limit: Option<f64>,
) -> Result<(), String> {
    let ctx = format!("目標 {target}・上限 {peak_limit:?}");
    let m = p.analyze::<LoudnessGating>(engine, input, &NoParams {})?;
    let graph = single_node_graph(
        p.op,
        p.op_v,
        json!({ "target": target, "peakLimit": peak_limit }),
    )
    .map_err(err)?;
    let r =
        render_full(engine, &graph, input.clone(), Digest::default(), &NoMonitor).map_err(err)?;
    let out = &r.outputs[0].value;
    let Some(mut db) = m.gain_to(target) else {
        let warned = r
            .warnings
            .iter()
            .any(|w| w.code == IssueCode::LoudnessUnmeasurable);
        // NaN を含む値は == で比べられないので、ダイジェスト（NaN をそろえる）で比べる
        return if out.digest().unwrap() == input.digest().unwrap() && warned {
            Ok(())
        } else {
            Err(format!("{ctx}：計れないのに変えた（か警告がない）"))
        };
    };
    let peak = m.loudness.true_peak_db;
    if let Some(limit) = peak_limit
        && peak + db > limit
    {
        db = limit - peak;
    }
    // 標本は audio.gain と同じ核（GainKernel）。audio.gain の引数の範囲（−96〜48 dB）の中なら操作でも確かめる
    let Value::Audio(a) = input else {
        return Err(format!("{ctx}：音声ではない"));
    };
    let k = GainKernel::new(a.clone(), db);
    let gained = render_all(&k, a.meta.clone(), &NoMonitor).map_err(err)?;
    if *out != gained {
        return Err(format!(
            "{ctx}：計測から解いた利得 {db} dB をかけたものと違う"
        ));
    }
    if (-96.0..=48.0).contains(&db) {
        let gain = super::apply_op(
            engine,
            "audio.gain",
            current_op(engine, "audio.gain")?,
            json!({ "db": db }),
            input,
        )
        .map_err(|e| format!("{ctx}：{e}"))?;
        if *out != gain {
            return Err(format!("{ctx}：audio.gain {db} dB と違う"));
        }
    }
    Ok(())
}

fn noise(rate: u32, channels: usize, frames: usize, seed: u64, amp: f32) -> Value {
    let mut r = kura_engine::determinism::rng(seed);
    let planes = (0..channels)
        .map(|_| {
            Arc::new(
                (0..frames)
                    .map(|_| amp * (((r.next_u32() >> 8) as f32 / 8_388_608.0) - 1.0))
                    .collect::<Vec<f32>>(),
            )
        })
        .collect();
    Value::Audio(Arc::new(
        Audio::new(rate, planes, AudioMeta::default()).unwrap(),
    ))
}

fn normalize_uses_the_gating_measurement(engine: &Engine, p: &Pair) -> Result<(), String> {
    // 操作の例
    for ex in p.examples(engine)? {
        let input = ex.inputs[0].generate().map_err(err)?;
        let target = ex.params["target"].as_f64().ok_or("target")?;
        let limit = ex.params["peakLimit"].as_f64();
        check_normalize(engine, p, &input, target, limit)
            .map_err(|e| format!("{}: {e}", ex.name))?;
    }
    // 目標・上限・レート・チャンネル数・長さ（短い・低いレート・NaN）を変えて
    let inputs = [
        InputGen::SineLevels {
            rate: 8_000,
            freq: 1_000,
            segments: vec![(16_000, -66), (64_000, -73)],
        }
        .generate()
        .unwrap(),
        InputGen::Sine {
            rate: 3_000,
            channels: 1,
            frames: 6_000,
            freq: 440,
            loop_range: None,
        }
        .generate()
        .unwrap(),
        noise(48_000, 6, 30_000, 1, 0.3),
        noise(44_100, 2, 17_000, 2, 0.01),
        noise(96_000, 1, 50_000, 3, 0.8),
        noise(22_050, 2, 8_000, 4, 0.5),
        {
            let Value::Audio(a) = noise(8_000, 1, 20_000, 5, 0.5) else {
                unreachable!()
            };
            let mut p = (*a.planes()[0]).clone();
            p[100] = f32::NAN;
            Value::Audio(Arc::new(
                Audio::new(8_000, vec![Arc::new(p)], AudioMeta::default()).unwrap(),
            ))
        },
    ];
    for (i, input) in inputs.iter().enumerate() {
        for (target, limit) in [
            (-23.0, None),
            (-16.0, Some(-1.0)),
            (-40.0, None),
            (-69.0, None),
            (0.0, Some(-10.0)),
        ] {
            check_normalize(engine, p, input, target, limit)
                .map_err(|e| format!("入力 {i}: {e}"))?;
        }
    }
    Ok(())
}

/// 正規化と表示のラウドネスの対で使う入力：正規化の例の入力と、短い・無音・ゲートより静か・低いレート・NaN・5.1。
fn loudness_inputs(engine: &Engine, p: &Pair) -> Result<Vec<(String, Value)>, String> {
    let mut out = Vec::new();
    for ex in p.examples(engine)? {
        out.push((
            format!("normalize の例 {}", ex.name),
            ex.inputs[0].generate().map_err(err)?,
        ));
    }
    for (name, v) in p.analysis_inputs(engine)? {
        out.push((format!("{} の例 {name}", p.analysis), v));
    }
    out.push(("雑音 5.1".into(), noise(48_000, 6, 30_000, 11, 0.3)));
    out.push(("雑音 静か".into(), noise(44_100, 2, 50_000, 12, 0.000_2)));
    let Value::Audio(a) = noise(8_000, 1, 20_000, 13, 0.5) else {
        return Err("音声ではない".into());
    };
    let mut p = (*a.planes()[0]).clone();
    p[300] = f32::NAN;
    out.push((
        "NaN".into(),
        Value::Audio(Arc::new(
            Audio::new(8_000, vec![Arc::new(p)], AudioMeta::default()).map_err(err)?,
        )),
    ));
    Ok(out)
}

/// 正規化（`audio.normalize`）と表示のラウドネス（`audio.loudness`）：表示する積分ラウドネス・トゥルーピーク・
/// サンプルピークは、正規化が使う計測（その版の正規化が申告する `audio.loudness_gating`）の値とビット単位で同じ。
/// 表示が「計れない」（`tooShort`・`silent`・`lowRate`・`nonFinite`）のときだけ、正規化は何もしない（警告）。
/// 計れるとき、正規化した出力の表示の積分ラウドネスは目標の ±0.1 LU（E19）。
fn normalize_matches_the_displayed_loudness(engine: &Engine, p: &Pair) -> Result<(), String> {
    let gating = p.declared(engine, LoudnessGating::ID)?;
    for (name, input) in loudness_inputs(engine, p)? {
        let r = p
            .analyze::<LoudnessAnalysis>(engine, &input, &NoParams {})
            .map_err(|e| format!("{name}: {e}"))?;
        let m = analyze_at::<LoudnessGating>(engine, gating, &input, &NoParams {})
            .map_err(|e| format!("{name}: {e}"))?;
        for (field, got, want) in [
            ("integrated", &r.integrated, m.loudness.integrated),
            ("truePeak", &r.true_peak, m.loudness.true_peak_db),
            ("samplePeak", &r.sample_peak, m.loudness.sample_peak_db),
        ] {
            let same = match got.value() {
                Some(v) => v.to_bits() == want.to_bits(),
                None => !want.is_finite(),
            };
            if !same {
                return Err(format!(
                    "{name}：表示の {field}（{got:?}）と正規化の計測（{want}）が違う"
                ));
            }
        }
        let unmeasurable = matches!(
            r.integrated.reason(),
            Some(
                Unmeasured::TooShort
                    | Unmeasured::Silent
                    | Unmeasured::LowRate
                    | Unmeasured::NonFinite
            )
        );
        for target in [-23.0, -16.0] {
            if unmeasurable != m.gain_to(target).is_none() {
                return Err(format!(
                    "{name}：表示の理由 {:?} と、正規化の計れる・計れないが食い違う",
                    r.integrated
                ));
            }
            let out = p
                .apply(
                    engine,
                    json!({ "target": target, "peakLimit": null }),
                    &input,
                )
                .map_err(|e| format!("{name}: {e}"))?;
            if unmeasurable {
                let (a, b) = (out.digest().map_err(err)?, input.digest().map_err(err)?);
                if a != b {
                    return Err(format!("{name}：計れないのに正規化が変えた"));
                }
                continue;
            }
            let after = p
                .analyze::<LoudnessAnalysis>(engine, &out, &NoParams {})
                .map_err(|e| format!("{name}: {e}"))?;
            match after.integrated.value() {
                Some(v) if (v - target).abs() <= 0.1 => {}
                _ => {
                    return Err(format!(
                        "{name}：{target} に正規化した表示の積分ラウドネスが {:?}",
                        after.integrated
                    ));
                }
            }
        }
    }
    Ok(())
}

/// 前後の無音（`audio.silence`）とトリム（`audio.trim`）：解析の `content` で切ると、切ったものをもう一度（同じ閾値で）
/// 解析した前後の無音が 0 で、`content` が全体になる（将来の「無音のトリム」が同じ解析を申告して切る形）。クリップの列と
/// 1 を超える標本は音のある範囲の中にしかないので、切っても数が変わらない（閾値が −1 dBFS 以下のとき）。
fn trimming_to_the_content_leaves_no_silence(engine: &Engine, p: &Pair) -> Result<(), String> {
    let mut inputs: Vec<(String, Value)> = Vec::new();
    for (name, v) in p.analysis_inputs(engine)? {
        inputs.push((format!("{} の例 {name}", p.analysis), v));
    }
    for (period, seed) in [(5_000u64, 1u64), (12_345, 2)] {
        inputs.push((
            format!("衝撃 {period}"),
            InputGen::Bursts {
                rate: 22_050,
                channels: 2,
                frames: 40_000,
                period,
                seed,
            }
            .generate()
            .map_err(err)?,
        ));
    }
    // 左右で始まり・終わりが違い、前後に NaN と小さな音がある
    let mut l = vec![0.0f32; 3_000];
    let mut r = vec![0.0f32; 3_000];
    l[0] = f32::NAN;
    l[2_999] = f32::NAN;
    r[10] = 0.000_5;
    l[400] = 0.3;
    r[2_100] = -0.9;
    l[1_000..1_005].fill(1.0);
    r[1_500..1_503].fill(-1.0);
    l[2_000] = 2.0;
    inputs.push((
        "手で組んだ".into(),
        Value::Audio(Arc::new(
            Audio::new(8_000, vec![Arc::new(l), Arc::new(r)], AudioMeta::default()).map_err(err)?,
        )),
    ));
    let mut cut = 0;
    for (name, input) in &inputs {
        for db in [-120.0, -80.0, -60.0, -30.0, -1.0, 0.0] {
            let sp = SilenceParams { threshold_db: db };
            let s = p
                .analyze::<SilenceAnalysis>(engine, input, &sp)
                .map_err(|e| format!("{name}: {e}"))?;
            let Some(c) = s.content else {
                if s.lead != s.frames || s.trail != s.frames {
                    return Err(format!("{name} {db}：全部が無音なのに余白が長さと違う"));
                }
                continue;
            };
            let out = p
                .apply(engine, json!({ "start": c.start, "end": c.end }), input)
                .map_err(|e| format!("{name} {db}: {e}"))?;
            let t = p
                .analyze::<SilenceAnalysis>(engine, &out, &sp)
                .map_err(|e| format!("{name} {db}: {e}"))?;
            let len = c.end - c.start;
            if t.content != Some(FrameSpan { start: 0, end: len })
                || t.lead != 0
                || t.trail != 0
                || t.frames != len
            {
                return Err(format!(
                    "{name} {db}：{c:?} で切ったものの無音が 0 でない（{t:?}）"
                ));
            }
            if db <= -1.0 && (t.clip_runs != s.clip_runs || t.overs != s.overs) {
                return Err(format!(
                    "{name} {db}：切ったらクリップの数が変わった（{s:?} → {t:?}）"
                ));
            }
            cut += 1;
        }
    }
    if cut == 0 {
        return Err("切った例がない".into());
    }
    Ok(())
}

/// ループ点の設定（`audio.set_loop`）とループ点の解析（`audio.loop`）：設定した区間を読み、消せば null。
/// マーカーは設定で変わらない（解析の結果も同じ）。
fn set_loop_is_read_back(engine: &Engine, p: &Pair) -> Result<(), String> {
    let mut cases: Vec<(String, Value, serde_json::Value)> = Vec::new();
    for ex in p.examples(engine)? {
        cases.push((
            format!("例 {}", ex.name),
            ex.inputs[0].generate().map_err(err)?,
            ex.params.clone(),
        ));
    }
    let with_markers = InputGen::WithMeta {
        base: Box::new(InputGen::Sine {
            rate: 16_000,
            channels: 2,
            frames: 5_000,
            freq: 330,
            loop_range: Some(LoopRange { start: 1, end: 2 }),
        }),
        meta: MetaGen {
            markers: vec![(4_000, "b".into()), (10, "a".into()), (5_000, "外".into())],
            ..MetaGen::default()
        },
    }
    .generate()
    .map_err(err)?;
    let mut r = kura_engine::determinism::rng(77);
    for _ in 0..20 {
        let start = u64::from(r.next_u32() % 4_999);
        let end = start + 1 + u64::from(r.next_u32()) % (5_000 - start);
        cases.push((
            format!("乱数 {start}..{end}"),
            with_markers.clone(),
            json!({ "loop": { "start": start, "end": end } }),
        ));
    }
    cases.push(("消す".into(), with_markers.clone(), json!({ "loop": null })));
    cases.push((
        "全体".into(),
        with_markers,
        json!({ "loop": { "start": 0, "end": 5_000 } }),
    ));
    for (name, input, params) in cases {
        let out = p
            .apply(engine, params.clone(), &input)
            .map_err(|e| format!("{name}: {e}"))?;
        let got = p
            .analyze::<LoopAnalysis>(engine, &out, &NoParams {})
            .map_err(|e| format!("{name}: {e}"))?;
        let want = match &params["loop"] {
            serde_json::Value::Null => None,
            l => Some(FrameSpan {
                start: l["start"].as_u64().ok_or("start")?,
                end: l["end"].as_u64().ok_or("end")?,
            }),
        };
        if got.range != want || got.dropped_loop {
            return Err(format!(
                "{name}：設定 {want:?} と解析 {:?} が違う",
                got.range
            ));
        }
        let before = p
            .analyze::<LoopAnalysis>(engine, &input, &NoParams {})
            .map_err(|e| format!("{name}: {e}"))?;
        if got.markers != before.markers || got.dropped_markers != before.dropped_markers {
            return Err(format!("{name}：ループ点の設定でマーカーが変わった"));
        }
    }
    Ok(())
}
