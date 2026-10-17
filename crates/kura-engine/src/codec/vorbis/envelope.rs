//! 過渡の検出と窓の長短（`lib/envelope.c`。aoTuV の 12 帯域）。
//!
//! # 移す C（SSE でない枝）
//!
//! | 関数 | 行 | 備考 |
//! |---|---|---|
//! | `_ve_envelope_init` | 42–85 | winlength 128・searchstep 64・`minenergy = preecho_minenergy`・`cursor = blocksizes[1]/2`・`storage = 128`。MDCT 128（mdct.rs）。窓 `t = sin(i/(n-1.)*M_PI); win = t*t`（double の sin を float にしてから float の積。62–63 の `#else` の側）。帯域は `vorbis_tables::BAND_BEGIN`・`BAND_END`（12 帯域。aoTuV の追加 5 帯域を含む）、帯域の窓 `sin((i+.5)/n*M_PI)`（float）と `total`（float に足し、`1./total`） |
//! | `_ve_amp` | 101–562 | SSE でない枝：窓を掛ける（173–175）、MDCT（その場）、DC 近くの広がり（180–202。`temp = v0*v0 + .7*v1*v1 + .2*v2*v2` は double を含む式、`nearDC` の 15 の輪、`decay *= 1./(VE_NEARDC+1)`、`decay = todB(&decay)*.5 - 15.f`）、2 本ずつ dB（430–438。`decay -= 8.`）、帯域ごと（444–559。SSE でない 519–522）に重み付き平均 → 過去 `stretch` 窓の max・min と比べて前エコー（ret 1|4）・後エコー（ret 2） |
//! | `_ve_envelope_search` | 569–681 | `first = current/searchstep`、`last = pcm_current/searchstep - VE_WIN`、mark の大きさを広げる、各 j で stretch を進め、チャンネルごとに `_ve_amp`、印を付ける。`testW` まで cursor を進め、印があれば 0（次は短い）か 1、データが足りなければ −1 |
//! | `_ve_envelope_mark` | 683–707 | 今の窓の範囲に印があるか（impulse か padding か） |
//! | `_ve_envelope_shift` | 709–728 | mark を `shift/searchstep` だけずらし、current・curmark・cursor を引く |
//!
//! - `max`・`min` は os.h のマクロ（`((x)<(y)?(y):(x))`）のまま比較で写す（534–541）。`stretch = max(VE_MINSTRETCH, stretch/2)` は int。
//! - `penalty = stretch_penalty - (stretch/2 - VE_MINSTRETCH)`（float と int）を 0〜stretch_penalty に止める。
//! - 12 帯域すべてを見る（aoTuV の「envelope の 12 帯域」は移す。7 帯域にする ABL(ENV) の枝は作らない）。
//! - 閾値 `gi->preecho_thresh[j]`・`postecho_thresh[j]` は setup.rs が先頭 4 帯域だけ補間したもの。
//! - 中止（kura の追加）：`_ve_envelope_search` の j のループで [`CANCEL_STEPS`] 歩ごとに `Monitor::canceled` を見て、
//!   頼まれていれば `Canceled` を返す（値は変わらない。中止した状態は捨てる）。
//!
//! 入力を一度に渡す形でも、C と同じ位置の数（current・cursor・curmark は blockout の `_ve_envelope_shift` でずれる）を
//! 保てばよい。ずらさずに絶対の位置で持つなら、すべての比較・割り算が同じ値になることを確かめる（`/ searchstep` の切り捨ては
//! 64 の倍数のずらしでは変わらない）。
//!
//! # 移さないもの
//!
//! SSE の枝（116–117・133–171・207–429・449–518）、`#if 0` の解析の出力、`_ve_envelope_clear`。
//!
//! # 型の対応
//!
//! `envelope_filter_state`・`envelope_band`・`envelope_lookup`（envelope.h:33–70）→ [`EnvelopeFilterState`]・[`EnvelopeBand`]・
//! [`EnvelopeLookup`]。`filter` は `ch * VE_BANDS` 個（チャンネル i の帯域 j は `i*VE_BANDS + j`）。`mark` は `Vec<i32>`。
//!
//! # 移植の補足（位置の持ち方）
//!
//! - `current`・`curmark`・`cursor`・`storage` は C と同じ値（C の座標。`_ve_envelope_shift` で C と同じに引く。`curmark` が
//!   負になったら止まる所も C のとおり）。比較・割り算はすべて C と同じ式・同じ値で行う。
//! - `mark` は動かさない：C の `mark[k]` は `mark[mark_base + k]`。`_ve_envelope_shift` の `memmove` は `mark_base` に
//!   `shift/searchstep` を足すことで同じにする（O(N²) の memmove をしない）。C の memmove で動かない後ろの残り（古い値）は
//!   ここでは 0 になるが、C も読む前に `mark[j+VE_POST]=0` で書き直す（読むのは `current/searchstep + 1` まで）ので結果は同じ。
//! - pcm は block.rs の [`VorbisDspState`] が同じく動かさずに持つ（C の `pcm[i][k]` は `pcm[i][pcm_base + k]`）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use std::f64::consts::PI;

