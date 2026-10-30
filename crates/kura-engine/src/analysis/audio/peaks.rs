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
//! - 部分更新（M13 段 1）：どの段のビンもチャンク（65,536 フレーム）の中で閉じるので、変わったチャンクのビンだけを作り直せる
//!   （[`patch`]・流れに載せる [`Patcher`]）。結果は `compute` と同じ（整数だけ）。

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::output::AnalysisOutput;
use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, Encoding, Estimate};
use crate::audio::stream::{AudioSink, StreamInfo};
use crate::codec::pcm;
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::op::{Numeric, OpParams, ParamKind, ParamSchema, Unit, spec};
use crate::render::region::{Area, Spans};
use crate::shape::{Shape, ValueKind};
use crate::testgen::{InputGen, NonFiniteSample};
use crate::value::{Audio, ChunkSet, Value};

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
/// 並列の仕事の大きさ（フレーム）。音声のチャンクと同じで、どの段のビンの大きさでも割り切れる（並列の分け方だけで、
/// 結果に効かない）。
const JOB_FRAMES: usize = crate::value::AUDIO_CHUNK;
const _: () = assert!(JOB_FRAMES.is_multiple_of(PEAK_BINS[PEAK_BINS.len() - 1] as usize));

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
            for pair in raw.as_chunks::<4>().0 {
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

/// チャンク 1 つ（全チャンネルの中身。長さはそろう）の最も細かい段のビン（ビンの順 → チャンネルの順 → [min, max]）。
/// どの段のビンもチャンクの中で閉じる（`PEAK_BINS` は `AUDIO_CHUNK` の約数）ので、チャンクごとに作って並べれば全体の段になる。
fn chunk_finest<C: Deref<Target = [f32]>>(data: &[C], b0: usize) -> Result<Vec<i16>, EngineError> {
    let len = data.first().map_or(0, |d| d.len());
    if data.iter().any(|d| d.len() != len) {
        return Err(EngineError::Internal(
            "波形のチャンクのチャンネルの長さがそろっていません".into(),
        ));
    }
    let bins = len.div_ceil(b0.max(1));
    let mut out = Vec::new();
    out.try_reserve_exact(bins.saturating_mul(data.len()).saturating_mul(2))
        .map_err(|_| EngineError::Limit("波形の作業領域を確保できません".into()))?;
    for b in 0..bins {
        let (s, e) = (b * b0, ((b + 1) * b0).min(len));
        for d in data {
            let (lo, hi) = min_max(&d[s..e]).unwrap_or((0, 0));
            out.push(lo);
            out.push(hi);
        }
    }
    Ok(out)
}

/// 段のビン i（ビンの大きさ `bin`）が属するチャンクの番号。
fn chunk_of(i: usize, bin: u32) -> usize {
    i.saturating_mul(bin as usize) / JOB_FRAMES
}

/// 下の段 `prev` から上の段（ビンの大きさ `bin`）を作る（下の段の ratio 個ずつの min の min・max の max）。`reuse` が
/// あれば、`R` に属さないチャンクのビンは前の段（同じ大きさ・同じ位置）から写す（波形の部分更新）。
fn upper_level(
    prev: &PeakLevel,
    bin: u32,
    frames: u64,
    c: usize,
    reuse: Option<(&PeakLevel, &ChunkSet)>,
    mon: &dyn Monitor,
) -> Result<PeakLevel, EngineError> {
    if mon.canceled() {
        return Err(EngineError::Canceled);
    }
    let ratio = (bin / prev.bin.max(1)) as usize;
    let prev_n = prev.data.len() / (2 * c).max(1);
    let n = bins_of(frames, bin);
    let n = usize::try_from(n).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    if ratio == 0 || n != prev_n.div_ceil(ratio) {
        return Err(EngineError::Internal("波形の段の大きさが合いません".into()));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(n.saturating_mul(c).saturating_mul(2))
        .map_err(|_| EngineError::Limit("波形の作業領域を確保できません".into()))?;
    for i in 0..n {
        if let Some((old, r)) = reuse
            && !r.contains(chunk_of(i, bin))
        {
            let at = i * c * 2;
            let src = old
                .data
                .get(at..at + c * 2)
                .ok_or_else(|| EngineError::Internal("前の波形の段のビンが足りません".into()))?;
            data.extend_from_slice(src);
            continue;
        }
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
    Ok(PeakLevel { bin, data })
}

/// 波形の段を作る（引数の段だけ）。仕事はチャンクの番号ごと（全チャンネル。遅延の源では 1 回の読みが全チャンネルに効く）。
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
    // 最も細かい段：チャンク（JOB_FRAMES フレーム。どのビンの大きさでも割り切れる）ごとに全チャンネル
    let parts = cx.par_blocks(jobs, |j| {
        let data = (0..c)
            .map(|ch| a.chunk_data(ch, j))
            .collect::<Result<Vec<_>, EngineError>>()?;
        chunk_finest(&data, b0)
    })?;
    // チャンク j の結果はビンの順 → チャンネルの順。チャンクの順に並べれば全体の段（仕事の境はビンの境）
    let n0 = bins_of(frames as u64, finest);
    let n0 = usize::try_from(n0).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let mut level0 = cx.reserve::<i16>(n0.saturating_mul(c).saturating_mul(2))?;
    for part in &parts {
        level0.extend_from_slice(part);
    }
    if level0.len() != n0 * c * 2 {
        return Err(EngineError::Internal("波形のビンが足りません".into()));
    }
    drop(parts);
    let mut levels = vec![PeakLevel {
        bin: finest,
        data: level0,
    }];
    // 上の段：下の段の ratio（= 4）個ずつ
    for &bin in bins.iter().skip(1) {
        let Some(prev) = levels.last() else { break };
        let next = upper_level(prev, bin, frames as u64, c, None, cx.mon)?;
        levels.push(next);
    }
    Ok(Peaks {
        rate: a.rate(),
        channels,
        frames: frames as u64,
        levels,
    })
}

/// 波形の部分更新で作り直すチャンク R（M13 段 1）：長さ `frames` の新しい値で、`changed` に掛かるチャンク。長さが変わったら
/// （前の長さ `old_frames`）、新しい長さの最後のチャンクと、旧い長さの最後のチャンクの番号から先も入れる（保守的）。
pub fn patch_chunks(old_frames: u64, frames: u64, changed: &Spans) -> ChunkSet {
    let fr = usize::try_from(frames).unwrap_or(usize::MAX);
    let mut r = crate::render::audio::chunks_covering(&Area::samples(changed.clone()), fr);
    if old_frames != frames {
        let count = r.universe();
        let old_last = old_frames.div_ceil(JOB_FRAMES as u64).saturating_sub(1);
        let new_last = (count as u64).saturating_sub(1);
        let from = usize::try_from(old_last.min(new_last)).unwrap_or(usize::MAX);
        for j in from..count {
            r.insert(j);
        }
    }
    r
}

/// 前の段 `old` と、R のチャンクの最も細かい段のビン（`fresh`）から、長さ `frames` の新しい値の段を組む。R に属さない
/// チャンクのビンは前の段の同じ位置から写す（整数だけなので、`compute` と同じ）。
fn assemble(
    old: &Peaks,
    p: &PeaksParams,
    frames: u64,
    r: &ChunkSet,
    fresh: &BTreeMap<usize, Vec<i16>>,
    mon: &dyn Monitor,
) -> Result<Peaks, EngineError> {
    let bins = p.bins();
    if old.levels.iter().map(|l| l.bin).collect::<Vec<_>>() != bins {
        return Err(EngineError::Internal(
            "前の波形の段が引数の段と違います".into(),
        ));
    }
    let Some(&finest) = bins.first() else {
        return Err(EngineError::Internal("波形の段がありません".into()));
    };
    let c = usize::from(old.channels);
    let b0 = finest as usize;
    let per = JOB_FRAMES / b0.max(1);
    let fr = usize::try_from(frames).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let jobs = fr.div_ceil(JOB_FRAMES);
    let n0 = usize::try_from(bins_of(frames, finest))
        .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    let mut level0 = Vec::new();
    level0
        .try_reserve_exact(n0.saturating_mul(c).saturating_mul(2))
        .map_err(|_| EngineError::Limit("波形の作業領域を確保できません".into()))?;
    let old0 = &old.levels[0];
    for j in 0..jobs {
        if r.contains(j) {
            let part = fresh.get(&j).ok_or_else(|| {
                EngineError::Internal(format!("作り直す波形のチャンク {j} がありません"))
            })?;
            level0.extend_from_slice(part);
            continue;
        }
        // R の外のチャンクは前と同じ長さ（R の作り方による）。前の段の同じビンを写す
        let len = (fr - j * JOB_FRAMES).min(JOB_FRAMES);
        let (s, e) = (j * per, j * per + len.div_ceil(b0));
        let src = old0
            .data
            .get(s * c * 2..e * c * 2)
            .ok_or_else(|| EngineError::Internal("前の波形の段のビンが足りません".into()))?;
        level0.extend_from_slice(src);
    }
    if level0.len() != n0 * c * 2 {
        return Err(EngineError::Internal("波形のビンが足りません".into()));
    }
    let mut levels = vec![PeakLevel {
        bin: finest,
        data: level0,
    }];
    for (k, &bin) in bins.iter().enumerate().skip(1) {
        let Some(prev) = levels.last() else { break };
        let next = upper_level(prev, bin, frames, c, Some((&old.levels[k], r)), mon)?;
        levels.push(next);
    }
    Ok(Peaks {
        rate: old.rate,
        channels: old.channels,
        frames,
        levels,
    })
}

/// 波形の段の部分更新（M13 段 1。docs/editing.md §11.4）：前の段 `old`（引数 `p`）から、長さ `frames` の新しい値の段を、
/// 変わった区間 `changed`（保守的でよい）に掛かるチャンク R（[`patch_chunks`]）だけ標本から作り直して求める。`chunk(j)` は
/// 新しい値のチャンク j の全チャンネルの中身（R のチャンクだけ呼ぶ）。結果は新しい値を `compute` したものと同じ（E40）。
/// レート・チャンネルの数は前と同じであること。
pub fn patch<C: Deref<Target = [f32]>>(
    old: &Peaks,
    p: &PeaksParams,
    frames: u64,
    changed: &Spans,
    mut chunk: impl FnMut(u32) -> Result<Vec<C>, EngineError>,
    cx: &AnalyzeCx<'_>,
) -> Result<Peaks, EngineError> {
    let r = patch_chunks(old.frames, frames, changed);
    let b0 = p.bins().first().copied().unwrap_or(PEAK_BINS[0]) as usize;
    let mut fresh = BTreeMap::new();
    for j in r.iter() {
        cx.check()?;
        let index = u32::try_from(j).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        let data = chunk(index)?;
        if data.len() != usize::from(old.channels) {
            return Err(EngineError::Internal(
                "波形のチャンクのチャンネルの数が違います".into(),
            ));
        }
        fresh.insert(j, chunk_finest(&data, b0)?);
    }
    assemble(old, p, frames, &r, &fresh, cx.mon)
}

/// 波形の段の部分更新を流れに載せるもの（`AudioSink`）：`Engine::stream_audio` に [`Patcher::spans`]（R のチャンクの区間）を
/// 渡して流すと、R のチャンクのビンを作り、`finish` で段を組む（[`Patcher::into_peaks`] で取り出す）。表示の中のチャンクは
/// 表示の値から渡してもよい（流れも区間も同じ値）。
pub struct Patcher {
    old: Peaks,
    params: PeaksParams,
    frames: u64,
    r: ChunkSet,
    fresh: BTreeMap<usize, Vec<i16>>,
    out: Option<Peaks>,
}

impl Patcher {
    /// 前の段 `old`（引数 `params`）から、長さ `frames` の新しい値の段を、変わった区間 `changed` だけ作り直す準備をする。
    pub fn new(old: Peaks, params: PeaksParams, frames: u64, changed: &Spans) -> Self {
        let r = patch_chunks(old.frames, frames, changed);
        Self {
            old,
            params,
            frames,
            r,
            fresh: BTreeMap::new(),
            out: None,
        }
    }

    /// 流す区間（R のチャンク）。
    pub fn spans(&self) -> Spans {
        crate::render::audio::chunk_spans(
            &self.r,
            usize::try_from(self.frames).unwrap_or(usize::MAX),
        )
    }

    /// 組んだ段（`finish` のあと。まだなら `Internal`）。
    pub fn into_peaks(self) -> Result<Peaks, EngineError> {
        self.out
            .ok_or_else(|| EngineError::Internal("波形の段をまだ組んでいません".into()))
    }
}

impl AudioSink for Patcher {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        if info.rate != self.old.rate
            || info.channels != usize::from(self.old.channels)
            || info.frames != self.frames
        {
            return Err(EngineError::Internal(
                "波形の部分更新の流れの形が前の段・新しい長さと違います".into(),
            ));
        }
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let b0 = self.params.bins().first().copied().unwrap_or(PEAK_BINS[0]) as usize;
        let n = planes.first().map_or(0, |p| p.len());
        if !start.is_multiple_of(JOB_FRAMES as u64) {
            return Err(EngineError::Internal(
                "波形の部分更新の窓がチャンクに揃っていません".into(),
            ));
        }
        let first = usize::try_from(start / JOB_FRAMES as u64)
            .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        for (i, s) in (0..n).step_by(JOB_FRAMES).enumerate() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let j = first + i;
            if !self.r.contains(j) {
                continue;
            }
            let e = (s + JOB_FRAMES).min(n);
            let data: Vec<&[f32]> = planes.iter().map(|p| &p[s..e]).collect();
            self.fresh.insert(j, chunk_finest(&data, b0)?);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        self.out = Some(assemble(
            &self.old,
            &self.params,
            self.frames,
            &self.r,
            &self.fresh,
            &crate::monitor::NoMonitor,
        )?);
        Ok(())
    }
}

/// 波形の段の部分更新を小分けに受けるもの（M13 段 4。エディタの波形の段）：[`Patcher`] と同じ規則（R のチャンクだけを
/// 標本から作り、ほかは前の段から写す）で、前の段を `Arc` で共有したまま（写さずに）持ち、R のチャンクを何回にも分けて
/// 受ける（表示の値のチャンクを [`PeaksPatch::feed_chunk`] で・流れの窓を [`PeaksPatch::partial`] で、混ぜてよい）。
/// R をすべて受けたら [`PeaksPatch::assemble`] で段を組む（結果は新しい値を `compute` したものと同じ。E46）。
pub struct PeaksPatch {
    /// 前の段（None は前の段なし：R は全部）。
    old: Option<Arc<Peaks>>,
    params: PeaksParams,
    rate: u32,
    channels: u16,
    frames: u64,
    r: ChunkSet,
    fresh: BTreeMap<usize, Vec<i16>>,
}

impl PeaksPatch {
    /// 前の段 `old`（引数 `params`）から、長さ `frames` の新しい値の段を、変わった区間 `changed` に掛かるチャンク
    /// （[`patch_chunks`]）だけ作り直す準備をする。レート・チャンネルの数は前の段と同じ。
    pub fn new(old: Arc<Peaks>, params: PeaksParams, frames: u64, changed: &Spans) -> Self {
        let r = patch_chunks(old.frames, frames, changed);
        Self {
            rate: old.rate,
            channels: old.channels,
            old: Some(old),
            params,
            frames,
            r,
            fresh: BTreeMap::new(),
        }
    }

    /// 前の段なし：レート `rate`・`channels` チャンネル・長さ `frames` の値の段を、すべてのチャンクから作る準備をする。
    pub fn fresh(params: PeaksParams, rate: u32, channels: u16, frames: u64) -> Self {
        let count = usize::try_from(frames.div_ceil(JOB_FRAMES as u64)).unwrap_or(usize::MAX);
        Self {
            old: None,
            params,
            rate,
            channels,
            frames,
            r: ChunkSet::all(count),
            fresh: BTreeMap::new(),
        }
    }

    /// 作る段の長さ（フレーム）。
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// 作る段のレート。
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// 作る段のチャンネルの数。
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// 作り直すチャンク R。
    pub fn target(&self) -> &ChunkSet {
        &self.r
    }

    /// R のうち、まだ受けていないチャンク。
    pub fn missing(&self) -> ChunkSet {
        let mut m = self.r.clone();
        for k in self.fresh.keys() {
            m.remove(*k);
        }
        m
    }

    /// まだ受けていないチャンクの区間（流す区間）。
    pub fn missing_spans(&self) -> Spans {
        crate::render::audio::chunk_spans(
            &self.missing(),
            usize::try_from(self.frames).unwrap_or(usize::MAX),
        )
    }

    /// 受けたチャンクのビンの持つバイト数（予算に数える）。
    pub fn fresh_bytes(&self) -> u64 {
        self.fresh
            .values()
            .map(|v| (v.len() as u64).saturating_mul(2))
            .fold(0, u64::saturating_add)
    }

    /// チャンク `index` の全チャンネルの中身（チャンネルの順。長さはそのチャンクの長さ）を受ける。R の外は何もしない。
    pub fn feed_chunk(&mut self, index: usize, planes: &[&[f32]]) -> Result<(), EngineError> {
        if !self.r.contains(index) {
            return Ok(());
        }
        let start = (index as u64).saturating_mul(JOB_FRAMES as u64);
        let len = self.frames.saturating_sub(start).min(JOB_FRAMES as u64);
        if planes.len() != usize::from(self.channels)
            || planes.iter().any(|p| p.len() as u64 != len)
        {
            return Err(EngineError::Internal(format!(
                "波形の部分更新のチャンク {index} の形が違います"
            )));
        }
        let b0 = self.params.bins().first().copied().unwrap_or(PEAK_BINS[0]) as usize;
        self.fresh.insert(index, chunk_finest(planes, b0)?);
        Ok(())
    }

    /// 流れで受ける口（`begin` で形を確かめ、`feed` の窓の R のチャンクを受け、`finish` は何もしない）。窓は R の一部でよい
    /// （何回にも分けて流してよい）。
    pub fn partial(&mut self) -> PeaksPatchSink<'_> {
        PeaksPatchSink { patch: self }
    }

    /// 段を組む（R をすべて受けていなければ `Internal`）。
    pub fn assemble(&self, mon: &dyn Monitor) -> Result<Peaks, EngineError> {
        let missing = self.missing();
        if !missing.is_empty() {
            return Err(EngineError::Internal(format!(
                "波形の部分更新で、まだ受けていないチャンクが {} 個あります",
                missing.count()
            )));
        }
        let empty;
        let old: &Peaks = match &self.old {
            Some(old) => old,
            None => {
                empty = Peaks {
                    rate: self.rate,
                    channels: self.channels,
                    frames: 0,
                    levels: self
                        .params
                        .bins()
                        .into_iter()
                        .map(|bin| PeakLevel {
                            bin,
                            data: Vec::new(),
                        })
                        .collect(),
                };
                &empty
            }
        };
        assemble(old, &self.params, self.frames, &self.r, &self.fresh, mon)
    }
}

