//! `audio.eq` v1：イコライザ（最小位相。M17 段 2。docs/editing.md §18.5、ADR-231 (5)(6)・ADR-234）。
//!
//! - 引数はバンドの列（24 個まで）・位相（`minimum`。`linear` は段 7）・出力の利得。バンドは型（ベル・ローシェルフ・
//!   ハイシェルフ・ローカット・ハイカット・ノッチ・バンドパス・チルト）・周波数・利得・Q・傾き・チャンネル（すべて・L・R・
//!   M・S）・オン／オフ。2 次の段の数（カットは傾き ÷ 12 段、6 dB/oct の奇数倍は 1 次の段を 1 段と数える）は 48 まで。
//! - 推論の扱い（凍結）：周波数がナイキスト以上なら、ローカット・バンドパスは断り、ハイカット・ノッチは外して警告、ベル・
//!   シェルフ・チルトは 0.49 × レートに止めて警告。ナイキスト未満でも 0.49 × レートを超えれば止めて警告。モノラルでは S の
//!   バンドを外して警告し、L・R・M はそのチャンネルに当てる。3 チャンネル以上では L・R は 1・2 チャンネル目、M・S は外して
//!   警告。オフのバンドと、利得 0 dB のベル・シェルフ・チルトは段を作らない。段が 1 つも残らなければ、出力の利得だけの核
//!   （0 dB なら `Identity`）で、状態の表を申告しない。
//! - 式は `audio::biquad`（凍結）。形は DF-I（標本が外・段が内、f64、最後に 1 回 f32）。縦続の順はバンドの列の順（カットは
//!   Butterworth の段の順）。M/S のバンドは、L/R の領域と M/S の領域が替わる所でだけ変換する（M = (L+R)·0.5、S = (L−R)·0.5、
//!   戻しは L = M+S、R = M−S）。「すべて」は L/R の領域で各チャンネルに当てる。M/S のバンドがあれば 2 チャンネルを 1 組に、
//!   なければ各チャンネルを独立に流す。
//! - 状態は内部用の解析 `audio.eq_state` v1 の表（`audio::state`。引数はバンドの列だけで、出力の利得は入れない）。核は表から
//!   区間を描く（`StatefulKernel`）。有限でない入力は 0 としてフィルタに通し、その位置は元の値（警告 `NonFiniteSamples`）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::Need;
use crate::analysis::audio::eq_state::{EqState, EqStateParams};
use crate::audio::biquad::{self, Section};
use crate::audio::state::{STATE_GRID, Stateful, StatefulKernel, stateful_work};
use crate::audio::{AudioKernel, Identity, render_all};
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::mapping::{SpaceMap, TimeMap};
use crate::op::{
    Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op, OpParams,
    ParamKind, ParamSchema, PortSpec, Signature, Unit, Work, expect_inputs, spec,
};
use crate::ops::audio::gain::GainKernel;
use crate::ops::util::{audio_of, audio_shape};
use crate::render::region::{Area, Reach};
use crate::shape::{Dim, Shape, ValueKind};
use crate::testgen::{InputGen, NonFiniteSample};
use crate::value::{AUDIO_CHUNK, Value};

/// バンドの数の上限。
pub const MAX_BANDS: usize = 24;
/// 2 次の段の数の上限（カットの 1 次の段も 1 段と数える）。
pub const MAX_SECTIONS: usize = 48;
/// 周波数の範囲（Hz）。
pub const FREQ_MIN: f64 = 10.0;
pub const FREQ_MAX: f64 = 30_000.0;
/// 利得の範囲（dB。バンドと出力）。
pub const GAIN_MAX: f64 = 30.0;
/// Q の範囲（ベル・ノッチ・バンドパス）と、シェルフの Q の上限。
pub const Q_MIN: f64 = 0.1;
pub const Q_MAX: f64 = 40.0;
pub const SHELF_Q_MAX: f64 = 4.0;
/// カットの傾き（dB/oct）。
pub const SLOPES: &[u32] = &[6, 12, 18, 24, 36, 48, 72, 96];
/// 周波数を止める上限（レートに対する比）。
pub const NYQUIST_GUARD: f64 = 0.49;

/// バンドの型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BandType {
    Bell,
    LowShelf,
    HighShelf,
    LowCut,
    HighCut,
    Notch,
    BandPass,
    Tilt,
}

impl BandType {
    /// 利得を使う型。
    pub fn has_gain(self) -> bool {
        matches!(
            self,
            Self::Bell | Self::LowShelf | Self::HighShelf | Self::Tilt
        )
    }
}

/// バンドのチャンネル。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BandChannel {
    #[default]
    All,
    Left,
    Right,
    Mid,
    Side,
}

/// 位相。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Minimum,
    Linear,
}

fn default_q() -> f64 {
    std::f64::consts::FRAC_1_SQRT_2
}

fn default_slope() -> u32 {
    12
}

fn yes() -> bool {
    true
}

/// バンド。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Band {
    #[serde(rename = "type")]
    pub kind: BandType,
    /// 周波数（Hz）。
    pub freq: f64,
    /// 利得（dB。ベル・シェルフ・チルト）。
    #[serde(default)]
    pub gain_db: f64,
    /// Q（ベル・ノッチ・バンドパス。シェルフは肩の形）。
    #[serde(default = "default_q")]
    pub q: f64,
    /// 傾き（dB/oct。カット）。
    #[serde(default = "default_slope")]
    pub slope: u32,
    #[serde(default)]
    pub channel: BandChannel,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Band {
    /// 段を作らないバンド（オフ・0 dB のベル・シェルフ・チルト）。推論の扱いで飛ばし、状態の表の引数にも入れない。
    pub fn is_inert(&self) -> bool {
        !self.enabled || (self.kind.has_gain() && self.gain_db == 0.0)
    }

    /// この型の 2 次の段の数（カットの 1 次の段も 1 段）。
    pub fn sections(&self) -> usize {
        match self.kind {
            BandType::LowCut | BandType::HighCut => {
                let order = (self.slope / 6).max(1) as usize;
                order / 2 + order % 2
            }
            _ => 1,
        }
    }
}