use vorbis_tables::{BAND_BEGIN, BAND_END, VE_BANDS, VorbisInfoPsyGlobal};

use super::block::VorbisDspState;
use super::info::VorbisInfo;
use super::mdct::{MdctLookup, mdct_forward_with, mdct_init};
use super::scales::todb;
use crate::error::EngineError;
use crate::math;
use crate::monitor::Monitor;

/// `_ve_envelope_search` の j のループで中止を確かめる間隔（歩数。1 歩は searchstep = 64 標本 × チャンネル。確かめるだけで
/// 値は変わらない）。入力を一度に渡すので最初の探索は入力の全体を 1 回で回る（48 kHz ステレオの 3 時間で数秒）。
pub(crate) const CANCEL_STEPS: i32 = 2048;

/// envelope.h:23。
pub(crate) const VE_WIN: i64 = 4;
/// envelope.h:24。
pub(crate) const VE_POST: i64 = 2;
/// envelope.h:25（`VE_PRE + VE_POST - 1`。VE_PRE は 16）。
pub(crate) const VE_AMP: usize = 17;
/// envelope.h:28。
pub(crate) const VE_NEARDC: usize = 15;
/// envelope.h:30。
pub(crate) const VE_MINSTRETCH: i32 = 2;
/// envelope.h:31。
pub(crate) const VE_MAXSTRETCH: i32 = 12;

/// `envelope_filter_state`（envelope.h:33–42）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct EnvelopeFilterState {
    pub ampbuf: [f32; VE_AMP],
    pub ampptr: i32,
    pub near_dc: [f32; VE_NEARDC],
    /// C の `nearDC_acc`。
    pub near_dc_acc: f32,
    /// C の `nearDC_partialacc`。
    pub near_dc_partialacc: f32,
    pub nearptr: i32,
}

/// `envelope_band`（envelope.h:44–49）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EnvelopeBand {
    pub begin: i32,
    pub end: i32,
    /// `end` 個。
    pub window: Vec<f32>,
    /// 窓の和の逆数。
    pub total: f32,
}

/// `envelope_lookup`（envelope.h:51–70）。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct EnvelopeLookup {
    pub ch: i32,
    pub winlength: i32,
    pub searchstep: i32,
    pub minenergy: f32,
    pub mdct: MdctLookup,
    /// 128 個。
    pub mdct_win: Vec<f32>,
    pub band: [EnvelopeBand; VE_BANDS],
    /// `ch * VE_BANDS` 個。
    pub filter: Vec<EnvelopeFilterState>,
    pub stretch: i32,
    /// 印（C の `mark[k]` は `mark[mark_base + k]`。長さは `mark_base + storage` 以上）。
    pub mark: Vec<i32>,
    /// C の `mark` の先頭の位置（`_ve_envelope_shift` で動かした数の和。kura の追加）。
    pub mark_base: usize,
    pub storage: i64,
    pub current: i64,
    pub curmark: i64,
    pub cursor: i64,
}

