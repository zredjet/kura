//! MP3（読みだけ。nanomp3。Pinned で値を再現できる。ADR-148）。
//!
//! - nanomp3 は minimp3 とビット単位で一致する純 Rust の実装で、libm を使わない（SIMD の feature は外す）。
//!   MPEG-1/2/2.5 の layer 3 を読む（layer 1・2 は feature を入れないので読まない）。
//! - 長さは開くときに決め、形はその上限（`AtMost`）にする。上限は、見つかったフレームの標本の数の和（ファイルの中身
//!   から出うる長さ）と、Xing・Info（LAME・Lavc）のタグの値（エンコーダの遅延と詰め物を切った長さ。nanomp3 はこのとき
//!   走査しない）の小さいほう。タグのフレームの数が 0（途中で止まった ffmpeg の出力など）なら長さは分からないものとし、
//!   タグがなければ nanomp3 の全体の走査の値。途中で切れたファイル（タグより短い）は短く読む。上限を超えて出てくれば
//!   壊れたファイルとして扱う。途中でモノとステレオが切り替わるもの・レートが変わるものは読まない。
//! - layer 3 だけを読む。最初のフレームが layer 1・2（MP2 など）なら `Unsupported`（次の段へ。nanomp3 は layer 1・2 の
//!   feature を入れないと、標本を出さずにエラーにもしない）。
//! - 最後の完全なフレーム（`nanomp3::frames` は、はみ出すフレームを返さない）の終わりまでを読む。途中で切れた最後の
//!   フレームや末尾のゴミを、nanomp3 は「形式が変わった」エラーにするので（minimp3 と同じ）、切れたダウンロードが
//!   読めなくならないように前もって外す。ただし free format（ビットレート 0）は、`nanomp3::frames` が最後の 2 フレームを
//!   見つけられないので切らない（上限にはその 2 フレームの分を足す）。
//! - 標本は nanomp3 の f32（合成の値 / 32768）。タグ（ID3）は読まない（段 6。ADR-148）。
//! - 流して読む（M13 段 2）：ファイルは今どおり全体を読んで持ち（nanomp3 にフレームを読み口から読む公開の口がない）、標本を
//!   フレームごとに流す（値は持たない）。形を読むのにも全体を読むので、`shape_work_bytes` は長さ。見込みの長さは形の上限。

use crate::codec::stream::{self, DecodeInfo, DecodeOut};
use crate::codec::symph::read_all;
use crate::codec::{Decoder, ReadSeek, Tier};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};
use crate::value::{AudioMeta, AudioSource, Value};

/// MP3 の読み取り。
pub struct Mp3Codec;

/// 読み取りの約束の版。
const DEC_ID: &str = "mp3-nanomp3-dec/1";

fn err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: message.into(),
    }
}

/// 開いて決めたこと：読むバイト列（最後の完全なフレームまで）と形。
struct Opened<'a> {
    data: &'a [u8],
    rate: u32,
    channels: u16,
    /// 長さの上限。
    frames: u64,
}

fn open<'a>(all: &'a [u8], limits: &Limits) -> Result<Opened<'a>, EngineError> {
    let first = nanomp3::frames(all)
        .next()
        .ok_or_else(|| err("MP3 のフレームがありません"))?;
    if first.layer != 3 {
        return Err(EngineError::Unsupported(format!(
            "{DEC_ID}: MPEG layer {} は読めません",
            first.layer
        )));
    }
    let free = first.bitrate_kbps == 0;
    // 中身から出うる長さ（Xing・Info のフレームも数えるので、出る長さを下回らない）と、最後の完全なフレームの終わり
    let (mut scan, mut end) = (0u64, 0usize);
    for f in nanomp3::frames(all) {
        scan = scan.saturating_add(f.samples as u64);
        end = f.offset.saturating_add(f.data.len()).min(all.len());
    }
    let data = if free {
        scan = scan.saturating_add(2 * 1_152);
        all
    } else {
        &all[..end]
    };
    let r = nanomp3::SliceReader::<f32>::new(data);
    let channels = r
        .channels()
        .map(|c| u16::from(c.num()))
        .ok_or_else(|| err("MP3 のフレームがありません"))?;
    let rate = r.sample_rate();
    if rate == 0 {
        return Err(err("MP3 のフレームがありません"));
    }
    if rate > limits.max_sample_rate {
        return Err(EngineError::Limit("サンプルレートが大きすぎます".into()));
    }
    let frames = match r.total_samples() {
        Some(t) if t > 0 => t.min(scan),
        _ => scan,
    };
    Ok(Opened {
        data,
        rate,
        channels,
        frames,
    })
}

impl Decoder for Mp3Codec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "mp3"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let data = read_all(src, limits)?;
        let o = open(&data, limits)?;
        Ok(Shape::Audio {
            rate: o.rate,
            channels: o.channels,
            frames: Dim::AtMost(o.frames),
        })
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        stream::collect(mon, |out| decode_into(src, limits, out, mon))
    }

    fn streams(&self, _format: &str) -> bool {
        true
    }

    /// ファイル全体（長さ `len`）と 1 フレームの写し（1 MiB を見込む）。
    fn stream_work_bytes(&self, _format: &str, _shape: &Shape, len: u64) -> u64 {
        len.saturating_add(1 << 20)
    }

    /// 形を読むのにもファイル全体を読む。
    fn shape_work_bytes(&self, _format: &str, len: u64) -> u64 {
        len
    }

    fn decode_stream(
        &self,
        src: &mut (dyn ReadSeek + Send),
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        _shape: &Shape,
        out: &mut DecodeOut<'_>,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        decode_into(src, limits, out, mon)
    }
}

/// ファイル全体を読み、フレームごとの標本を `out` に渡す（`decode`・`decode_stream` が通る）。
fn decode_into(
    src: &mut dyn ReadSeek,
    limits: &Limits,
    out: &mut DecodeOut<'_>,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let all = read_all(src, limits)?;
    let Opened {
        data,
        rate,
        channels,
        frames,
    } = open(&all, limits)?;
    limits
        .check(&Shape::Audio {
            rate,
            channels,
            frames: Dim::AtMost(frames),
        })
        .map_err(EngineError::Limit)?;
    let ch = usize::from(channels);
    // 上限は中身から出うる長さ以下なので、そのまま見込みにしてよい
    out.begin(DecodeInfo {
        rate,
        channels: ch,
        frames: Dim::AtMost(frames),
        expect: frames,
    })?;
    let mut r = nanomp3::SliceReader::<f32>::new(data);
    let mut done = 0u64;
    loop {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let samples = r.read_frame().map_err(|e| err(e.to_string()))?;
        let Some(samples) = samples else { break };
        if samples.len() % ch != 0 {
            return Err(err("チャンネルの数が途中で変わりました"));
        }
        let n = samples.len() / ch;
        done = done.saturating_add(n as u64);
        if done > frames {
            return Err(err("長さがヘッダと合いません"));
        }
        out.push_interleaved(samples)?;
    }
    let meta = AudioMeta {
        source: Some(AudioSource {
            bits: 16,
            float: false,
            rate,
        }),
        ..AudioMeta::default()
    };
    out.end(meta)
}
