//! 試験用の状態を運ぶ操作（M17 段 1。登録しない。docs/editing.md §18.1）：1 次の IIR `test.iir1` v1 と、その状態の表の
//! 内部用の解析 `test.iir1_state` v1。状態の表・表から区間を描く核（`audio::state`）と、実行器の流し解き・範囲の描画・
//! 流れ・保存を、実物の EQ（段 2）の前に確かめるためのもの。
//!
//! - 組（独立なら各チャンネル、`link` なら全チャンネルが 1 組）ごとに、入力 x を食べるたびに
//!   u = x ＋（link なら 0.5 · 組の平均）、v = b0·u + b1·u₋₁ − a1·v₋₁（f64）。
//! - 出力 i = gain · (wet · v(i + L) + (1 − wet) · x(i))。v(i + L) は入力 i + L を食べたときの v、x(i) は遅らせた入力で、
//!   先読みの列（L 個。絶対位置 p の入力を p mod L に置く）として状態に持つ（M18 のリミッターの形の下地）。入力の終わりの
//!   後ろは 0。
//! - 状態は組の各チャンネルの (u₋₁, v₋₁, 先読みの列) で、表の解析の引数は状態に効くもの（b0・b1・a1・lookahead・link・
//!   grid）だけ。wet・gain は表に入れない（それだけを動かしても表を解き直さない）。
//! - wet が 0 で gain が 0 dB なら入力のまま（`Identity`。解析を申告しない）。
//! - `grid` は表の格子（実物は `STATE_GRID`）。`AUDIO_CHUNK` を割り切らない・より大きい格子を試すために選べる。
//! - [`reference`] は全体を先頭から順に流す素朴な参照（テストの正。`audio::state` を使わずに書いた）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::{
    Analysis, AnalysisExample, AnalysisRegistry, AnalyzeCx, AudioStream, Estimate, Need,
};
use crate::audio::state::{
    STATE_GRID, StateTable, Stateful, StatefulKernel, TableBuilder, stateful_work, table_estimate,
    table_stream,
};
use crate::audio::{AudioKernel, Identity, render_all};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::limits::Limits;
use crate::mapping::{SpaceMap, TimeMap};
use crate::math;
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Registry, Signature, Unit, Work, expect_inputs, spec,
};
use crate::ops::util::{audio_of, audio_shape};
use crate::render::Engine;
use crate::render::region::{Area, Reach};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::{InputGen, NonFiniteSample};
use crate::value::{AUDIO_CHUNK, Audio, Value};

/// 操作の ID。
pub const OP_ID: &str = "test.iir1";

/// 表を解いた回数（状態に効く引数の正規の JSON ごと。`run` と、流して解き終えた数）。テストが「表を何回解いたか」を
/// 確かめるのに使う（流し解きの数は、窓が全体を覆うと値に当てて解く道になるので数えにくい）。テストは引数を変えて
/// 互いに混ざらないようにする。
static SOLVES: std::sync::Mutex<std::collections::BTreeMap<String, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn solve_key(p: &Iir1StateParams) -> String {
    serde_json::to_string(p).unwrap_or_default()
}

fn count_solve(p: &Iir1StateParams) {
    if let Ok(mut m) = SOLVES.lock() {
        *m.entry(solve_key(p)).or_default() += 1;
    }
}

/// 引数 `p` の表を解いた回数（このプロセスで）。
pub fn solves(p: &Iir1StateParams) -> u64 {
    SOLVES
        .lock()
        .map(|m| m.get(&solve_key(p)).copied().unwrap_or(0))
        .unwrap_or(0)
}

/// 解き終えたら数える流れ（`Analysis::stream` の包み）。
struct Counted {
    inner: Box<dyn AudioStream>,
    params: Iir1StateParams,
}

impl AudioStream for Counted {
    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        cx: &AnalyzeCx<'_>,
    ) -> Result<(), EngineError> {
        self.inner.feed(start, planes, cx)
    }

    fn finish(
        self: Box<Self>,
        end: &crate::audio::stream::StreamEnd,
        cx: &AnalyzeCx<'_>,
    ) -> Result<crate::analysis::AnyOutput, EngineError> {
        let out = self.inner.finish(end, cx)?;
        count_solve(&self.params);
        Ok(out)
    }
}
/// 表の解析の ID。
pub const STATE_ID: &str = "test.iir1_state";