/// `_ve_envelope_init`（envelope.c:42–85）。
pub(crate) fn ve_envelope_init(vi: &VorbisInfo) -> EnvelopeLookup {
    let ci = &vi.codec_setup;
    let gi = &ci.psy_g_param;
    let ch = vi.channels;
    let n: i32 = 128;
    let mut e = EnvelopeLookup {
        winlength: n,
        searchstep: 64, // not random
        minenergy: gi.preecho_minenergy,
        ch,
        storage: 128,
        cursor: ci.blocksizes[1] / 2,
        mdct: mdct_init(n),
        ..EnvelopeLookup::default()
    };

    e.mdct_win = (0..n)
        .map(|i| {
            // float t = sin(i/(n-1.)*M_PI); e->mdct_win[i] = t*t;
            let t = math::sin(f64::from(i) / (f64::from(n) - 1.0) * PI) as f32;
            t * t
        })
        .collect();

    for (j, b) in e.band.iter_mut().enumerate() {
        b.begin = BAND_BEGIN[j];
        b.end = BAND_END[j];
        let n = b.end;
        let mut total = 0.0_f32;
        b.window = (0..n)
            .map(|i| {
                let w = math::sin((f64::from(i) + 0.5) / f64::from(n) * PI) as f32;
                total += w;
                w
            })
            .collect();
        // e->band[j].total=1./e->band[j].total;
        b.total = (1.0 / f64::from(total)) as f32;
    }

    let ch = usize::try_from(ch).unwrap_or(0);
    e.filter = vec![EnvelopeFilterState::default(); VE_BANDS * ch];
    e.mark = vec![0; 128];
    e
}

/// `_ve_amp` の作業領域（C の `alloca` の `vec` と、MDCT の作業領域）。
struct AmpScratch {
    /// 窓を掛けた入力（winlength 個）。
    windowed: Vec<f32>,
    /// MDCT の出力（winlength/2 個。C の `vec` の前半。その場で dB に書き換える）。
    vec: Vec<f32>,
    /// MDCT の作業領域（winlength 個）。
    w: Vec<f32>,
}

impl AmpScratch {
    fn new(n: usize) -> Self {
        Self {
            windowed: vec![0.0; n],
            vec: vec![0.0; n / 2],
            w: vec![0.0; n],
        }
    }
}