/// 引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EqParams {
    /// バンドの列（順に当てる）。
    pub bands: Vec<Band>,
    #[serde(default)]
    pub phase: Phase,
    /// 出力の利得（dB）。
    #[serde(default)]
    pub gain_db: f64,
}

impl OpParams for EqParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![
                spec("bands", ParamKind::EqBands, Unit::None, json!([])),
                spec(
                    "phase",
                    ParamKind::Enum {
                        values: &["minimum", "linear"],
                    },
                    Unit::None,
                    json!("minimum"),
                ),
                spec(
                    "gainDb",
                    ParamKind::Float {
                        min: -GAIN_MAX,
                        max: GAIN_MAX,
                        step: 0.1,
                    },
                    Unit::Db,
                    json!(0.0),
                ),
            ],
        }
    }
}

/// バンドの列の範囲の確かめ（`ParamSchema::check_ranges` が引数の JSON に当てる）：24 個まで、周波数・利得・Q・傾き・
/// 段の数。型・チャンネルの綴りと知らない項目は serde が断る。
pub fn bands_out_of_range(v: &serde_json::Value) -> bool {
    let Some(list) = v.as_array() else {
        return false;
    };
    if list.len() > MAX_BANDS {
        return true;
    }
    let num = |b: &serde_json::Value, k: &str| b.get(k).and_then(serde_json::Value::as_f64);
    let mut sections = 0usize;
    for b in list {
        let kind = b.get("type").and_then(serde_json::Value::as_str);
        let bad_f = |k: &str, lo: f64, hi: f64| {
            num(b, k).is_some_and(|x| !x.is_finite() || x < lo || x > hi)
        };
        let q_max = match kind {
            Some("lowShelf" | "highShelf") => SHELF_Q_MAX,
            _ => Q_MAX,
        };
        if bad_f("freq", FREQ_MIN, FREQ_MAX)
            || bad_f("gainDb", -GAIN_MAX, GAIN_MAX)
            || bad_f("q", Q_MIN, q_max)
        {
            return true;
        }
        let slope = b.get("slope").and_then(serde_json::Value::as_u64);
        if slope.is_some_and(|s| !SLOPES.iter().any(|x| u64::from(*x) == s)) {
            return true;
        }
        let enabled = b.get("enabled").and_then(serde_json::Value::as_bool) != Some(false);
        if enabled {
            sections += match kind {
                Some("lowCut" | "highCut") => {
                    let order = (slope.unwrap_or(12) / 6).max(1) as usize;
                    order / 2 + order % 2
                }
                _ => 1,
            };
        }
    }
    sections > MAX_SECTIONS
}

/// 当てる信号（L/R の領域のチャンネルか、M/S の領域の M・S）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// すべてのチャンネル（L/R の領域）。
    All,
    /// チャンネル 1 つ（L/R の領域）。
    Channel(usize),
    Mid,
    Side,
}

impl Target {
    fn ms(self) -> bool {
        matches!(self, Self::Mid | Self::Side)
    }
}

/// 推論を通したバンド：引数の番号・当てる信号・段の係数。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBand {
    pub index: usize,
    pub target: Target,
    pub sections: Vec<Section>,
}

/// 推論を通したイコライザ。
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub bands: Vec<ResolvedBand>,
    pub warnings: Vec<Issue>,
}

impl Resolved {
    /// M/S のバンドがあるか（2 チャンネルを 1 組で流す）。
    pub fn has_ms(&self) -> bool {
        self.bands.iter().any(|b| b.target.ms())
    }
}