/// 状態に効く引数（表の解析の引数）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Iir1StateParams {
    pub b0: f64,
    pub b1: f64,
    pub a1: f64,
    /// 先読み（フレーム）。
    pub lookahead: u64,
    /// 全チャンネルを 1 組にして平均を混ぜる。
    pub link: bool,
    /// 表の格子（フレーム）。
    pub grid: u64,
}

impl Default for Iir1StateParams {
    fn default() -> Self {
        Self {
            b0: 0.3,
            b1: 0.2,
            a1: -0.6,
            lookahead: 0,
            link: false,
            grid: STATE_GRID,
        }
    }
}

/// 状態に効く引数の記述（操作と解析で同じ）。
fn state_specs() -> Vec<crate::op::ParamSpec> {
    let coef = || ParamKind::Float {
        min: -2.0,
        max: 2.0,
        step: 0.001,
    };
    vec![
        spec("b0", coef(), Unit::None, json!(0.3)),
        spec("b1", coef(), Unit::None, json!(0.2)),
        spec(
            "a1",
            ParamKind::Float {
                min: -0.999,
                max: 0.999,
                step: 0.001,
            },
            Unit::None,
            json!(-0.6),
        ),
        spec(
            "lookahead",
            ParamKind::Int {
                min: 0,
                max: 8_192,
                step: 1,
            },
            Unit::Samples,
            json!(0),
        ),
        spec("link", ParamKind::Bool, Unit::None, json!(false)),
        spec(
            "grid",
            ParamKind::Int {
                min: 1,
                max: 1 << 20,
                step: 1,
            },
            Unit::Samples,
            json!(STATE_GRID),
        ),
    ]
}

impl OpParams for Iir1StateParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: state_specs(),
        }
    }
}

/// 操作の引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Iir1Params {
    pub b0: f64,
    pub b1: f64,
    pub a1: f64,
    pub lookahead: u64,
    pub link: bool,
    pub grid: u64,
    /// 混ぜる割合（0〜1。0 なら IIR を使わない）。
    pub wet: f64,
    /// 出力の利得（dB）。
    pub gain_db: f64,
}

impl Iir1Params {
    /// 状態に効く引数。
    pub fn state(&self) -> Iir1StateParams {
        Iir1StateParams {
            b0: self.b0,
            b1: self.b1,
            a1: self.a1,
            lookahead: self.lookahead,
            link: self.link,
            grid: self.grid,
        }
    }

    /// 何も変えない引数か（wet 0・gain 0 dB）。
    pub fn is_identity(&self) -> bool {
        self.wet == 0.0 && self.gain_db == 0.0
    }
}

impl OpParams for Iir1Params {
    fn schema() -> ParamSchema {
        let mut params = state_specs();
        params.push(spec(
            "wet",
            ParamKind::Float {
                min: 0.0,
                max: 1.0,
                step: 0.01,
            },
            Unit::None,
            json!(1.0),
        ));
        params.push(spec(
            "gainDb",
            ParamKind::Float {
                min: -24.0,
                max: 24.0,
                step: 0.1,
            },
            Unit::Db,
            json!(0.0),
        ));
        ParamSchema { params }
    }
}

/// 状態を運ぶ処理（`audio::state::Stateful`）。
pub struct Iir1 {
    channels: usize,
    p: Iir1StateParams,
    wet: f64,
    gain: f64,
}

impl Iir1 {
    /// 処理を作る（`wet`・`gain` は出力だけに効く。表を作るときは何でもよい）。
    pub fn new(channels: usize, p: &Iir1StateParams, wet: f64, gain: f64) -> Self {
        Self {
            channels,
            p: p.clone(),
            wet,
            gain,
        }
    }

    fn width(&self) -> usize {
        if self.p.link { self.channels.max(1) } else { 1 }
    }

    fn per_channel(&self) -> usize {
        2 + self.p.lookahead as usize
    }
}

impl Stateful for Iir1 {
    fn channels(&self) -> usize {
        self.channels
    }

    fn lane_width(&self) -> usize {
        self.width()
    }

    fn state_len(&self) -> usize {
        self.width() * self.per_channel()
    }

    fn after(&self) -> u64 {
        self.p.lookahead
    }

    fn init(&self, _lane: usize, st: &mut [f64]) {
        st.fill(0.0);
    }