/// `_ve_amp`（envelope.c:101–562）：1 窓（128 個の PCM）を見て、前エコー・後エコーの印（ビット 1・2・4）を返す。
/// `filters` はそのチャンネルの `VE_BANDS` 個。大きさは [`lookup_is_valid`] で確かめてから呼ぶ。
fn ve_amp(
    ve: &EnvelopeLookup,
    gi: &VorbisInfoPsyGlobal,
    data: &[f32],
    bands: &[EnvelopeBand],
    filters: &mut [EnvelopeFilterState],
    s: &mut AmpScratch,
) -> i32 {
    let n = ve.winlength as usize;
    let mut ret = 0;

    // we want to have a 'minimum bar' for energy
    let min_v = ve.minenergy;

    // int stretch=max(VE_MINSTRETCH,ve->stretch/2);
    let half = ve.stretch / 2;
    let stretch = if VE_MINSTRETCH < half {
        half
    } else {
        VE_MINSTRETCH
    };
    // float penalty=gi->stretch_penalty-(ve->stretch/2-VE_MINSTRETCH);
    let mut penalty = gi.stretch_penalty - (ve.stretch / 2 - VE_MINSTRETCH) as f32;
    if penalty < 0.0 {
        penalty = 0.0;
    }
    if penalty > gi.stretch_penalty {
        penalty = gi.stretch_penalty;
    }

    // window and transform
    for ((o, d), w) in s.windowed.iter_mut().zip(&data[..n]).zip(&ve.mdct_win) {
        *o = *d * *w;
    }
    mdct_forward_with(&ve.mdct, &s.windowed, &mut s.vec, &mut s.w);
    let vec = &mut s.vec;

    // near-DC spreading function
    let mut decay;
    {
        // float temp=vec[0]*vec[0]+.7*vec[1]*vec[1]+.2*vec[2]*vec[2];
        let temp = (f64::from(vec[0] * vec[0])
            + 0.7 * f64::from(vec[1]) * f64::from(vec[1])
            + 0.2 * f64::from(vec[2]) * f64::from(vec[2])) as f32;
        let f0 = &mut filters[0];
        let ptr = f0.nearptr as usize;

        // the accumulation is regularly refreshed from scratch to avoid floating point creep
        if ptr == 0 {
            f0.near_dc_acc = f0.near_dc_partialacc + temp;
            decay = f0.near_dc_acc;
            f0.near_dc_partialacc = temp;
        } else {
            f0.near_dc_acc += temp;
            decay = f0.near_dc_acc;
            f0.near_dc_partialacc += temp;
        }
        f0.near_dc_acc -= f0.near_dc[ptr];
        f0.near_dc[ptr] = temp;

        // decay*=(1./(VE_NEARDC+1));
        decay = (f64::from(decay) * (1.0 / (VE_NEARDC as f64 + 1.0))) as f32;
        f0.nearptr += 1;
        if f0.nearptr >= VE_NEARDC as i32 {
            f0.nearptr = 0;
        }
        // decay=todB(&decay)*.5-15.f;
        decay = (f64::from(todb(decay)) * 0.5 - f64::from(15.0_f32)) as f32;
    }

    // perform spreading and limiting, also smooth the spectrum（SSE でない枝 431–438）
    let mut i = 0;
    while i < n / 2 {
        let mut val = vec[i] * vec[i] + vec[i + 1] * vec[i + 1];
        val = todb(val) * 0.5_f32;
        if val < decay {
            val = decay;
        }
        if val < min_v {
            val = min_v;
        }
        vec[i >> 1] = val;
        // decay-=8.;
        decay = (f64::from(decay) - 8.0) as f32;
        i += 2;
    }

    // perform preecho/postecho triggering by band
    for (j, (b, f)) in bands.iter().zip(filters.iter_mut()).enumerate() {
        let mut acc = 0.0_f32;

        // accumulate amplitude（SSE でない枝 520–521）
        let begin = b.begin as usize;
        for (k, w) in b.window.iter().enumerate().take(b.end as usize) {
            acc += vec[k + begin] * *w;
        }

        acc *= b.total;

        // convert amplitude to delta
        let valmax;
        let valmin;
        {
            let this = f.ampptr;
            let mut premax = -99999.0_f32;
            let mut premin = 99999.0_f32;

            let mut p = this;
            p -= 1;
            if p < 0 {
                p += VE_AMP as i32;
            }
            let a = f.ampbuf[p as usize];
            // postmax=max(acc,filters[j].ampbuf[p]); postmin=min(acc,filters[j].ampbuf[p]);
            let postmax = if acc < a { a } else { acc };
            let postmin = if acc > a { a } else { acc };

            for _ in 0..stretch {
                p -= 1;
                if p < 0 {
                    p += VE_AMP as i32;
                }
                let a = f.ampbuf[p as usize];
                premax = if premax < a { a } else { premax };
                premin = if premin > a { a } else { premin };
            }

            valmin = postmin - premin;
            valmax = postmax - premax;

            f.ampbuf[this as usize] = acc;
            f.ampptr += 1;
            if f.ampptr >= VE_AMP as i32 {
                f.ampptr = 0;
            }
        }

        // look at min/max, decide trigger
        if valmax > gi.preecho_thresh[j] + penalty {
            ret |= 1;
            ret |= 4;
        }
        if valmin < gi.postecho_thresh[j] - penalty {
            ret |= 2;
        }
    }

    ret
}

/// envelope の look が `_ve_envelope_init` の作った形か（`_ve_amp` の添字がすべて範囲の中に収まるか）。
fn lookup_is_valid(ve: &EnvelopeLookup) -> bool {
    let Ok(n) = usize::try_from(ve.winlength) else {
        return false;
    };
    let Ok(ch) = usize::try_from(ve.ch) else {
        return false;
    };
    n >= 8
        && ve.searchstep > 0
        && ve.mdct.n == ve.winlength
        && ve.mdct_win.len() == n
        && ve.filter.len() == ch * VE_BANDS
        && ve.band.iter().all(|b| {
            b.begin >= 0
                && b.end >= 0
                && b.window.len() == b.end as usize
                && (b.begin + b.end) as usize <= n / 2
        })
        && ve.filter.iter().all(|f| {
            (0..VE_AMP as i32).contains(&f.ampptr) && (0..VE_NEARDC as i32).contains(&f.nearptr)
        })
        && ve.stretch <= VE_MAXSTRETCH * 2
        && ve.mark.len()
            >= ve
                .mark_base
                .saturating_add(usize::try_from(ve.storage).unwrap_or(usize::MAX))
}

