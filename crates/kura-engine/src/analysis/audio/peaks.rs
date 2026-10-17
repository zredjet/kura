//! `audio.peaks` v1：波形の段（ビンごと・チャンネルごとの i16 の min・max）。
//!
//! - 段は 1 ビンあたり 256・1,024・4,096・16,384・65,536 フレーム（`PEAK_BINS`）。引数 `finest`・`coarsest`（ビンの
//!   フレーム数）で、その間の段だけを作る（既定は 5 段とも）。
//! - 標本 → i16 は 16 ビットで書き出すときと同じ格子（`codec::pcm::quantize(x, 16)`：x·32768 を偶数への丸め、
//!   [−32768, 32767] で止める、NaN は 0、±∞ は端）。16 ビットの元を読んだ値はファイルの整数そのものに戻る。
//! - 段 B のビン b は `[b·B, min((b+1)·B, frames))`。ビンの数は ⌈frames / B⌉（最後のビンは半端でもよい。長さ 0 なら 0 個）。
//!   最も細かい段だけを標本から作り、上の段のビン i は下の段のビン 4i〜4i+3（あるものだけ）の min の min・max の max
//!   （整数なので、並列の分け方・スレッド数・ターゲットによらない。⌈⌈n/a⌉/b⌉ = ⌈n/(ab)⌉ なので、半端なビンも標本から
//!   直接求めた値と同じ）。ビンの中は f32 の min・max（NaN を除く）を取ってから両端だけ格子に写す（格子への写しは
//!   単調なので、標本ごとに写したものと同じ。NaN があったビンは 0 も含める）。
//! - 結果の正規の形はバイナリ `kura-peaks` 版 1（リトルエンディアン。M11 の kura:// で配る形）：
//!
//!   ```text
//!   0   [8]  magic "kurapeak"
//!   8   u16  並びの版 = 1
//!   10  u16  解析の版（audio.peaks の VERSION）
//!   12  u16  チャンネル数 C
//!   14  u8   段の数 L（1〜16）
//!   15  u8   値の型 = 1（i16 の min・max の組）
//!   16  u32  サンプルレート
//!   20  u32  0（予約）
//!   24  u64  フレーム数 N
//!   32  L × 24 バイト：{ u32 ビンのフレーム数 B, u32 0, u64 ビンの数（= ⌈N/B⌉）, u64 データの位置 }
//!   …   各段のデータ：ビンの順 → チャンネルの順 → [min i16, max i16]（段の間は 8 バイト境界まで 0 で詰める）
//!   ```
//!
//!   ビンが外側・チャンネルが内側なので、範囲 [a, b) のビンは段ごとに 1 つの連続したバイト列になる（Range で取れる）。
//!   段の表に B と位置を書くので、一部の段だけを持つファイルも同じ並びで表せる。データの位置は先頭から
//!   8 の倍数（最初の段は 32 + 24L、次の段は前の段の終わりを 8 の倍数に切り上げた位置）、ファイルは最後の段の
//!   データで終わる。読む側（`from_canonical`）は外から来たバイト列として、magic・版・値の型・L・B が増えていくこと・
//!   ビンの数・位置・詰め物が 0・長さがちょうど・min ≤ max を確かめる（確保の前に大きさを照らす）。
//! - 整数だけ（`Numeric::Exact`）。

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::output::AnalysisOutput;
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Encoding, Estimate};
use crate::codec::pcm;
use crate::error::EngineError;
use crate::op::{Numeric, OpParams, ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Shape, ValueKind};
use crate::testgen::{InputGen, NonFiniteSample};
use crate::value::{Audio, Value};

/// 段のビンのフレーム数（版 1 で凍結）。
pub const PEAK_BINS: [u32; 5] = [256, 1_024, 4_096, 16_384, 65_536];

const MAGIC: &[u8; 8] = b"kurapeak";
/// 並びの版。
pub const FORMAT_VERSION: u16 = 1;
/// 値の型：i16 の min・max の組。
const VALUE_MIN_MAX_I16: u8 = 1;
const HEADER_BYTES: u64 = 32;
const LEVEL_ENTRY_BYTES: u64 = 24;
/// 並びが表せる段の数の上限。
pub const MAX_LEVELS: usize = 16;
/// 並列の仕事の大きさ（フレーム）。どの段のビンの大きさでも割り切れる（並列の分け方だけで、結果に効かない）。
const JOB_FRAMES: usize = 65_536;

/// 引数（読むときに、段の大きさが `PEAK_BINS` のどれかで `finest ≤ coarsest` かを確かめる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPeaksParams")]
pub struct PeaksParams {
    /// 最も細かい段のビンのフレーム数（256・1,024・4,096・16,384・65,536）。
    pub finest: u32,
    /// 最も粗い段のビンのフレーム数（同じ値のどれか。`finest` 以上）。
    pub coarsest: u32,
}