    fn run(
        &self,
        _lane: usize,
        st: &mut [f64],
        pos: u64,
        input: &[&[f32]],
        mut out: Option<&mut [&mut [f32]]>,
    ) {
        let w = input.len();
        let per = self.per_channel();
        let l = self.p.lookahead;
        let m = input.first().map_or(0, |x| x.len());
        for t in 0..m {
            let p = pos + t as u64;
            let mean = if self.p.link {
                let mut s = 0.0f64;
                for x in input {
                    s += f64::from(x[t]);
                }
                s / w as f64
            } else {
                0.0
            };
            for c in 0..w {
                let s = &mut st[c * per..(c + 1) * per];
                let x = f64::from(input[c][t]);
                let u = if self.p.link { x + 0.5 * mean } else { x };
                let v = self.p.b0 * u + self.p.b1 * s[0] - self.p.a1 * s[1];
                s[0] = u;
                s[1] = v;
                let delayed = if l == 0 {
                    x
                } else {
                    let slot = 2 + (p % l) as usize;
                    let d = s[slot];
                    s[slot] = x;
                    d
                };
                if let Some(o) = out.as_mut() {
                    o[c][t] = (self.gain * (self.wet * v + (1.0 - self.wet) * delayed)) as f32;
                }
            }
        }
    }
}

/// 表の解析（内部用）。
pub struct Iir1State;

impl Analysis for Iir1State {
    const ID: &'static str = STATE_ID;
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
    const NUMERIC: Numeric = Numeric::Float;
    const INTERNAL: bool = true;
    type Params = Iir1StateParams;
    type Output = StateTable;

    fn run(p: &Iir1StateParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<StateTable, EngineError> {
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(format!(
                "{STATE_ID} の入力が音声ではありません"
            )));
        };
        let t = TableBuilder::solve(
            Arc::new(Iir1::new(a.channels(), p, 1.0, 1.0)),
            p.grid,
            v,
            cx,
        )?;
        count_solve(p);
        Ok(t)
    }

    fn stream(p: &Iir1StateParams, input: &Shape) -> Option<Box<dyn AudioStream>> {
        let Shape::Audio { channels, .. } = *input else {
            return None;
        };
        let inner = table_stream(
            Arc::new(Iir1::new(usize::from(channels), p, 1.0, 1.0)),
            p.grid,
            input,
        )?;
        Some(Box::new(Counted {
            inner,
            params: p.clone(),
        }))
    }

    fn examples() -> Vec<AnalysisExample<Iir1StateParams>> {
        Iir1Op::examples()
            .into_iter()
            .filter(|e| !e.params.is_identity())
            .map(|e| AnalysisExample {
                name: e.name,
                params: e.params.state(),
                input: e.inputs[0].clone(),
            })
            .collect()
    }

    fn estimate(p: &Iir1StateParams, input: &Shape) -> Estimate {
        let Shape::Audio { channels, .. } = *input else {
            return Estimate::default();
        };
        let m = Iir1::new(usize::from(channels), p, 1.0, 1.0);
        let lanes = (usize::from(channels) / m.width().max(1)) as u64;
        table_estimate(p.grid, input, lanes, m.state_len() as u64, 8)
    }
}

/// 操作。
pub struct Iir1Op;

impl Iir1Op {
    /// 核（表を申告して受け取る。何も変えない引数は `Identity`）。
    fn kernel(
        p: &Iir1Params,
        input: &Value,
        cx: &mut ApplyCx<'_>,
    ) -> Result<Box<dyn AudioKernel>, EngineError> {
        let audio = audio_of(input, Self::ID)?;
        if p.is_identity() {
            return Ok(Box::new(Identity(audio.clone())));
        }
        let table = cx.analysis::<Iir1State>(0, input, &p.state())?;
        if table.non_finite() {
            cx.warn(Issue::warning(
                IssueCode::NonFiniteSamples,
                "NaN・∞ の標本を 0 としてフィルタに通し、その位置は元の値のまま出しました",
            ));
        }
        let m = Iir1::new(
            audio.channels(),
            &p.state(),
            p.wet,
            math::db_to_gain(p.gain_db),
        );
        Ok(Box::new(StatefulKernel::new(
            Arc::new(m),
            p.grid,
            table,
            audio.clone(),
        )?))
    }

    /// 長さの決まった音声の形なら（チャンネルの数, 長さ）。
    fn exact(inputs: &[Shape]) -> Option<(usize, u64)> {
        match inputs.first().and_then(audio_shape) {
            Some((_, ch, Dim::Exact(n))) => Some((usize::from(ch), n)),
            _ => None,
        }
    }
}