/// C の `mark[k]` の位置（範囲の外は `Err`：C では範囲の外の読み書き）。
fn mark_index(ve: &EnvelopeLookup, k: i64) -> Result<usize, EngineError> {
    usize::try_from(k)
        .ok()
        .filter(|&k| (k as i64) < ve.storage)
        .and_then(|k| ve.mark_base.checked_add(k))
        .filter(|&i| i < ve.mark.len())
        .ok_or_else(|| EngineError::Internal(format!("envelope：mark[{k}] は範囲の外")))
}

/// `_ve_envelope_search`（envelope.c:569–681）：次の窓が長い（1）・短い（0）・まだ決まらない（−1）。j のループの
/// [`CANCEL_STEPS`] 歩ごとに `mon` の中止を確かめる（頼まれていれば `Canceled`）。
pub(crate) fn ve_envelope_search(
    v: &mut VorbisDspState,
    mon: &dyn Monitor,
) -> Result<i64, EngineError> {
    let vi = &v.vi;
    let ci = &vi.codec_setup;
    let gi = &ci.psy_g_param;
    let ve = &mut v.backend_state.ve;
    let pcm = &v.pcm;
    let base = v.pcm_base;

    if !lookup_is_valid(ve) || pcm.len() != ve.filter.len() / VE_BANDS {
        return Err(EngineError::Internal(
            "envelope：look の形が正しくない".into(),
        ));
    }
    let step = ve.searchstep;
    let n = ve.winlength as usize;

    // int first=ve->current/ve->searchstep;
    let mut first = (ve.current / i64::from(step)) as i32;
    // int last=v->pcm_current/ve->searchstep-VE_WIN;
    let last = v.pcm_current / step - VE_WIN as i32;
    if first < 0 {
        first = 0;
    }

    // make sure we have enough storage to match the PCM
    if i64::from(last) + VE_WIN + VE_POST > ve.storage {
        ve.storage = i64::from(last) + VE_WIN + VE_POST; // be sure
    }
    let need = usize::try_from(ve.storage)
        .ok()
        .and_then(|s| s.checked_add(ve.mark_base))
        .ok_or_else(|| EngineError::Internal("envelope：mark の大きさがあふれる".into()))?;
    if ve.mark.len() < need {
        ve.mark.resize(need, 0);
    }

    if first < last {
        // 読む pcm が範囲の中にあるか（C では範囲の外の読み取り）
        let end = base + step as usize * (last as usize - 1) + n;
        if pcm.iter().any(|p| p.len() < end) {
            return Err(EngineError::Internal("envelope：pcm が足りない".into()));
        }
        let mut s = AmpScratch::new(n);
        // filter を一時的に外す（ve を読みながら filter を書くため）
        let mut filter = std::mem::take(&mut ve.filter);
        let mut canceled = false;
        for j in first..last {
            if (j - first) % CANCEL_STEPS == CANCEL_STEPS - 1 && mon.canceled() {
                canceled = true;
                break;
            }
            let mut ret = 0;

            ve.stretch += 1;
            if ve.stretch > VE_MAXSTRETCH * 2 {
                ve.stretch = VE_MAXSTRETCH * 2;
            }

            // float *pcm=v->pcm[i]+ve->searchstep*(j);
            let at = base + step as usize * j as usize;
            for (p, f) in pcm.iter().zip(filter.chunks_exact_mut(VE_BANDS)) {
                ret |= ve_amp(ve, gi, &p[at..at + n], &ve.band, f, &mut s);
            }

            // j は first 以上（0 以上）、j+VE_POST は storage より小さい
            let k = ve.mark_base + j as usize;
            ve.mark[k + VE_POST as usize] = 0;
            if ret & 1 != 0 {
                ve.mark[k] = 1;
                ve.mark[k + 1] = 1;
            }

            if ret & 2 != 0 {
                ve.mark[k] = 1;
                if j > 0 {
                    ve.mark[k - 1] = 1;
                }
            }

            if ret & 4 != 0 {
                ve.stretch = -1;
            }
        }
        ve.filter = filter;
        if canceled {
            return Err(EngineError::Canceled);
        }
    }

    ve.current = i64::from(last * step);

    let center_w = v.center_w;
    let bs = ci.blocksizes;
    let test_w = center_w + bs[usize::from(v.w != 0)] / 4 + bs[1] / 2 + bs[0] / 4;

    let step = i64::from(step);
    let mut j = ve.cursor;

    // account for postecho working back one window
    while j < ve.current - step {
        if j >= test_w {
            return Ok(1);
        }

        ve.cursor = j;

        if ve.mark[mark_index(ve, j / step)?] != 0 && j > center_w {
            ve.curmark = j;
            if j >= test_w {
                return Ok(1);
            }
            return Ok(0);
        }
        j += step;
    }

    Ok(-1)
}