impl Default for PeaksParams {
    fn default() -> Self {
        Self {
            finest: PEAK_BINS[0],
            coarsest: PEAK_BINS[PEAK_BINS.len() - 1],
        }
    }
}

/// 読むときの形（知らない項目を拒む）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPeaksParams {
    finest: u32,
    coarsest: u32,
}

impl TryFrom<RawPeaksParams> for PeaksParams {
    type Error = String;

    fn try_from(r: RawPeaksParams) -> Result<Self, String> {
        if !PEAK_BINS.contains(&r.finest) || !PEAK_BINS.contains(&r.coarsest) {
            return Err(format!(
                "段のビンの大きさは {PEAK_BINS:?} のどれかです（{}・{}）",
                r.finest, r.coarsest
            ));
        }
        if r.finest > r.coarsest {
            return Err(format!(
                "finest（{}）は coarsest（{}）以下にします",
                r.finest, r.coarsest
            ));
        }
        Ok(Self {
            finest: r.finest,
            coarsest: r.coarsest,
        })
    }
}

impl OpParams for PeaksParams {
    fn schema() -> ParamSchema {
        let bins = ParamKind::Int {
            min: i64::from(PEAK_BINS[0]),
            max: i64::from(PEAK_BINS[PEAK_BINS.len() - 1]),
            step: 1,
        };
        ParamSchema {
            params: vec![
                spec("finest", bins.clone(), Unit::Samples, json!(PEAK_BINS[0])),
                spec(
                    "coarsest",
                    bins,
                    Unit::Samples,
                    json!(PEAK_BINS[PEAK_BINS.len() - 1]),
                ),
            ],
        }
    }
}

impl PeaksParams {
    /// 作る段のビンの大きさ（細かい順）。
    pub fn bins(&self) -> Vec<u32> {
        PEAK_BINS
            .iter()
            .copied()
            .filter(|b| (self.finest..=self.coarsest).contains(b))
            .collect()
    }
}

/// 段 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeakLevel {
    /// 1 ビンのフレーム数。
    pub bin: u32,
    /// ビンの順 → チャンネルの順 → [min, max]。長さは ⌈frames / bin⌉ × チャンネル数 × 2。
    pub data: Vec<i16>,
}

/// 結果（正規の形はバイナリ `kura-peaks`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peaks {
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// 段（ビンの細かい順）。
    pub levels: Vec<PeakLevel>,
}

impl Peaks {
    /// 段 `level` のビン `bin` のチャンネル `ch` の (min, max)。
    pub fn get(&self, level: usize, bin: u64, ch: u16) -> Option<(i16, i16)> {
        let l = self.levels.get(level)?;
        let at = usize::try_from(
            bin.checked_mul(u64::from(self.channels))?
                .checked_add(u64::from(ch))?
                .checked_mul(2)?,
        )
        .ok()?;
        Some((*l.data.get(at)?, *l.data.get(at + 1)?))
    }
}

/// ⌈n / b⌉（b > 0）。
fn bins_of(frames: u64, bin: u32) -> u64 {
    frames.div_ceil(u64::from(bin.max(1)))
}

fn align8(n: u64) -> Option<u64> {
    n.checked_add(7).map(|v| v & !7)
}

/// 段ごとの（ビンの数, データの位置, データのバイト数）。
type LevelLayout = (u64, u64, u64);

/// 段ごとの並びと全体の大きさ（バイト）。あふれたら None。
fn layout(frames: u64, channels: u16, bins: &[u32]) -> Option<(Vec<LevelLayout>, u64)> {
    let mut out = Vec::with_capacity(bins.len());
    let mut at = HEADER_BYTES.checked_add(LEVEL_ENTRY_BYTES.checked_mul(bins.len() as u64)?)?;
    let mut end = at;
    for (i, b) in bins.iter().enumerate() {
        if i > 0 {
            at = align8(end)?;
        }
        let n = bins_of(frames, *b);
        let bytes = n.checked_mul(u64::from(channels))?.checked_mul(4)?;
        end = at.checked_add(bytes)?;
        out.push((n, at, bytes));
    }
    Some((out, end))
}

/// 結果の正規の形の大きさ（バイト。あふれたら u64::MAX）。
pub fn encoded_len(frames: u64, channels: u16, bins: &[u32]) -> u64 {
    layout(frames, channels, bins).map_or(u64::MAX, |(_, total)| total)
}

fn decode_error(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: "kura-peaks".into(),
        message: message.into(),
    }
}

impl AnalysisOutput for Peaks {
    const ENCODING: Encoding = Encoding::Binary;