impl Op for Iir1Op {
    const ID: &'static str = OP_ID;
    const VERSION: u16 = 1;
    type Params = Iir1Params;
    const NEEDS: &'static [(&'static str, u16)] = &[(STATE_ID, 1)];

    fn needs(p: &Iir1Params, _inputs: &[Shape]) -> Result<Vec<Need>, Issues> {
        if p.is_identity() {
            return Ok(Vec::new());
        }
        Ok(vec![Need::of::<Iir1State>(0, &p.state())?])
    }

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "audio",
                accepts: &[ValueKind::Audio],
            }],
            lift: Lift::None,
            numeric: Numeric::Float,
            meta: MetaAccess::Map,
        }
    }

    fn infer(_p: &Iir1Params, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((_, channels, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: frames
                .max()
                .saturating_mul(u64::from(channels))
                .saturating_mul(8),
        })
    }

    fn apply(
        p: &Iir1Params,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let audio = audio_of(inputs[0], Self::ID)?;
        if p.is_identity() {
            return Ok(Applied::value(Value::Audio(audio.clone())));
        }
        let k = Self::kernel(p, inputs[0], cx)?;
        Ok(Applied::value(render_all(
            k.as_ref(),
            audio.meta.clone(),
            cx.mon,
        )?))
    }

    fn audio_kernel(
        p: &Iir1Params,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(Self::kernel(p, inputs[0], cx))
    }

    fn work_bytes(p: &Self::Params, inputs: &[Shape], out: &Shape) -> Work {
        let channels = inputs
            .first()
            .and_then(audio_shape)
            .map_or(1, |(_, c, _)| usize::from(c));
        let m = Iir1::new(channels, &p.state(), 1.0, 1.0);
        let w = m.width().max(1);
        stateful_work(out, w as u64, (channels / w) as u64, m.state_len() as u64)
    }

    fn reach(p: &Iir1Params, inputs: &[Shape]) -> Reach {
        match (p.is_identity(), Self::exact(inputs)) {
            (true, _) => Reach::SameTime,
            (false, Some((_, n))) => Reach::Prefix {
                after: p.lookahead,
                frames: n,
            },
            _ => Reach::Global,
        }
    }

    fn audio_reach(p: &Iir1Params, inputs: &[Shape]) -> Reach {
        match (p.is_identity(), Self::exact(inputs)) {
            (true, _) => Reach::SameTime,
            (false, Some((_, n))) => Reach::Aligned {
                grid: p.grid,
                before: 0,
                after: p.lookahead,
                frames: n,
            },
            _ => Reach::Global,
        }
    }

    fn audio_length_change(
        old: &Iir1Params,
        new: &Iir1Params,
        _old_in: &[Shape],
        _new_in: &[Shape],
    ) -> Option<Area> {
        (old == new).then_some(Area::Empty)
    }

    fn examples() -> Vec<Example<Iir1Params>> {
        let base = Iir1Params {
            b0: 0.3,
            b1: 0.2,
            a1: -0.6,
            lookahead: 0,
            link: false,
            grid: STATE_GRID,
            wet: 1.0,
            gain_db: 0.0,
        };
        let noise = |rate, channels, frames, seed| InputGen::AudioNoise {
            rate,
            channels,
            frames,
            seed,
        };
        vec![
            Example {
                name: "plain-stereo",
                params: base.clone(),
                inputs: vec![noise(48_000, 2, 20_000, 81)],
            },
            // 先読み・リンク・C を割り切らない格子（3·4,096）・混ぜる割合と利得
            Example {
                name: "lookahead-link-small-grid",
                params: Iir1Params {
                    lookahead: 37,
                    link: true,
                    grid: 3 * 4_096,
                    wet: 0.7,
                    gain_db: -3.0,
                    a1: -0.9,
                    ..base.clone()
                },
                inputs: vec![noise(8_000, 2, 40_000, 82)],
            },
            // C より大きい格子（2C）でチャンクの境をまたぐ
            Example {
                name: "large-grid-crosses-chunks",
                params: Iir1Params {
                    lookahead: 5,
                    grid: 2 * AUDIO_CHUNK as u64,
                    ..base.clone()
                },
                inputs: vec![noise(8_000, 2, 140_001, 83)],
            },
            // 実物の格子（C）でチャンクの境をまたぐ
            Example {
                name: "state-grid-crosses-chunks",
                params: Iir1Params {
                    a1: 0.5,
                    ..base.clone()
                },
                inputs: vec![noise(8_000, 1, 131_109, 84)],
            },
            // 格子 1（すべてのフレームに状態）
            Example {
                name: "grid-one",
                params: Iir1Params {
                    grid: 1,
                    lookahead: 3,
                    ..base.clone()
                },
                inputs: vec![noise(22_050, 1, 3_000, 85)],
            },
            // 先読みが入力より長い（終わりの後ろの 0 で流し切る）
            Example {
                name: "lookahead-past-the-end",
                params: Iir1Params {
                    lookahead: 4_096,
                    grid: 1_000,
                    link: true,
                    ..base.clone()
                },
                inputs: vec![noise(44_100, 3, 3_000, 86)],
            },
            // NaN・∞ を含む：0 としてフィルタに通し、その位置は元の値のまま（警告）
            Example {
                name: "non-finite-samples",
                params: Iir1Params {
                    grid: 4_000,
                    lookahead: 2,
                    ..base.clone()
                },
                inputs: vec![InputGen::NonFinite {
                    base: Box::new(noise(8_000, 2, 9_000, 87)),
                    at: vec![
                        (0, 10, NonFiniteSample::Nan),
                        (1, 4_000, NonFiniteSample::Inf),
                        (0, 8_999, NonFiniteSample::NegInf),
                    ],
                }],
            },
            // 何も変えない（Identity。申告しない）
            Example {
                name: "identity",
                params: Iir1Params { wet: 0.0, ..base },
                inputs: vec![noise(8_000, 2, 5_000, 88)],
            },
        ]
    }
}