/// `_ve_envelope_mark`（envelope.c:683–707）：今の短い窓が impulse（印がある）か。
pub(crate) fn ve_envelope_mark(v: &VorbisDspState) -> Result<bool, EngineError> {
    let ve = &v.backend_state.ve;
    let bs = v.vi.codec_setup.blocksizes;
    let at = |w: i64| bs[usize::from(w != 0)];
    let center_w = v.center_w;
    let mut begin_w = center_w - at(v.w) / 4;
    let mut end_w = center_w + at(v.w) / 4;
    if v.w != 0 {
        begin_w -= at(v.lw) / 4;
        end_w += at(v.nw) / 4;
    } else {
        begin_w -= bs[0] / 4;
        end_w += bs[0] / 4;
    }

    if ve.curmark >= begin_w && ve.curmark < end_w {
        return Ok(true);
    }
    if ve.searchstep <= 0 {
        return Err(EngineError::Internal(
            "envelope：searchstep が正しくない".into(),
        ));
    }
    let step = i64::from(ve.searchstep);
    let first = begin_w / step;
    let last = end_w / step;
    for i in first..last {
        if ve.mark[mark_index(ve, i)?] != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `_ve_envelope_shift`（envelope.c:709–728）。mark は動かさずに `mark_base` を進める（冒頭の「位置の持ち方」）。
pub(crate) fn ve_envelope_shift(e: &mut EnvelopeLookup, shift: i64) -> Result<(), EngineError> {
    if e.searchstep <= 0 {
        return Err(EngineError::Internal(
            "envelope：searchstep が正しくない".into(),
        ));
    }
    let step = i64::from(e.searchstep);
    // int smallsize=e->current/e->searchstep+VE_POST;
    let smallsize = e.current / step + VE_POST;
    // int smallshift=shift/e->searchstep;
    let smallshift = shift / step;

    // memmove(e->mark,e->mark+smallshift,(smallsize-smallshift)*sizeof(*e->mark));
    // （動かす範囲が C の配列の中にあること。C では範囲の外の読み書き）
    if smallshift < 0 || smallsize < smallshift || smallsize > e.storage {
        return Err(EngineError::Internal(format!(
            "envelope：mark のずらし（{smallshift}・{smallsize}）が範囲の外"
        )));
    }
    e.mark_base += smallshift as usize;
    let need = e.mark_base + e.storage as usize;
    if e.mark.len() < need {
        e.mark.resize(need, 0);
    }

    e.current -= shift;
    if e.curmark >= 0 {
        e.curmark -= shift;
    }
    e.cursor -= shift;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(ch: i32, bs: [i64; 2]) -> VorbisInfo {
        let mut vi = super::super::info::vorbis_info_init();
        vi.channels = ch;
        vi.rate = 44_100;
        vi.codec_setup.blocksizes = bs;
        vi.codec_setup.psy_g_param.preecho_minenergy = -80.0;
        vi.codec_setup.psy_g_param.preecho_thresh = [10.0; VE_BANDS];
        vi.codec_setup.psy_g_param.postecho_thresh = [-20.0; VE_BANDS];
        vi
    }

    /// init の形：窓・帯域・大きさ。
    #[test]
    fn init_has_the_c_shape() {
        let e = ve_envelope_init(&info(2, [256, 2048]));
        assert!(lookup_is_valid(&e));
        assert_eq!(e.winlength, 128);
        assert_eq!(e.searchstep, 64);
        assert_eq!(e.cursor, 1024);
        assert_eq!(e.storage, 128);
        assert_eq!(e.mark.len(), 128);
        assert_eq!(e.filter.len(), 24);
        assert_eq!(e.minenergy, -80.0);
        // sin² の窓は両端 0 近く、真ん中で 1 近く、ほぼ左右対称
        assert_eq!(e.mdct_win[0], 0.0);
        assert!(e.mdct_win[127] < 1e-30);
        for i in 0..128 {
            assert!((e.mdct_win[i] - e.mdct_win[127 - i]).abs() < 1e-6);
        }
        assert!(e.mdct_win[64] > 0.99);
        for (j, b) in e.band.iter().enumerate() {
            assert_eq!(b.begin, BAND_BEGIN[j]);
            assert_eq!(b.window.len(), BAND_END[j] as usize);
            let sum: f32 = b.window.iter().sum();
            assert!((sum * b.total - 1.0).abs() < 1e-6);
        }
    }

    /// 無音では前エコーの印が付かず、急な立ち上がりでは前エコーの印（1|4）が付く。同じ入力なら同じ結果。
    /// （C と同じく `ampbuf` は 0 から始まるので、最初の数窓は無音でも後エコー（2）が付く。）
    #[test]
    fn amp_detects_an_attack() {
        let vi = info(1, [256, 2048]);
        let run = |signal: &[f32]| {
            let mut e = ve_envelope_init(&vi);
            let gi = vi.codec_setup.psy_g_param;
            let mut filter = std::mem::take(&mut e.filter);
            let mut s = AmpScratch::new(128);
            let mut rets = Vec::new();
            let mut j = 0;
            while j * 64 + 128 <= signal.len() {
                e.stretch = (e.stretch + 1).min(VE_MAXSTRETCH * 2);
                let r = ve_amp(
                    &e,
                    &gi,
                    &signal[j * 64..j * 64 + 128],
                    &e.band,
                    &mut filter,
                    &mut s,
                );
                if r & 4 != 0 {
                    e.stretch = -1;
                }
                rets.push(r);
                j += 1;
            }
            rets
        };
        let silence = vec![0.0_f32; 64 * 40];
        let rets = run(&silence);
        assert!(rets.iter().all(|&r| r & 5 == 0), "{rets:?}");
        assert!(rets[20..].iter().all(|&r| r == 0), "{rets:?}");
        let mut attack = vec![0.0_f32; 64 * 40];
        let mut r = crate::determinism::rng(3);
        use rand_chacha::rand_core::Rng;
        for x in &mut attack[64 * 25..] {
            *x = (f64::from(r.next_u32()) / 4_294_967_296.0 - 0.5) as f32;
        }
        let rets = run(&attack);
        assert!(rets[..20].iter().all(|&r| r & 5 == 0), "{rets:?}");
        assert!(rets[22..26].iter().any(|&r| r & 5 == 5), "{rets:?}");
        assert_eq!(rets, run(&attack));
    }

    /// shift は C の位置を引き、curmark は負になったら止まる。mark は mark_base で同じ並びに見える。
    #[test]
    fn shift_moves_positions_like_c() {
        let mut e = ve_envelope_init(&info(1, [256, 2048]));
        e.storage = 40;
        e.mark = vec![0; 40];
        e.mark[10] = 1;
        e.mark[11] = 1;
        e.current = 30 * 64;
        e.cursor = 20 * 64;
        e.curmark = 64;
        ve_envelope_shift(&mut e, 640).unwrap();
        assert_eq!(e.current, 20 * 64);
        assert_eq!(e.cursor, 10 * 64);
        assert_eq!(e.curmark, 64 - 640);
        assert_eq!(e.mark[e.mark_base], 1);
        assert_eq!(e.mark[e.mark_base + 1], 1);
        assert_eq!(e.mark[e.mark_base + 2], 0);
        assert!(e.mark.len() >= e.mark_base + 40);
        ve_envelope_shift(&mut e, 640).unwrap();
        // 負の curmark はもう引かない
        assert_eq!(e.curmark, 64 - 640);
        // C の配列の外へのずらしは Err
        e.current = 0;
        assert!(ve_envelope_shift(&mut e, 64 * 10).is_err());
    }
}