/// 推論の扱い（凍結）でバンドを解く：レート `rate`・チャンネル `channels`。断るもの（ナイキスト以上のローカット・
/// バンドパス、直線位相）は Issues。
pub fn resolve(p: &EqParams, rate: u32, channels: usize) -> Result<Resolved, Issues> {
    if p.phase == Phase::Linear {
        return Err(Issues::one(
            Issue::error(
                IssueCode::BadParams,
                "直線位相のイコライザはまだ使えません（M17 段 7）",
            )
            .param("phase"),
        ));
    }
    let fs = f64::from(rate.max(1));
    let nyquist = 0.5 * fs;
    let guard = NYQUIST_GUARD * fs;
    let mut out = Resolved {
        bands: Vec::new(),
        warnings: Vec::new(),
    };
    let mut errors = Issues::none();
    for (index, b) in p.bands.iter().enumerate() {
        if b.is_inert() {
            continue;
        }
        let mut f = b.freq;
        if f >= nyquist {
            match b.kind {
                BandType::LowCut | BandType::BandPass => {
                    errors.push(
                        Issue::error(
                            IssueCode::OutOfRange,
                            format!(
                                "バンド {} の周波数 {f} Hz がナイキスト（{nyquist} Hz）以上です",
                                index + 1
                            ),
                        )
                        .param("bands"),
                    );
                    continue;
                }
                BandType::HighCut | BandType::Notch => {
                    out.warnings.push(Issue::warning(
                        IssueCode::BandDropped,
                        format!(
                            "バンド {} の周波数 {f} Hz がナイキスト以上なので外しました",
                            index + 1
                        ),
                    ));
                    continue;
                }
                _ => {}
            }
        }
        if f > guard {
            out.warnings.push(Issue::warning(
                IssueCode::FrequencyClamped,
                format!(
                    "バンド {} の周波数 {f} Hz を {guard} Hz に止めました",
                    index + 1
                ),
            ));
            f = guard;
        }
        let target = match (b.channel, channels) {
            (BandChannel::All, _) => Target::All,
            (BandChannel::Side, 1) | (BandChannel::Mid | BandChannel::Side, 3..) => {
                out.warnings.push(Issue::warning(
                    IssueCode::BandDropped,
                    format!(
                        "バンド {} の M/S は {channels} チャンネルの音声に当てられないので外しました",
                        index + 1
                    ),
                ));
                continue;
            }
            (_, 1) => Target::All,
            (BandChannel::Left, _) => Target::Channel(0),
            (BandChannel::Right, _) => Target::Channel(1),
            (BandChannel::Mid, _) => Target::Mid,
            (BandChannel::Side, _) => Target::Side,
        };
        let w0 = 2.0 * std::f64::consts::PI * f / fs;
        let sections = match b.kind {
            BandType::Bell => vec![biquad::peaking(w0, b.q, b.gain_db)],
            BandType::LowShelf => vec![biquad::shelf(w0, b.q, b.gain_db, fs, false)],
            BandType::HighShelf => vec![biquad::shelf(w0, b.q, b.gain_db, fs, true)],
            BandType::LowCut => biquad::cut(w0, b.slope, false),
            BandType::HighCut => biquad::cut(w0, b.slope, true),
            BandType::Notch => vec![biquad::notch(w0, b.q)],
            BandType::BandPass => vec![biquad::bandpass(w0, b.q)],
            BandType::Tilt => vec![biquad::tilt(w0, b.gain_db)],
        };
        out.bands.push(ResolvedBand {
            index,
            target,
            sections,
        });
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// 段 1 つの位置（組の中の信号の番号と、状態の語の始まり）。
#[derive(Debug, Clone, Copy)]
struct Slot {
    section: Section,
    /// 組の中の信号の番号（L/R の領域ならチャンネル、M/S の領域なら 0 = M・1 = S）。
    signal: usize,
    /// 状態（x1, x2, y1, y2）の始まり。
    at: usize,
}

/// バンド 1 つの、組の中での段の並び（領域と段）。
#[derive(Debug, Clone)]
struct LaneBand {
    ms: bool,
    slots: Vec<Slot>,
}

/// 状態を運ぶイコライザ（`audio::state::Stateful`）。
pub struct EqMachine {
    channels: usize,
    width: usize,
    /// 組ごとのバンドの並び。
    lanes: Vec<Vec<LaneBand>>,
    state_len: usize,
    gain: f64,
}

impl EqMachine {
    /// 推論を通したバンドから作る（`gain` は出力の利得の倍率。表を作るときは何でもよい）。
    pub fn new(r: &Resolved, channels: usize, gain: f64) -> Self {
        let width = if r.has_ms() && channels == 2 { 2 } else { 1 };
        let lane_count = channels / width.max(1);
        let mut lanes = Vec::with_capacity(lane_count);
        let mut state_len = 0;
        for lane in 0..lane_count {
            let mut at = 0usize;
            let mut list = Vec::new();
            for b in &r.bands {
                let signals: Vec<usize> = match (width, b.target) {
                    (2, Target::All) => vec![0, 1],
                    (2, Target::Channel(c)) => vec![c],
                    (2, Target::Mid) => vec![0],
                    (2, Target::Side) => vec![1],
                    (_, Target::All) => vec![0],
                    (_, Target::Channel(c)) if c == lane => vec![0],
                    _ => Vec::new(),
                };
                if signals.is_empty() {
                    continue;
                }
                let mut slots = Vec::new();
                for &signal in &signals {
                    for s in &b.sections {
                        slots.push(Slot {
                            section: *s,
                            signal,
                            at,
                        });
                        at += 4;
                    }
                }
                list.push(LaneBand {
                    ms: b.target.ms(),
                    slots,
                });
            }
            state_len = state_len.max(at);
            lanes.push(list);
        }
        Self {
            channels,
            width,
            lanes,
            state_len,
            gain,
        }
    }
}

/// DF-I の 1 標本（凍結した演算の順）。
#[inline]
fn step(s: &Section, st: &mut [f64], x: f64) -> f64 {
    let y = s.b0 * x + s.b1 * st[0] + s.b2 * st[1] - s.a1 * st[2] - s.a2 * st[3];
    st[1] = st[0];
    st[0] = x;
    st[3] = st[2];
    st[2] = y;
    y
}

impl Stateful for EqMachine {
    fn channels(&self) -> usize {
        self.channels
    }

    fn lane_width(&self) -> usize {
        self.width
    }

    fn state_len(&self) -> usize {
        self.state_len
    }

    fn after(&self) -> u64 {
        0
    }

    fn init(&self, _lane: usize, st: &mut [f64]) {
        st.fill(0.0);
    }

    fn run(
        &self,
        lane: usize,
        st: &mut [f64],
        _pos: u64,
        input: &[&[f32]],
        mut out: Option<&mut [&mut [f32]]>,
    ) {
        let Some(bands) = self.lanes.get(lane) else {
            return;
        };
        let m = input.first().map_or(0, |x| x.len());
        let w = self.width;
        let mut v = [0f64; 2];
        for t in 0..m {
            for (c, x) in input.iter().enumerate().take(w) {
                v[c] = f64::from(x[t]);
            }
            let mut ms = false;
            for b in bands {
                if w == 2 && b.ms != ms {
                    let (x0, x1) = (v[0], v[1]);
                    if b.ms {
                        v[0] = (x0 + x1) * 0.5;
                        v[1] = (x0 - x1) * 0.5;
                    } else {
                        v[0] = x0 + x1;
                        v[1] = x0 - x1;
                    }
                    ms = b.ms;
                }
                for slot in &b.slots {
                    let x = v[slot.signal];
                    v[slot.signal] = step(&slot.section, &mut st[slot.at..slot.at + 4], x);
                }
            }
            if ms {
                let (x0, x1) = (v[0], v[1]);
                v[0] = x0 + x1;
                v[1] = x0 - x1;
            }
            if let Some(o) = out.as_mut() {
                for (c, plane) in o.iter_mut().enumerate().take(w) {
                    plane[t] = (v[c] * self.gain) as f32;
                }
            }
        }
    }
}

/// 曲線（`edit_eq_curve`。描かずに求める）：周波数 `freqs`（Hz）ごとの、バンドごとの利得（dB）と合成（全バンドの和 ＋ 出力の
/// 利得。チャンネルは分けない）、バンドの段の係数。推論の扱いは操作と同じ。
#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    pub total: Vec<f64>,
    pub bands: Vec<BandCurve>,
}

/// バンド 1 つの曲線。
#[derive(Debug, Clone, PartialEq)]
pub struct BandCurve {
    pub index: usize,
    pub db: Vec<f64>,
    pub sections: Vec<Section>,
}

/// 曲線を求める。
pub fn curve(p: &EqParams, rate: u32, channels: usize, freqs: &[f64]) -> Result<Curve, Issues> {
    let r = resolve(p, rate, channels)?;
    let fs = f64::from(rate.max(1));
    let w: Vec<f64> = freqs
        .iter()
        .map(|f| {
            2.0 * std::f64::consts::PI * crate::math::min(crate::math::max(*f, 0.0), 0.5 * fs) / fs
        })
        .collect();
    let mut total = vec![p.gain_db; freqs.len()];
    let mut bands = Vec::with_capacity(r.bands.len());
    for b in &r.bands {
        let db: Vec<f64> = w
            .iter()
            .map(|ww| b.sections.iter().map(|s| s.db(*ww)).sum::<f64>())
            .collect();
        for (t, d) in total.iter_mut().zip(&db) {
            *t += d;
        }
        bands.push(BandCurve {
            index: b.index,
            db,
            sections: b.sections.clone(),
        });
    }
    Ok(Curve { total, bands })
}

/// イコライザ。
pub struct Eq;

impl Eq {
    /// 入力の形のレート・チャンネル（音声でなければ None）。
    fn rate_channels(inputs: &[Shape]) -> Option<(u32, usize)> {
        inputs
            .first()
            .and_then(audio_shape)
            .map(|(rate, ch, _)| (rate, usize::from(ch)))
    }

    /// 段が残るか（なければ申告しない）。
    fn has_sections(p: &EqParams, inputs: &[Shape]) -> bool {
        Self::rate_channels(inputs)
            .and_then(|(rate, ch)| resolve(p, rate, ch).ok())
            .is_some_and(|r| !r.bands.is_empty())
    }

    fn kernel(
        p: &EqParams,
        input: &Value,
        cx: &mut ApplyCx<'_>,
    ) -> Result<Box<dyn AudioKernel>, EngineError> {
        let audio = audio_of(input, Self::ID)?;
        let r = resolve(p, audio.rate(), audio.channels()).map_err(EngineError::Invalid)?;
        for w in &r.warnings {
            cx.warn(w.clone());
        }
        if r.bands.is_empty() {
            return Ok(if p.gain_db == 0.0 {
                Box::new(Identity(audio.clone()))
            } else {
                Box::new(GainKernel::new(audio.clone(), p.gain_db))
            });
        }
        let table = cx.analysis::<EqState>(0, input, &EqStateParams::of(p))?;
        if table.non_finite() {
            cx.warn(Issue::warning(
                IssueCode::NonFiniteSamples,
                "NaN・∞ の標本を 0 としてイコライザに通し、その位置は元の値のまま出しました",
            ));
        }
        let m = EqMachine::new(&r, audio.channels(), crate::math::db_to_gain(p.gain_db));
        Ok(Box::new(StatefulKernel::new(
            Arc::new(m),
            STATE_GRID,
            table,
            audio.clone(),
        )?))
    }
}

impl Op for Eq {
    const ID: &'static str = "audio.eq";
    const VERSION: u16 = 1;
    type Params = EqParams;
    const NEEDS: &'static [(&'static str, u16)] = &[("audio.eq_state", 1)];

    fn needs(p: &EqParams, inputs: &[Shape]) -> Result<Vec<Need>, Issues> {
        if !Self::has_sections(p, inputs) {
            return Ok(Vec::new());
        }
        Ok(vec![Need::of::<EqState>(0, &EqStateParams::of(p))?])
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

    fn infer(p: &EqParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        expect_inputs(inputs, &Self::signature())?;
        let Some((rate, channels, frames)) = audio_shape(&inputs[0]) else {
            return Err(Issues::none());
        };
        let r = resolve(p, rate, usize::from(channels))?;
        let sections: usize = r.bands.iter().map(|b| b.sections.len()).sum();
        Ok(Inferred {
            shape: inputs[0],
            space: SpaceMap::Identity,
            time: TimeMap::Identity,
            cost: frames
                .max()
                .saturating_mul(u64::from(channels))
                .saturating_mul(sections.max(1) as u64 * 2),
        })
    }

    fn apply(
        p: &EqParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        cx.check()?;
        let audio = audio_of(inputs[0], Self::ID)?;
        let k = Self::kernel(p, inputs[0], cx)?;
        if k.passthrough() == Some(0) {
            return Ok(Applied::value(Value::Audio(audio.clone())));
        }
        Ok(Applied::value(render_all(
            k.as_ref(),
            audio.meta.clone(),
            cx.mon,
        )?))
    }

    fn audio_kernel(
        p: &EqParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Box<dyn AudioKernel>, EngineError>> {
        Some(Self::kernel(p, inputs[0], cx))
    }

    fn work_bytes(p: &Self::Params, inputs: &[Shape], out: &Shape) -> Work {
        let Some((rate, channels)) = Self::rate_channels(inputs) else {
            return crate::audio::render_all_work(out);
        };
        match resolve(p, rate, channels) {
            Ok(r) if !r.bands.is_empty() => {
                let m = EqMachine::new(&r, channels, 1.0);
                stateful_work(
                    out,
                    m.width as u64,
                    (channels / m.width.max(1)) as u64,
                    m.state_len as u64,
                )
            }
            _ => crate::audio::render_all_work(out),
        }
    }

    fn reach(p: &EqParams, inputs: &[Shape]) -> Reach {
        match inputs.first().and_then(audio_shape) {
            Some((_, _, Dim::Exact(n))) if Self::has_sections(p, inputs) => Reach::Prefix {
                after: 0,
                frames: n,
            },
            Some(_) if !Self::has_sections(p, inputs) => Reach::SameTime,
            _ => Reach::Global,
        }
    }

    fn audio_reach(p: &EqParams, inputs: &[Shape]) -> Reach {
        match inputs.first().and_then(audio_shape) {
            Some((_, _, Dim::Exact(n))) if Self::has_sections(p, inputs) => Reach::Aligned {
                grid: STATE_GRID,
                before: 0,
                after: 0,
                frames: n,
            },
            Some(_) if !Self::has_sections(p, inputs) => Reach::SameTime,
            _ => Reach::Global,
        }
    }

    fn audio_length_change(
        old: &EqParams,
        new: &EqParams,
        _old_in: &[Shape],
        _new_in: &[Shape],
    ) -> Option<Area> {
        (old == new).then_some(Area::Empty)
    }

    fn examples() -> Vec<Example<EqParams>> {
        examples()
    }
}

/// バンドを作る（例とテストの補助）。
pub fn band(kind: BandType, freq: f64, gain_db: f64, q: f64) -> Band {
    Band {
        kind,
        freq,
        gain_db,
        q,
        slope: 12,
        channel: BandChannel::All,
        enabled: true,
    }
}

fn noise(rate: u32, channels: u16, frames: u64, seed: u64) -> InputGen {
    InputGen::AudioNoise {
        rate,
        channels,
        frames,
        seed,
    }
}

fn params(bands: Vec<Band>, gain_db: f64) -> EqParams {
    EqParams {
        bands,
        phase: Phase::Minimum,
        gain_db,
    }
}

/// 例（適合テストとゴールデンの種）。
fn examples() -> Vec<Example<EqParams>> {
    use BandType::*;
    let cut = |kind, freq, slope| Band {
        slope,
        ..band(kind, freq, 0.0, 0.707)
    };
    let on = |mut b: Band, channel| {
        b.channel = channel;
        b
    };
    vec![
        Example {
            name: "bells-and-shelves",
            params: params(
                vec![
                    band(LowShelf, 120.0, 4.5, 0.707),
                    band(Bell, 950.0, -6.0, 2.5),
                    band(Bell, 3_200.0, 3.0, 0.9),
                    band(HighShelf, 9_000.0, -3.0, 1.2),
                ],
                -1.5,
            ),
            inputs: vec![noise(48_000, 2, 20_000, 91)],
        },
        Example {
            name: "cuts-notch-bandpass-tilt",
            params: params(
                vec![
                    cut(LowCut, 40.0, 18),
                    cut(HighCut, 15_000.0, 48),
                    band(Notch, 60.0, 0.0, 8.0),
                    band(BandPass, 2_000.0, 0.0, 0.6),
                    band(Tilt, 1_000.0, 4.0, 0.707),
                ],
                0.0,
            ),
            inputs: vec![noise(44_100, 2, 30_000, 92)],
        },
        // M/S と L・R のバンド（2 チャンネルを 1 組で流す）
        Example {
            name: "mid-side-and-sides",
            params: params(
                vec![
                    on(band(Bell, 500.0, 5.0, 1.0), BandChannel::Mid),
                    on(band(HighShelf, 6_000.0, -4.0, 0.707), BandChannel::Side),
                    on(band(Bell, 2_500.0, 3.0, 4.0), BandChannel::Left),
                    band(Bell, 120.0, -2.0, 1.4),
                    on(band(Bell, 7_000.0, 2.0, 2.0), BandChannel::Right),
                ],
                0.0,
            ),
            inputs: vec![noise(48_000, 2, 40_000, 93)],
        },
        // モノラル：S は外して警告、M・L は当てる
        Example {
            name: "mono-drops-side",
            params: params(
                vec![
                    on(band(Bell, 800.0, 6.0, 1.0), BandChannel::Mid),
                    on(band(Bell, 3_000.0, -6.0, 1.0), BandChannel::Side),
                    on(band(LowShelf, 200.0, 3.0, 0.707), BandChannel::Left),
                ],
                0.0,
            ),
            inputs: vec![noise(22_050, 1, 9_000, 94)],
        },
        // 3 チャンネル：L・R は 1・2 チャンネル目、M は外して警告
        Example {
            name: "three-channels",
            params: params(
                vec![
                    on(band(Bell, 1_000.0, 6.0, 1.0), BandChannel::Right),
                    on(band(Bell, 3_000.0, 6.0, 1.0), BandChannel::Mid),
                    band(HighCut, 6_000.0, 0.0, 0.707),
                ],
                0.0,
            ),
            inputs: vec![noise(16_000, 3, 8_000, 95)],
        },
        // ナイキストの近く・以上（8 kHz）：ベルは止め、ハイカット・ノッチは外す
        Example {
            name: "near-and-above-nyquist",
            params: params(
                vec![
                    band(Bell, 3_990.0, 6.0, 2.0),
                    band(HighShelf, 12_000.0, -6.0, 0.707),
                    cut(HighCut, 4_500.0, 24),
                    band(Notch, 4_000.0, 0.0, 4.0),
                    band(Bell, 300.0, -3.0, 40.0),
                ],
                0.0,
            ),
            inputs: vec![noise(8_000, 2, 8_000, 96)],
        },
        // チャンクの境をまたぐ長さ（状態の表の格子を 2 つまたぐ）
        Example {
            name: "crosses-audio-chunks",
            params: params(
                vec![
                    cut(LowCut, 25.0, 96),
                    band(Bell, 10.0, 12.0, 0.1),
                    band(Bell, 1_500.0, -30.0, 40.0),
                ],
                2.0,
            ),
            inputs: vec![noise(8_000, 2, 2 * AUDIO_CHUNK as u64 + 777, 97)],
        },
        // 信号のあとの無音を、遅い極（20 Hz の高い Q・48 dB/oct のローカット）に通す（サブノーマルの重さを測る形。F5）
        Example {
            name: "silence-after-signal",
            params: params(
                vec![
                    band(Bell, 20.0, 12.0, 18.0),
                    cut(LowCut, 20.0, 48),
                    band(LowShelf, 30.0, 6.0, 4.0),
                ],
                0.0,
            ),
            inputs: vec![InputGen::SineLevels {
                rate: 8_000,
                freq: 40,
                segments: vec![(4_000, -6), (60_000, -1_000)],
            }],
        },
        // NaN・∞：0 としてフィルタに通し、その位置は元の値（警告）
        Example {
            name: "non-finite-samples",
            params: params(vec![band(Bell, 1_000.0, 6.0, 1.0)], 0.0),
            inputs: vec![InputGen::NonFinite {
                base: Box::new(noise(8_000, 2, 9_000, 98)),
                at: vec![
                    (0, 10, NonFiniteSample::Nan),
                    (1, 4_000, NonFiniteSample::Inf),
                ],
            }],
        },
        // 何も変えない（Identity。申告しない）
        Example {
            name: "identity",
            params: params(
                vec![
                    band(Bell, 1_000.0, 0.0, 1.0),
                    Band {
                        enabled: false,
                        ..cut(LowCut, 100.0, 24)
                    },
                ],
                0.0,
            ),
            inputs: vec![noise(8_000, 2, 5_000, 99)],
        },
        // 出力の利得だけ（申告しない）
        Example {
            name: "gain-only",
            params: params(Vec::new(), -6.0),
            inputs: vec![noise(8_000, 1, 5_000, 100)],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    fn eq_json(bands: serde_json::Value) -> serde_json::Value {
        json!({ "bands": bands, "phase": "minimum", "gainDb": 0.0 })
    }

    /// 範囲の確かめ：24 個まで・周波数・利得・Q（シェルフは 4 まで）・傾き・段の数 48 まで。
    #[test]
    fn ranges_are_checked() {
        let schema = EqParams::schema();
        let bad = |b: serde_json::Value| !schema.check_ranges(&eq_json(b)).is_empty();
        assert!(!bad(
            json!([{ "type": "bell", "freq": 1000.0, "gainDb": 3.0, "q": 1.0 }])
        ));
        assert!(bad(json!([{ "type": "bell", "freq": 5.0 }])));
        assert!(bad(
            json!([{ "type": "bell", "freq": 1000.0, "gainDb": 31.0 }])
        ));
        assert!(bad(json!([{ "type": "bell", "freq": 1000.0, "q": 41.0 }])));
        assert!(bad(
            json!([{ "type": "lowShelf", "freq": 1000.0, "q": 4.5 }])
        ));
        assert!(!bad(json!([{ "type": "bell", "freq": 1000.0, "q": 4.5 }])));
        assert!(bad(
            json!([{ "type": "lowCut", "freq": 100.0, "slope": 30 }])
        ));
        let many: Vec<_> = (0..25)
            .map(|_| json!({ "type": "bell", "freq": 100.0 }))
            .collect();
        assert!(bad(json!(many)));
        // 96 dB/oct のカット 6 本（48 段）は通り、7 本（56 段）は断る。オフのバンドは数えない
        let cuts = |n: usize, off: usize| {
            let mut v: Vec<_> = (0..n)
                .map(|_| json!({ "type": "lowCut", "freq": 100.0, "slope": 96 }))
                .collect();
            v.extend((0..off).map(
                |_| json!({ "type": "lowCut", "freq": 100.0, "slope": 96, "enabled": false }),
            ));
            json!(v)
        };
        assert!(!bad(cuts(6, 3)));
        assert!(bad(cuts(7, 0)));
    }

    /// 推論の扱い：ナイキスト以上のローカット・バンドパスは断り、ハイカット・ノッチは外し、ベル・シェルフ・チルトは止める。
    /// 直線位相は断る。
    #[test]
    fn nyquist_and_channels_follow_the_rules() {
        use BandType::*;
        let p = |b: Band| params(vec![b], 0.0);
        assert!(resolve(&p(band(LowCut, 5_000.0, 0.0, 0.7)), 8_000, 2).is_err());
        assert!(resolve(&p(band(BandPass, 4_000.0, 0.0, 0.7)), 8_000, 2).is_err());
        let r = resolve(&p(band(HighCut, 4_000.0, 0.0, 0.7)), 8_000, 2).unwrap();
        assert!(r.bands.is_empty() && r.warnings[0].code == IssueCode::BandDropped);
        let r = resolve(&p(band(Notch, 6_000.0, 0.0, 0.7)), 8_000, 2).unwrap();
        assert!(r.bands.is_empty() && r.warnings[0].code == IssueCode::BandDropped);
        for kind in [Bell, LowShelf, HighShelf, Tilt] {
            let r = resolve(&p(band(kind, 9_000.0, 3.0, 0.7)), 8_000, 2).unwrap();
            assert_eq!(r.bands.len(), 1, "{kind:?}");
            assert_eq!(r.warnings[0].code, IssueCode::FrequencyClamped, "{kind:?}");
        }
        let mut lin = p(band(Bell, 1_000.0, 3.0, 1.0));
        lin.phase = Phase::Linear;
        assert!(resolve(&lin, 48_000, 2).is_err());
        // 0 dB のベル・オフのバンドは段を作らない
        let r = resolve(&p(band(Bell, 1_000.0, 0.0, 1.0)), 48_000, 2).unwrap();
        assert!(r.bands.is_empty() && r.warnings.is_empty());
        let mut off = band(Bell, 1_000.0, 6.0, 1.0);
        off.enabled = false;
        let r = resolve(&p(off), 48_000, 2).unwrap();
        assert!(r.bands.is_empty() && r.warnings.is_empty());
        // チャンネルの行き先：モノラルは L・R・M をすべてに、S は外す。ステレオは L・R を 1・2 チャンネル目に、M/S は
        // M/S の領域に。3 チャンネル以上は L・R を 1・2 チャンネル目に、M/S は外す。
        let to = |ch: BandChannel, channels: usize| {
            let mut b = band(Bell, 1_000.0, 3.0, 1.0);
            b.channel = ch;
            let r = resolve(&p(b), 48_000, channels).unwrap();
            (
                r.bands.first().map(|b| b.target),
                r.warnings.first().map(|w| w.code),
            )
        };
        use BandChannel as C;
        let dropped = (None, Some(IssueCode::BandDropped));
        for (ch, mono, stereo, three) in [
            (
                C::All,
                Some(Target::All),
                Some(Target::All),
                Some(Target::All),
            ),
            (
                C::Left,
                Some(Target::All),
                Some(Target::Channel(0)),
                Some(Target::Channel(0)),
            ),
            (
                C::Right,
                Some(Target::All),
                Some(Target::Channel(1)),
                Some(Target::Channel(1)),
            ),
            (C::Mid, Some(Target::All), Some(Target::Mid), None),
            (C::Side, None, Some(Target::Side), None),
        ] {
            for (channels, want) in [(1, mono), (2, stereo), (3, three), (6, three)] {
                let got = to(ch, channels);
                match want {
                    Some(t) => assert_eq!(got, (Some(t), None), "{ch:?} {channels}"),
                    None => assert_eq!(got, dropped, "{ch:?} {channels}"),
                }
            }
        }
    }

    /// 素朴な参照：バンドごと・チャンネルごとに、1 標本ずつ DF-I を回す（M/S は毎標本で変換し直す形でなく、核と同じ
    /// 「領域が替わる所だけ」の規則を素直に書いたもの）。
    fn reference(p: &EqParams, planes: &[Vec<f32>], rate: u32) -> Vec<Vec<f32>> {
        let channels = planes.len();
        let r = resolve(p, rate, channels).unwrap();
        let gain = crate::math::db_to_gain(p.gain_db);
        let n = planes.first().map_or(0, Vec::len);
        let ms_lane = r.has_ms() && channels == 2;
        // 状態：（バンド, 信号, 段）ごとの [x1, x2, y1, y2]
        let mut states: Vec<Vec<Vec<[f64; 4]>>> = r
            .bands
            .iter()
            .map(|b| vec![vec![[0.0; 4]; b.sections.len()]; channels.max(2)])
            .collect();
        let mut out = vec![vec![0f32; n]; channels];
        for t in 0..n {
            let mut v: Vec<f64> = planes
                .iter()
                .map(|p| {
                    if p[t].is_finite() {
                        f64::from(p[t])
                    } else {
                        0.0
                    }
                })
                .collect();
            let mut ms = false;
            for (bi, b) in r.bands.iter().enumerate() {
                if ms_lane && b.target.ms() != ms {
                    let (x0, x1) = (v[0], v[1]);
                    if b.target.ms() {
                        v[0] = (x0 + x1) * 0.5;
                        v[1] = (x0 - x1) * 0.5;
                    } else {
                        v[0] = x0 + x1;
                        v[1] = x0 - x1;
                    }
                    ms = b.target.ms();
                }
                let signals: Vec<usize> = match b.target {
                    Target::All => (0..channels).collect(),
                    Target::Channel(c) => vec![c],
                    Target::Mid => vec![0],
                    Target::Side => vec![1],
                };
                for sig in signals {
                    for (si, s) in b.sections.iter().enumerate() {
                        let st = &mut states[bi][sig][si];
                        let x = v[sig];
                        let y =
                            s.b0 * x + s.b1 * st[0] + s.b2 * st[1] - s.a1 * st[2] - s.a2 * st[3];
                        *st = [x, st[0], y, st[2]];
                        v[sig] = y;
                    }
                }
            }
            if ms {
                let (x0, x1) = (v[0], v[1]);
                v[0] = x0 + x1;
                v[1] = x0 - x1;
            }
            for c in 0..channels {
                out[c][t] = if planes[c][t].is_finite() {
                    (v[c] * gain) as f32
                } else {
                    planes[c][t]
                };
            }
        }
        out
    }

    /// すべての例で、操作の値（表を解いて核で描いたもの）が素朴な参照とビット一致し、警告が推論の扱いのとおり。
    #[test]
    fn examples_match_the_reference() {
        let engine = crate::Engine::new().unwrap();
        for ex in examples() {
            let input = ex.inputs[0].generate().unwrap();
            let Value::Audio(a) = &input else { panic!() };
            let planes: Vec<Vec<f32>> = (0..a.channels())
                .map(|c| {
                    let mut p = vec![0f32; a.frames()];
                    a.read(c, 0, &mut p).unwrap();
                    p
                })
                .collect();
            let graph = crate::testing::single_node_graph(
                Eq::ID,
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
            let Value::Audio(got) = &r.outputs[0].value else {
                panic!()
            };
            let want = reference(&ex.params, &planes, a.rate());
            for (c, w) in want.iter().enumerate() {
                let mut g = vec![0f32; got.frames()];
                got.read(c, 0, &mut g).unwrap();
                assert!(
                    g.iter().zip(w).all(|(x, y)| x.to_bits() == y.to_bits()),
                    "{}：チャンネル {c}",
                    ex.name
                );
            }
            let codes: Vec<IssueCode> = r.warnings.iter().map(|w| w.code).collect();
            let want_codes: &[IssueCode] = match ex.name {
                "mono-drops-side" | "three-channels" => &[IssueCode::BandDropped],
                "near-and-above-nyquist" => &[
                    IssueCode::FrequencyClamped,
                    IssueCode::FrequencyClamped,
                    IssueCode::BandDropped,
                    IssueCode::BandDropped,
                ],
                "non-finite-samples" => &[IssueCode::NonFiniteSamples],
                _ => &[],
            };
            assert_eq!(codes, want_codes, "{}", ex.name);
        }
    }

    /// 正弦の掃引：定常の正弦の出力の振幅が、係数から求めたデジタルの応答と ±0.05 dB（各型・端の周波数）。
    #[test]
    fn sine_sweep_matches_the_digital_response() {
        use BandType::*;
        let rate = 48_000u32;
        let cases = vec![
            band(Bell, 1_000.0, 9.0, 1.5),
            band(Bell, 18_000.0, -12.0, 3.0),
            band(LowShelf, 300.0, 6.0, 2.0),
            band(HighShelf, 8_000.0, -9.0, 0.5),
            Band {
                slope: 36,
                ..band(LowCut, 200.0, 0.0, 0.7)
            },
            Band {
                slope: 18,
                ..band(HighCut, 5_000.0, 0.0, 0.7)
            },
            band(Notch, 2_000.0, 0.0, 2.0),
            band(BandPass, 700.0, 0.0, 1.0),
            band(Tilt, 1_000.0, 6.0, 0.7),
        ];
        for b in cases {
            let p = params(vec![b.clone()], 0.0);
            let r = resolve(&p, rate, 1).unwrap();
            for f in [60.0, 440.0, 1_000.0, 3_000.0, 9_000.0, 15_000.0] {
                let n = 48_000usize;
                let x: Vec<f32> = (0..n)
                    .map(|i| {
                        (0.25
                            * crate::math::sin(
                                2.0 * std::f64::consts::PI * f * i as f64 / f64::from(rate),
                            )) as f32
                    })
                    .collect();
                let y = &reference(&p, std::slice::from_ref(&x), rate)[0];
                // 後ろ半分の RMS の比（定常）
                let rms = |v: &[f32]| {
                    (v[n / 2..]
                        .iter()
                        .map(|s| f64::from(*s) * f64::from(*s))
                        .sum::<f64>()
                        / (n / 2) as f64)
                        .sqrt()
                };
                let got = 20.0 * crate::math::log10(rms(y) / rms(&x));
                let w = 2.0 * std::f64::consts::PI * f / f64::from(rate);
                let want: f64 = r.bands[0].sections.iter().map(|s| s.db(w)).sum();
                if want < -60.0 {
                    continue;
                }
                assert!((got - want).abs() < 0.05, "{b:?} {f} Hz：{got} と {want}");
            }
        }
    }

    /// 曲線は、推論を通したバンドの段の振幅の和で、合成は出力の利得を足す。
    #[test]
    fn curve_sums_the_sections() {
        use BandType::*;
        let p = params(
            vec![
                band(Bell, 1_000.0, 6.0, 1.0),
                band(Bell, 30_000.0, 3.0, 1.0),
            ],
            -2.0,
        );
        let freqs = [100.0, 1_000.0, 10_000.0];
        let c = curve(&p, 48_000, 2, &freqs).unwrap();
        assert_eq!(c.bands.len(), 2);
        assert!((c.bands[0].db[1] - 6.0).abs() < 1e-6);
        for (k, t) in c.total.iter().enumerate() {
            let sum: f64 = c.bands.iter().map(|b| b.db[k]).sum();
            assert!((t - (sum - 2.0)).abs() < 1e-9);
        }
    }
}