/// 試験用のエンジン：今の登録表に `test.iir1` と `test.iir1_state` を足したもの。
pub fn engine() -> Result<Engine, EngineError> {
    engine_with(Limits::default())
}

/// 上限を指定した試験用のエンジン。
pub fn engine_with(limits: Limits) -> Result<Engine, EngineError> {
    let (ops, analyses) = registries()?;
    Engine::with_registries(ops, analyses, limits)
}

/// 試験用の登録表（今のもの ＋ 試験の操作と解析）。
pub fn registries() -> Result<(Registry, AnalysisRegistry), EngineError> {
    let mut ops = crate::ops::registry().map_err(EngineError::Internal)?;
    ops.add_current::<Iir1Op>().map_err(EngineError::Internal)?;
    let mut analyses = crate::analysis::registry().map_err(EngineError::Internal)?;
    analyses
        .add_current::<Iir1State>()
        .map_err(EngineError::Internal)?;
    Ok((ops, analyses))
}

/// 素朴な参照（テストの正）：全体を先頭から順に 1 標本ずつ流す。`audio::state` を使わない。有限でない入力は 0 として食べ、
/// 出力のその位置は元の値。格子の境（kG + L）を食べる前に状態の NaN を揃える（表と同じ）。
pub fn reference(p: &Iir1Params, a: &Audio) -> Result<Vec<Vec<f32>>, EngineError> {
    let channels = a.channels();
    let n = a.frames();
    let mut x = vec![vec![0f32; n]; channels];
    for (ch, plane) in x.iter_mut().enumerate() {
        a.read(ch, 0, plane)?;
    }
    if p.is_identity() {
        return Ok(x);
    }
    let gain = math::db_to_gain(p.gain_db);
    let w = if p.link { channels.max(1) } else { 1 };
    let l = p.lookahead as usize;
    let grid = p.grid.max(1) as usize;
    let mut out = vec![vec![0f32; n]; channels];
    for lane in 0..channels / w {
        let chans: Vec<usize> = (lane * w..(lane + 1) * w).collect();
        let mut prev_u = vec![0f64; w];
        let mut prev_v = vec![0f64; w];
        let mut ring = vec![vec![0f64; l.max(1)]; w];
        // 入力 q を食べる（q ≥ n は 0）。出力 q − L を書く
        for q in 0..n + l {
            if q >= l && (q - l).is_multiple_of(grid) {
                for v in prev_u
                    .iter_mut()
                    .chain(prev_v.iter_mut())
                    .chain(ring.iter_mut().flatten())
                {
                    if v.is_nan() {
                        *v = f64::from_bits(crate::audio::state::CANONICAL_NAN);
                    }
                }
            }
            let xs: Vec<f64> = chans
                .iter()
                .map(|&c| {
                    let v = if q < n { x[c][q] } else { 0.0 };
                    if v.is_finite() { f64::from(v) } else { 0.0 }
                })
                .collect();
            let mean = if p.link {
                let mut s = 0.0f64;
                for v in &xs {
                    s += *v;
                }
                s / w as f64
            } else {
                0.0
            };
            for c in 0..w {
                let u = if p.link { xs[c] + 0.5 * mean } else { xs[c] };
                let v = p.b0 * u + p.b1 * prev_u[c] - p.a1 * prev_v[c];
                prev_u[c] = u;
                prev_v[c] = v;
                let delayed = if l == 0 {
                    xs[c]
                } else {
                    let slot = q % l;
                    let d = ring[c][slot];
                    ring[c][slot] = xs[c];
                    d
                };
                if q >= l {
                    let i = q - l;
                    let raw = x[chans[c]][i];
                    out[chans[c]][i] = if raw.is_finite() {
                        (gain * (p.wet * v + (1.0 - p.wet) * delayed)) as f32
                    } else {
                        raw
                    };
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::AnalysisOutput;
    use crate::monitor::NoMonitor;

    fn audio(v: &Value) -> Arc<Audio> {
        match v {
            Value::Audio(a) => a.clone(),
            _ => panic!("音声ではありません"),
        }
    }

    fn planes(a: &Audio) -> Vec<Vec<f32>> {
        (0..a.channels())
            .map(|ch| {
                let mut p = vec![0f32; a.frames()];
                a.read(ch, 0, &mut p).unwrap();
                p
            })
            .collect()
    }

    fn same(a: &[Vec<f32>], b: &[Vec<f32>]) -> bool {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits())
            })
    }

    /// すべての例で、操作の値（試験用のエンジンが表を解いて核で描いたもの）が素朴な参照とビット一致。NaN・∞ の例は
    /// 警告を出す。
    #[test]
    fn examples_match_the_reference() {
        let engine = engine().unwrap();
        for ex in Iir1Op::examples() {
            let input = ex.inputs[0].generate().unwrap();
            let graph = crate::testing::single_node_graph(
                OP_ID,
                1,
                serde_json::to_value(&ex.params).unwrap(),
            )
            .unwrap();
            let r = crate::testing::render_full(
                &engine,
                &graph,
                input.clone(),
                crate::digest::Digest::default(),
                &NoMonitor,
            )
            .unwrap();
            let want = reference(&ex.params, &audio(&input)).unwrap();
            assert!(
                same(&planes(&audio(&r.outputs[0].value)), &want),
                "{}",
                ex.name
            );
            let warned = r
                .warnings
                .iter()
                .any(|w| w.code == IssueCode::NonFiniteSamples);
            assert_eq!(warned, ex.name == "non-finite-samples", "{}", ex.name);
        }
    }

    /// 表の正規の形は往復し、流して作った表（窓の切り方・上限の形）が `run` と同じ。
    #[test]
    fn tables_round_trip_and_stream_the_same() {
        let limits = Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        for ex in Iir1State::examples() {
            let v = ex.input.generate().unwrap();
            let t = Iir1State::run(&ex.params, &v, &cx).unwrap();
            let bytes = t.to_canonical().unwrap();
            assert_eq!(
                StateTable::from_canonical(&bytes).unwrap(),
                t,
                "{}",
                ex.name
            );
            let a = audio(&v);
            let n = a.frames() as u64;
            for (shape, win) in [
                (Dim::Exact(n), 1usize),
                (Dim::Exact(n), 7),
                (Dim::AtMost(n + 9), 4_096),
                (Dim::Exact(n), 1 << 20),
            ] {
                let s = Shape::Audio {
                    rate: a.rate(),
                    channels: a.channels() as u16,
                    frames: shape,
                };
                // 1 フレームずつは長い例で重いので、短い例だけ
                if win == 1 && n > 5_000 {
                    continue;
                }
                let mut st = Iir1State::stream(&ex.params, &s).unwrap();
                let p = planes(&a);
                let mut at = 0usize;
                while at < a.frames() {
                    let end = (at + win).min(a.frames());
                    let refs: Vec<&[f32]> = p.iter().map(|q| &q[at..end]).collect();
                    st.feed(at as u64, &refs, &cx).unwrap();
                    at = end;
                }
                let end = crate::audio::stream::StreamEnd {
                    frames: n,
                    meta: a.meta.clone(),
                };
                let got = st.finish(&end, &cx).unwrap();
                let got = got.downcast_ref::<StateTable>().unwrap();
                assert_eq!(got.to_canonical().unwrap(), bytes, "{} 窓 {win}", ex.name);
            }
        }
    }
}