    fn to_canonical(&self) -> Result<Vec<u8>, EngineError> {
        let bad = |m: &str| EngineError::Internal(format!("波形の段が正しくありません：{m}"));
        if self.levels.is_empty() || self.levels.len() > MAX_LEVELS || self.channels == 0 {
            return Err(bad("段の数・チャンネル数"));
        }
        let bins: Vec<u32> = self.levels.iter().map(|l| l.bin).collect();
        if bins.contains(&0) || bins.windows(2).any(|w| w[0] >= w[1]) {
            return Err(bad("ビンの大きさ"));
        }
        let (table, total) =
            layout(self.frames, self.channels, &bins).ok_or_else(|| bad("大きさ"))?;
        let total = usize::try_from(total).map_err(|_| bad("大きさ"))?;
        let mut out = Vec::new();
        out.try_reserve_exact(total)
            .map_err(|_| EngineError::Limit("波形の段が大きすぎます".into()))?;
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&PeaksAnalysis::VERSION.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        out.push(self.levels.len() as u8);
        out.push(VALUE_MIN_MAX_I16);
        out.extend_from_slice(&self.rate.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        for (l, (n, at, _)) in self.levels.iter().zip(&table) {
            out.extend_from_slice(&l.bin.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&at.to_le_bytes());
        }
        for (l, (n, at, bytes)) in self.levels.iter().zip(&table) {
            if (l.data.len() as u64).checked_mul(2) != Some(*bytes)
                || n.checked_mul(u64::from(self.channels))
                    .and_then(|v| v.checked_mul(2))
                    != Some(l.data.len() as u64)
            {
                return Err(bad("データの長さ"));
            }
            // 段の間を 8 バイト境界まで 0 で詰める（位置は前の段の終わり以上）
            let at = usize::try_from(*at).map_err(|_| bad("位置"))?;
            if at < out.len() {
                return Err(bad("位置"));
            }
            out.resize(at, 0);
            for v in &l.data {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        if out.len() != total {
            return Err(bad("全体の長さ"));
        }
        Ok(out)
    }

    fn from_canonical(bytes: &[u8]) -> Result<Self, EngineError> {
        let u16_at = |at: usize| -> Result<u16, EngineError> {
            bytes
                .get(at..at + 2)
                .and_then(|s| s.first_chunk::<2>())
                .map(|b| u16::from_le_bytes(*b))
                .ok_or_else(|| decode_error("短すぎます"))
        };
        let u32_at = |at: usize| -> Result<u32, EngineError> {
            bytes
                .get(at..at + 4)
                .and_then(|s| s.first_chunk::<4>())
                .map(|b| u32::from_le_bytes(*b))
                .ok_or_else(|| decode_error("短すぎます"))
        };
        let u64_at = |at: usize| -> Result<u64, EngineError> {
            bytes
                .get(at..at + 8)
                .and_then(|s| s.first_chunk::<8>())
                .map(|b| u64::from_le_bytes(*b))
                .ok_or_else(|| decode_error("短すぎます"))
        };
        if bytes.get(..8) != Some(MAGIC.as_slice()) {
            return Err(decode_error("kura-peaks ではありません"));
        }
        if u16_at(8)? != FORMAT_VERSION {
            return Err(decode_error("知らない並びの版です"));
        }
        if u16_at(10)? != PeaksAnalysis::VERSION {
            return Err(decode_error("解析の版が違います"));
        }
        let channels = u16_at(12)?;
        let levels = usize::from(*bytes.get(14).ok_or_else(|| decode_error("短すぎます"))?);
        let kind = *bytes.get(15).ok_or_else(|| decode_error("短すぎます"))?;
        let rate = u32_at(16)?;
        let reserved = u32_at(20)?;
        let frames = u64_at(24)?;
        if channels == 0 || levels == 0 || levels > MAX_LEVELS {
            return Err(decode_error("チャンネル数・段の数が正しくありません"));
        }
        if kind != VALUE_MIN_MAX_I16 || reserved != 0 {
            return Err(decode_error("値の型・予約の欄が正しくありません"));
        }
        let mut bins = Vec::with_capacity(levels);
        let mut entries = Vec::with_capacity(levels);
        for i in 0..levels {
            let at = 32 + 24 * i;
            let (b, zero, n, pos) = (
                u32_at(at)?,
                u32_at(at + 4)?,
                u64_at(at + 8)?,
                u64_at(at + 16)?,
            );
            if b == 0 || zero != 0 || bins.last().is_some_and(|last| *last >= b) {
                return Err(decode_error("段の表が正しくありません"));
            }
            bins.push(b);
            entries.push((n, pos));
        }
        let (table, total) =
            layout(frames, channels, &bins).ok_or_else(|| decode_error("大きさがあふれます"))?;
        if total != bytes.len() as u64 {
            return Err(decode_error("長さが合いません"));
        }
        let mut out = Vec::with_capacity(levels);
        let mut cursor = 32 + 24 * levels;
        for ((b, (n, pos)), (want_n, want_pos, len)) in bins.iter().zip(&entries).zip(&table) {
            if n != want_n || pos != want_pos {
                return Err(decode_error("ビンの数・データの位置が正しくありません"));
            }
            let pos = usize::try_from(*pos).map_err(|_| decode_error("位置"))?;
            let len = usize::try_from(*len).map_err(|_| decode_error("長さ"))?;
            // 段の間の詰め物は 0
            if bytes
                .get(cursor..pos)
                .is_none_or(|pad| pad.iter().any(|b| *b != 0))
            {
                return Err(decode_error("詰め物が正しくありません"));
            }
            let raw = bytes
                .get(pos..pos + len)
                .ok_or_else(|| decode_error("短すぎます"))?;
            let mut data = Vec::new();
            data.try_reserve_exact(len / 2)
                .map_err(|_| EngineError::Limit("波形の段が大きすぎます".into()))?;
            for pair in raw.chunks_exact(4) {
                let lo = i16::from_le_bytes([pair[0], pair[1]]);
                let hi = i16::from_le_bytes([pair[2], pair[3]]);
                if lo > hi {
                    return Err(decode_error("min が max より大きい"));
                }
                data.push(lo);
                data.push(hi);
            }
            out.push(PeakLevel { bin: *b, data });
            cursor = pos + len;
        }
        Ok(Self {
            rate,
            channels,
            frames,
            levels: out,
        })
    }
}

/// 標本を 16 ビットの格子に写す（`codec::pcm::quantize(x, 16)`）。
pub fn to_i16(x: f32) -> i16 {
    i16::try_from(pcm::quantize(x, 16)).unwrap_or(0)
}

/// 標本の列の (min, max)（16 ビットの格子）。空なら None。
fn min_max(xs: &[f32]) -> Option<(i16, i16)> {
    if xs.is_empty() {
        return None;
    }
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut nan, mut any) = (false, false);
    for &v in xs {
        if v.is_nan() {
            nan = true;
            continue;
        }
        any = true;
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
    }
    if !any {
        return Some((0, 0));
    }
    let (mut lo, mut hi) = (to_i16(lo), to_i16(hi));
    if nan {
        lo = lo.min(0);
        hi = hi.max(0);
    }
    Some((lo, hi))
}

/// 波形の段を作る（引数の段だけ）。
pub fn compute(a: &Audio, p: &PeaksParams, cx: &AnalyzeCx<'_>) -> Result<Peaks, EngineError> {
    let bins = p.bins();
    let Some(&finest) = bins.first() else {
        return Err(EngineError::Internal("波形の段がありません".into()));
    };
    let channels = u16::try_from(a.channels())
        .map_err(|_| EngineError::Internal("チャンネルが多すぎます".into()))?;
    let c = a.channels();
    let frames = a.frames();
    let b0 = finest as usize;
    let jobs = frames.div_ceil(JOB_FRAMES);
    let tasks = c
        .checked_mul(jobs)
        .ok_or_else(|| EngineError::Limit("音声が長すぎます".into()))?;
    let planes = a.planes();
    // 最も細かい段：チャンネル × 仕事（JOB_FRAMES フレーム。どのビンの大きさでも割り切れる）
    let parts = cx.par_blocks(tasks, |t| {
        let (ch, j) = (t / jobs.max(1), t % jobs.max(1));
        let Some(plane) = planes.get(ch) else {
            return Err(EngineError::Internal(
                "波形の仕事の番号が範囲の外です".into(),
            ));
        };
        let start = j * JOB_FRAMES;
        let end = start.saturating_add(JOB_FRAMES).min(frames);
        let xs = plane.get(start..end).unwrap_or(&[]);
        let mut out = Vec::new();
        out.try_reserve_exact(xs.len().div_ceil(b0))
            .map_err(|_| EngineError::Limit("波形の作業領域を確保できません".into()))?;
        out.extend(xs.chunks(b0).filter_map(min_max));
        Ok(out)
    })?;
    // 仕事 (c, j) の結果は parts[c·jobs + j]。ビンの順 → チャンネルの順に並べる（仕事の境はビンの境）
    let n0 = bins_of(frames as u64, finest);
    let n0 = usize::try_from(n0).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let mut level0 = cx.reserve::<i16>(n0.saturating_mul(c).saturating_mul(2))?;
    let missing = || EngineError::Internal("波形のビンが足りません".into());
    for j in 0..jobs {
        let len = parts.get(j).map_or(0, Vec::len);
        for b in 0..len {
            for ch in 0..c {
                let (lo, hi) = parts
                    .get(ch * jobs + j)
                    .and_then(|p| p.get(b))
                    .copied()
                    .ok_or_else(missing)?;
                level0.push(lo);
                level0.push(hi);
            }
        }
    }
    if level0.len() != n0 * c * 2 {
        return Err(missing());
    }
    drop(parts);
    let mut levels = vec![PeakLevel {
        bin: finest,
        data: level0,
    }];
    // 上の段：下の段の ratio（= 4）個ずつ
    for &bin in bins.iter().skip(1) {
        cx.check()?;
        let Some(prev) = levels.last() else { break };
        let ratio = (bin / prev.bin.max(1)) as usize;
        let prev_n = prev.data.len() / (2 * c);
        let n = bins_of(frames as u64, bin);
        let n = usize::try_from(n).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        if ratio == 0 || n != prev_n.div_ceil(ratio) {
            return Err(EngineError::Internal("波形の段の大きさが合いません".into()));
        }
        let mut data = cx.reserve::<i16>(n.saturating_mul(c).saturating_mul(2))?;
        for i in 0..n {
            let first = i * ratio;
            let last = (first + ratio).min(prev_n);
            for ch in 0..c {
                let (mut lo, mut hi) = (i16::MAX, i16::MIN);
                for k in first..last {
                    let at = (k * c + ch) * 2;
                    if let (Some(l), Some(h)) = (prev.data.get(at), prev.data.get(at + 1)) {
                        lo = lo.min(*l);
                        hi = hi.max(*h);
                    }
                }
                data.push(lo);
                data.push(hi);
            }
        }
        levels.push(PeakLevel { bin, data });
    }
    Ok(Peaks {
        rate: a.rate(),
        channels,
        frames: frames as u64,
        levels,
    })
}

/// 波形の段。
pub struct PeaksAnalysis;

impl Analysis for PeaksAnalysis {
    const ID: &'static str = "audio.peaks";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
    const NUMERIC: Numeric = Numeric::Exact;
    type Params = PeaksParams;
    type Output = Peaks;

    fn run(p: &PeaksParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<Peaks, EngineError> {
        cx.check()?;
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(format!(
                "{} の入力が音声ではありません",
                Self::ID
            )));
        };
        compute(a, p, cx)
    }

    fn examples() -> Vec<AnalysisExample<PeaksParams>> {
        let all = PeaksParams::default();
        vec![
            // どの段にも半端なビンがある（65,536 の段は 2 ビン）
            AnalysisExample {
                name: "noise-stereo-partial-bins",
                params: all,
                input: InputGen::AudioNoise {
                    rate: 44_100,
                    channels: 2,
                    frames: 70_001,
                    seed: 91,
                },
            },
            // 1 を超える値（端に止まる）
            AnalysisExample {
                name: "swell-over-full-scale",
                params: all,
                input: InputGen::Swell {
                    rate: 22_050,
                    channels: 1,
                    frames: 40_000,
                    freqs: vec![300, 1_100],
                    from_db: -30,
                    to_db: 6,
                },
            },
            // 粗い 3 段だけ
            AnalysisExample {
                name: "coarse-levels",
                params: PeaksParams {
                    finest: 4_096,
                    coarsest: 65_536,
                },
                input: InputGen::Sine {
                    rate: 48_000,
                    channels: 3,
                    frames: 50_000,
                    freq: 440,
                    loop_range: None,
                },
            },
            // 1 段だけ
            AnalysisExample {
                name: "one-level",
                params: PeaksParams {
                    finest: 1_024,
                    coarsest: 1_024,
                },
                input: InputGen::Bursts {
                    rate: 16_000,
                    channels: 2,
                    frames: 9_000,
                    period: 3_000,
                    seed: 92,
                },
            },
            // 1 ビンに満たない
            AnalysisExample {
                name: "shorter-than-a-bin",
                params: all,
                input: InputGen::AudioNoise {
                    rate: 8_000,
                    channels: 1,
                    frames: 100,
                    seed: 93,
                },
            },
            // 長さ 0：どの段もビン 0 個
            AnalysisExample {
                name: "empty",
                params: all,
                input: InputGen::AudioNoise {
                    rate: 48_000,
                    channels: 2,
                    frames: 0,
                    seed: 94,
                },
            },
            // NaN・±∞（ゆっくりした正の正弦の上に）：NaN のあるビンは 0 も含み、NaN だけのビン（最後の 1 フレーム）は
            // (0, 0)、±∞ は格子の端
            AnalysisExample {
                name: "non-finite-bins",
                params: PeaksParams {
                    finest: 256,
                    coarsest: 1_024,
                },
                input: InputGen::NonFinite {
                    base: Box::new(InputGen::Sine {
                        rate: 48_000,
                        channels: 2,
                        frames: 1_025,
                        freq: 1,
                        loop_range: None,
                    }),
                    at: vec![
                        (0, 300, NonFiniteSample::Nan),
                        (0, 1_024, NonFiniteSample::Nan),
                        (1, 10, NonFiniteSample::Inf),
                        (1, 700, NonFiniteSample::NegInf),
                    ],
                },
            },
        ]
    }

    fn estimate(p: &PeaksParams, input: &Shape) -> Estimate {
        let Shape::Audio {
            channels, frames, ..
        } = *input
        else {
            return Estimate::default();
        };
        let frames = frames.max();
        Estimate {
            bytes: encoded_len(frames, channels, &p.bins()),
            cost: frames.saturating_mul(u64::from(channels)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::value::AudioMeta;
    use rand_chacha::rand_core::Rng;
    use std::sync::Arc;

    fn cx_run(a: &Audio, p: &PeaksParams) -> Peaks {
        let limits = crate::Limits::default();
        compute(
            a,
            p,
            &AnalyzeCx {
                mon: &NoMonitor,
                limits: &limits,
            },
        )
        .unwrap()
    }

    /// 段 B を標本から直接（標本ごとに格子へ写して）求めたもの。
    fn direct(x: &[f32], bin: usize) -> Vec<(i16, i16)> {
        x.chunks(bin)
            .map(|c| {
                c.iter().fold((i16::MAX, i16::MIN), |(lo, hi), v| {
                    (lo.min(to_i16(*v)), hi.max(to_i16(*v)))
                })
            })
            .collect()
    }

    fn audio(planes: Vec<Vec<f32>>) -> Audio {
        Audio::new(
            44_100,
            planes.into_iter().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap()
    }

    fn noisy(frames: usize, channels: usize, seed: u64) -> Audio {
        let mut r = crate::determinism::rng(seed);
        let specials = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            1.0,
            -1.0,
            1.5,
            -0.0,
            32_766.5 / 32_768.0,
            -32_767.5 / 32_768.0,
        ];
        audio(
            (0..channels)
                .map(|_| {
                    (0..frames)
                        .map(|_| {
                            let u = r.next_u32();
                            if u.is_multiple_of(997) {
                                specials[(u as usize / 997) % specials.len()]
                            } else {
                                (u >> 8) as f32 / 8_388_608.0 - 1.0
                            }
                        })
                        .collect()
                })
                .collect(),
        )
    }

    /// 下の段から作った段 = 標本から直接求めた段（すべての段・半端なビン・端の長さ・NaN・±∞・±1・格子の境）。
    #[test]
    fn levels_match_the_direct_computation() {
        for (frames, channels, seed) in [
            (0usize, 1usize, 1u64),
            (1, 2, 2),
            (255, 1, 3),
            (256, 1, 4),
            (257, 3, 5),
            (65_535, 2, 6),
            (65_536, 1, 7),
            (65_537, 2, 8),
            (300_001, 1, 9),
        ] {
            let a = noisy(frames, channels, seed);
            let p = cx_run(&a, &PeaksParams::default());
            assert_eq!(p.levels.len(), 5);
            assert_eq!(p.frames, frames as u64);
            for (k, l) in p.levels.iter().enumerate() {
                assert_eq!(l.bin, PEAK_BINS[k]);
                for ch in 0..channels {
                    let want = direct(&a.planes()[ch], l.bin as usize);
                    let got: Vec<(i16, i16)> = (0..want.len() as u64)
                        .map(|b| p.get(k, b, ch as u16).unwrap())
                        .collect();
                    assert_eq!(got, want, "{frames} フレーム・段 {}・ch {ch}", l.bin);
                    assert_eq!(p.get(k, want.len() as u64, ch as u16), None);
                }
            }
            // 一部の段だけでも同じ値
            let coarse = cx_run(
                &a,
                &PeaksParams {
                    finest: 4_096,
                    coarsest: 16_384,
                },
            );
            assert_eq!(coarse.levels, p.levels[2..4].to_vec());
        }
    }

    /// 例の値を直接確かめる（ゴールデンに凍結する前に）：NaN・±∞ を置いた例は、標本ごとに格子へ写して求めた段と同じで、
    /// NaN のあるビンは 0 を含み、NaN だけのビンは (0, 0)、±∞ は端。
    #[test]
    fn non_finite_example() {
        let ex = PeaksAnalysis::examples()
            .into_iter()
            .find(|e| e.name == "non-finite-bins")
            .unwrap();
        let Value::Audio(a) = ex.input.generate().unwrap() else {
            panic!()
        };
        let p = cx_run(&a, &ex.params);
        assert_eq!(
            p.levels.iter().map(|l| l.bin).collect::<Vec<_>>(),
            [256, 1_024]
        );
        for (k, l) in p.levels.iter().enumerate() {
            for ch in 0..2 {
                let want = direct(&a.planes()[ch], l.bin as usize);
                let got: Vec<(i16, i16)> = (0..want.len() as u64)
                    .map(|b| p.get(k, b, ch as u16).unwrap())
                    .collect();
                assert_eq!(got, want, "段 {}・ch {ch}", l.bin);
            }
        }
        // ch 0：ビン 1 は正の正弦と NaN（最小は 0）、ビン 4 は NaN だけ。ch 1：ビン 0 は +∞、ビン 2 は −∞
        let (lo, hi) = p.get(0, 1, 0).unwrap();
        assert!(lo == 0 && hi > 0, "{lo} {hi}");
        assert!(
            a.planes()[0][256..512]
                .iter()
                .all(|v| v.is_nan() || *v > 0.0)
        );
        assert_eq!(p.get(0, 4, 0), Some((0, 0)));
        assert_eq!(p.get(1, 1, 0), Some((0, 0)));
        assert_eq!(p.get(0, 0, 1).map(|b| b.1), Some(32_767));
        assert_eq!(p.get(0, 2, 1).map(|b| b.0), Some(-32_768));
    }

    /// 格子：16 ビットの元を読んだ値（v/32768）はファイルの整数に戻る。端・NaN・±∞・偶数への丸め。
    #[test]
    fn the_grid_is_the_16_bit_export_grid() {
        for v in [-32_768i32, -32_767, -1, 0, 1, 12_345, 32_766, 32_767] {
            assert_eq!(i32::from(to_i16(v as f32 / 32_768.0)), v);
        }
        assert_eq!(to_i16(1.0), 32_767);
        assert_eq!(to_i16(-1.0), -32_768);
        assert_eq!(to_i16(7.0), 32_767);
        assert_eq!(to_i16(f32::INFINITY), 32_767);
        assert_eq!(to_i16(f32::NEG_INFINITY), -32_768);
        assert_eq!(to_i16(f32::NAN), 0);
        assert_eq!(to_i16(-0.0), 0);
        // 32,766.5 は偶数（32,766）へ、0.5 は 0 へ、1.5 は 2 へ
        assert_eq!(to_i16(32_766.5 / 32_768.0), 32_766);
        assert_eq!(to_i16(0.5 / 32_768.0), 0);
        assert_eq!(to_i16(1.5 / 32_768.0), 2);
        // NaN だけのビンは (0, 0)。NaN と正の値のビンは 0 を含む
        let a = audio(vec![vec![f32::NAN; 300]]);
        assert_eq!(
            cx_run(&a, &PeaksParams::default()).get(0, 0, 0),
            Some((0, 0))
        );
        let mut x = vec![0.5f32; 256];
        x[3] = f32::NAN;
        let a = audio(vec![x]);
        assert_eq!(
            cx_run(&a, &PeaksParams::default()).get(0, 0, 0),
            Some((0, 16_384))
        );
    }

    /// 16 ビットの WAV に書いて読んだ値の段は、ファイルの整数の min・max と同じ。
    #[test]
    fn wav_16_bit_values_are_the_file_integers() {
        let a = noisy(5_000, 2, 21);
        let v = Value::Audio(Arc::new(a));
        let set = crate::codec::CodecSet::pinned();
        let (bytes, _) = set
            .encode_bytes(&v, "wav", &json!({ "bits": 16 }), &NoMonitor)
            .unwrap();
        let (back, _) = set
            .decode(
                &mut std::io::Cursor::new(bytes.clone()),
                "wav",
                &Default::default(),
                &crate::Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Audio(back) = back else { panic!() };
        // data チャンクの整数（ステレオのインターリーブ）
        let at = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
        let ints: Vec<i16> = bytes[at..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        let p = cx_run(&back, &PeaksParams::default());
        for ch in 0..2usize {
            let col: Vec<i16> = ints.iter().skip(ch).step_by(2).copied().collect();
            for (b, chunk) in col.chunks(256).enumerate() {
                let want = (*chunk.iter().min().unwrap(), *chunk.iter().max().unwrap());
                assert_eq!(p.get(0, b as u64, ch as u16), Some(want));
            }
        }
    }

    /// バイナリの並び：既知の小さな例のバイト列・往復・一部の段・凍結した大きさ。
    #[test]
    fn the_binary_layout_is_frozen() {
        let a = audio(vec![vec![0.5, -0.25, 1.0], vec![-1.0, 0.0, f32::NAN]]);
        let p = cx_run(
            &a,
            &PeaksParams {
                finest: 256,
                coarsest: 1_024,
            },
        );
        let bytes = p.to_canonical().unwrap();
        let mut want = Vec::new();
        want.extend_from_slice(b"kurapeak");
        want.extend_from_slice(&[1, 0, 1, 0, 2, 0, 2, 1]);
        want.extend_from_slice(&44_100u32.to_le_bytes());
        want.extend_from_slice(&[0; 4]);
        want.extend_from_slice(&3u64.to_le_bytes());
        // 段の表（32 + 48 = 80 から。データは 1 ビン × 2 ch × 4 = 8 バイト）
        for (bin, at) in [(256u32, 80u64), (1_024, 88)] {
            want.extend_from_slice(&bin.to_le_bytes());
            want.extend_from_slice(&[0; 4]);
            want.extend_from_slice(&1u64.to_le_bytes());
            want.extend_from_slice(&at.to_le_bytes());
        }
        for _ in 0..2 {
            for v in [-8_192i16, 32_767, -32_768, 0] {
                want.extend_from_slice(&v.to_le_bytes());
            }
        }
        assert_eq!(bytes, want);
        assert_eq!(Peaks::from_canonical(&bytes).unwrap(), p);
        assert_eq!(encoded_len(3, 2, &[256, 1_024]), bytes.len() as u64);
        // 詰め物：1 チャンネル・1 ビンは 4 バイトなので、次の段は 8 の倍数に切り上げる
        let mono = cx_run(&audio(vec![vec![0.1; 10]]), &PeaksParams::default());
        let bytes = mono.to_canonical().unwrap();
        assert_eq!(bytes.len() as u64, 32 + 24 * 5 + 8 * 4 + 4);
        assert_eq!(Peaks::from_canonical(&bytes).unwrap(), mono);
        // 5 分 44.1 kHz ステレオの 5 段（ヘッダ込み）
        assert_eq!(encoded_len(44_100 * 300, 2, &PEAK_BINS), 550_872);
        // 4,096 以上の 3 段だけ
        assert_eq!(encoded_len(44_100 * 300, 2, &PEAK_BINS[2..]), 34_024);
    }

    /// 壊れたバイナリは落ちずにエラー（ChaCha8 で変異。長さの不一致・巨大な見出し・詰め物・min > max）。
    #[test]
    fn broken_binaries_are_errors() {
        let p = cx_run(&noisy(70_001, 2, 31), &PeaksParams::default());
        let good = p.to_canonical().unwrap();
        let mut r = crate::determinism::rng(32);
        for _ in 0..3_000 {
            let mut b = good.clone();
            match r.next_u32() % 4 {
                0 => {
                    let at = r.next_u32() as usize % b.len();
                    b[at] ^= 1 << (r.next_u32() % 8);
                }
                1 => b.truncate(r.next_u32() as usize % b.len()),
                2 => {
                    // 見出し・段の表を書き換える
                    let at = r.next_u32() as usize % 152;
                    b[at] = r.next_u32() as u8;
                }
                _ => b.push(r.next_u32() as u8),
            }
            if let Ok(back) = Peaks::from_canonical(&b) {
                // 読めたなら正規の形（同じバイト列に戻る）
                assert_eq!(back.to_canonical().unwrap(), b);
            }
        }
        for bad in [
            Vec::new(),
            b"kurapeak".to_vec(),
            good[..good.len() - 1].to_vec(),
        ] {
            assert!(Peaks::from_canonical(&bad).is_err());
        }
        // フレーム数を巨大に（確保の前に長さで断る）
        let mut huge = good.clone();
        huge[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Peaks::from_canonical(&huge).is_err());
        // min > max
        let mut swapped = good.clone();
        let at = 32 + 24 * 5;
        swapped[at..at + 4].copy_from_slice(&[0xff, 0x7f, 0x00, 0x80]);
        assert!(Peaks::from_canonical(&swapped).is_err());
        // 段の表の順が逆
        let mut order = good.clone();
        order[32..36].copy_from_slice(&70_000u32.to_le_bytes());
        assert!(Peaks::from_canonical(&order).is_err());
    }

    /// 引数：段は PEAK_BINS のどれか・finest ≤ coarsest。知らない項目は拒む。
    #[test]
    fn params_are_checked() {
        let ok = |v: serde_json::Value| serde_json::from_value::<PeaksParams>(v).is_ok();
        assert!(ok(json!({ "finest": 256, "coarsest": 65_536 })));
        assert!(ok(json!({ "finest": 4_096, "coarsest": 4_096 })));
        assert!(!ok(json!({ "finest": 512, "coarsest": 65_536 })));
        assert!(!ok(json!({ "finest": 4_096, "coarsest": 1_024 })));
        assert!(!ok(json!({ "finest": 256, "coarsest": 65_536, "x": 1 })));
        assert_eq!(PeaksParams::default().bins(), PEAK_BINS.to_vec());
    }
}