/// [`PeaksPatch::partial`] の流れの口。
pub struct PeaksPatchSink<'a> {
    patch: &'a mut PeaksPatch,
}

impl AudioSink for PeaksPatchSink<'_> {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        let p = &self.patch;
        if info.rate != p.rate
            || info.channels != usize::from(p.channels)
            || info.frames != p.frames
        {
            return Err(EngineError::Internal(
                "波形の部分更新の流れの形が新しい値と違います".into(),
            ));
        }
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        if !start.is_multiple_of(JOB_FRAMES as u64) {
            return Err(EngineError::Internal(
                "波形の部分更新の窓がチャンクに揃っていません".into(),
            ));
        }
        let n = planes.first().map_or(0, |p| p.len());
        let first = usize::try_from(start / JOB_FRAMES as u64)
            .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        for (i, s) in (0..n).step_by(JOB_FRAMES).enumerate() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let e = (s + JOB_FRAMES).min(n);
            let data: Vec<&[f32]> = planes.iter().map(|p| &p[s..e]).collect();
            self.patch.feed_chunk(first + i, &data)?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
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
            // 音声のチャンク（65,536 フレーム。M13）の境を 2 つまたぐ長さ（どの段にも半端なビンがあり、最も粗い段は 3 ビン）
            AnalysisExample {
                name: "crosses-audio-chunks",
                params: all,
                input: InputGen::AudioNoise {
                    rate: 8_000,
                    channels: 2,
                    frames: 131_109,
                    seed: 95,
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
                    let want = direct(&a.to_planes().unwrap()[ch], l.bin as usize);
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
                let want = direct(&a.to_planes().unwrap()[ch], l.bin as usize);
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
            a.to_planes().unwrap()[0][256..512]
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
            .as_chunks::<2>()
            .0
            .iter()
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

    /// E40（M13 段 1）：ランダムな値・変わった区間（保守的な大きめも）・長さの変化（縮める・伸ばす）・`finest`／`coarsest` の
    /// 組で、`patch` が新しい値を `compute` したものと同じ。作り直すのは R のチャンクだけ（ほかのチャンクの中身は読まない）。
    #[test]
    fn patch_equals_compute() {
        let c = JOB_FRAMES;
        let mut r = crate::determinism::rng(40);
        let limits = crate::Limits::default();
        let cx = AnalyzeCx {
            mon: &NoMonitor,
            limits: &limits,
        };
        let below =
            |r: &mut rand_chacha::ChaCha8Rng, n: usize| (r.next_u64() % n.max(1) as u64) as usize;
        let mut checked = 0;
        for case in 0..40 {
            let channels = 1 + below(&mut r, 3);
            let lens = [1usize, 300, c - 1, c, c + 1, 2 * c + 37, 3 * c];
            let n = lens[below(&mut r, lens.len())];
            let old: Vec<Vec<f32>> = (0..channels)
                .map(|_| {
                    (0..n)
                        .map(|_| (r.next_u32() as f32 / u32::MAX as f32) * 2.2 - 1.1)
                        .collect()
                })
                .collect();
            // 新しい値：区間をいくつか書き換え、ときどき長さを変える
            let mut new = old.clone();
            let mut changed = Spans::new();
            for _ in 0..1 + below(&mut r, 3) {
                let s = below(&mut r, n);
                let e = (s + 1 + below(&mut r, 3_000)).min(n);
                for p in &mut new {
                    for v in &mut p[s..e] {
                        *v = -*v * 0.5;
                    }
                }
                // 保守的な大きめも
                changed.push(
                    s.saturating_sub(below(&mut r, 2) * 500) as u64,
                    e as u64 + below(&mut r, 2) as u64 * 700,
                );
            }
            match below(&mut r, 3) {
                0 => {
                    let m = 1 + below(&mut r, n);
                    for p in &mut new {
                        p.truncate(m);
                    }
                }
                1 => {
                    let extra = 1 + below(&mut r, c + 10);
                    for p in &mut new {
                        p.extend((0..extra).map(|i| (i % 100) as f32 * 0.01 - 0.5));
                    }
                }
                _ => {}
            }
            let (fa, fb) = (audio(old), audio(new.clone()));
            let sizes = [256u32, 1_024, 4_096, 16_384, 65_536];
            let i = below(&mut r, 5);
            let j = i + below(&mut r, 5 - i);
            let p = PeaksParams {
                finest: sizes[i],
                coarsest: sizes[j],
            };
            let before = cx_run(&fa, &p);
            let want = cx_run(&fb, &p);
            let rset = patch_chunks(fa.frames() as u64, fb.frames() as u64, &changed);
            let mut asked = Vec::new();
            let got = patch(
                &before,
                &p,
                fb.frames() as u64,
                &changed,
                |k| {
                    asked.push(k as usize);
                    (0..channels)
                        .map(|ch| fb.chunk_data(ch, k as usize).map(|d| d.to_vec()))
                        .collect()
                },
                &cx,
            )
            .unwrap();
            assert_eq!(
                got,
                want,
                "{case}（{n} → {}・{p:?}・{changed:?}）",
                fb.frames()
            );
            assert_eq!(asked, rset.iter().collect::<Vec<_>>(), "{case}");
            checked += 1;
        }
        assert_eq!(checked, 40);
        // 前の段の引数が違えば断る
        let a = audio(vec![vec![0.5; 1_000]]);
        let before = cx_run(&a, &PeaksParams::default());
        let other = PeaksParams {
            finest: 1_024,
            coarsest: 4_096,
        };
        assert!(
            patch(
                &before,
                &other,
                1_000,
                &Spans::new(),
                |_| Ok(Vec::<Vec<f32>>::new()),
                &cx
            )
            .is_err()
        );
    }
}
