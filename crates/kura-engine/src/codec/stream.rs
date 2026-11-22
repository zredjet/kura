//! 流れのデコード（M13 段 2。docs/editing.md §9.1・§9.11）：音声のデコーダが標本を渡す口 [`DecodeOut`] と、流れを受ける
//! [`DecodeSink`]。
//!
//! - デコーダは標本を `DecodeOut` に渡す（口は `AudioBuilder` と同じ形：`begin`・`window`・`push_frames`・`push_interleaved`・
//!   `frames`・`end`）。`DecodeOut` は 2 つの形を持つ：
//!   - 組み手の形（[`collect`]）：`AudioBuilder` にそのまま渡して値にする（今と同じ写しの数）。音声のデコーダの `decode` は
//!     この形で `decode_stream` を通すので、実装が 1 つになり値は同じ。
//!   - 流す形（`CodecSet::decode_stream*`）：渡された標本を、チャンクに揃えた窓（`STREAM_CHUNKS` チャンク。最後だけ短い）に
//!     まとめて呼び手の `DecodeSink` へ流し、値のダイジェスト（`PcmTreeDigest`）も同時に取る。持つ量は窓 1 つとダイジェストの
//!     作業列だけ。
//! - `DecodeSink` の約束：試すデコーダごとに `begin`（前の試みで受けたものは捨てる）→ 頭から順・重ならない窓の `feed` →
//!   `end`。このあと `decode_stream` が `Err` を返したら、受けたものはすべて無効（FLAC の MD5 は最後に照らす）。
//! - 長さが上限しか分からないデコーダ（MP3・AAC）は見込みの長さ（`DecodeInfo::expect`）でダイジェストを取り、外れたら
//!   `CodecSet` が同じデコーダでダイジェストだけをもう 1 度読む（呼び手の `DecodeSink` には 1 度だけ流れる）。

use std::sync::Arc;

use crate::audio::digest::PcmTreeDigest;
use crate::audio::stream::{STREAM_CHUNKS, StreamEnd};
use crate::digest::Digest;
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::shape::Dim;
use crate::value::{AUDIO_CHUNK, Audio, AudioBuilder, AudioMeta, Value};

/// デコーダが読み始めるときの形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeInfo {
    pub rate: u32,
    pub channels: usize,
    /// 長さ（`Exact`）か上限（`AtMost`）。
    pub frames: Dim,
    /// 見込みの長さ（`Exact` ならその値。上限の形はデコーダが見込む長さ。ダイジェストはこの長さで取り始める）。
    pub expect: u64,
}

/// 流れのデコードを受けるもの（解析の扇・作業用の WAV の書き手・ダイジェストだけを取るもの）。
pub trait DecodeSink: Send {
    /// 試すデコーダごとに呼ぶ（前の試みで受けたものは捨てる）。
    fn begin(&mut self, info: &DecodeInfo) -> Result<(), EngineError>;
    /// チャンクに揃えた窓（最後だけ短い）。頭から順・重ならない。
    fn feed(&mut self, start: u64, planes: &[&[f32]], mon: &dyn Monitor)
    -> Result<(), EngineError>;
    /// 読み終えた。このあと `decode_stream` が `Err` を返したら、受けたものはすべて無効。
    fn end(&mut self, end: &StreamEnd) -> Result<(), EngineError>;
}

/// 何もしない受け手（ダイジェストだけを取る 2 度目の読みに使う）。
pub struct NullSink;

impl DecodeSink for NullSink {
    fn begin(&mut self, _info: &DecodeInfo) -> Result<(), EngineError> {
        Ok(())
    }

    fn feed(
        &mut self,
        _start: u64,
        _planes: &[&[f32]],
        _mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        Ok(())
    }

    fn end(&mut self, _end: &StreamEnd) -> Result<(), EngineError> {
        Ok(())
    }
}

/// 流れの窓のフレームの数（`STREAM_CHUNKS` チャンク）。
pub const WINDOW_FRAMES: usize = STREAM_CHUNKS * AUDIO_CHUNK;

/// 形 `frames` × `channels` の音声を流すときの窓の作業列の大きさ（バイト）：min(長さ, 16·C) × チャンネル × 4。
pub fn stream_window_bytes(channels: usize, frames: u64) -> u64 {
    frames
        .min(WINDOW_FRAMES as u64)
        .saturating_mul(channels as u64)
        .saturating_mul(4)
}

/// 流す形の状態。
struct Streaming<'a> {
    sink: &'a mut dyn DecodeSink,
    /// ダイジェストを取るか、取るならその長さを見込みの代わりに使うか（2 度目の読み）。
    digest_mode: DigestMode,
    digest: Option<PcmTreeDigest>,
    /// 見込みの長さが外れた（ダイジェストは使えない）。
    mispredicted: bool,
    /// 窓の作業列（チャンネルごと）。
    buf: Vec<Vec<f32>>,
    /// 作業列の頭の位置（フレーム）。
    start: u64,
    rate: u32,
}

/// ダイジェストの取り方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestMode {
    /// 取らない。
    Off,
    /// 見込みの長さで取る。
    Expect,
    /// この長さで取る（見込みが外れたあとの 2 度目の読み）。
    Frames(u64),
}

enum Mode<'a> {
    /// 組み手の形（値にする）。
    Build {
        builder: Option<AudioBuilder>,
        value: Option<Audio>,
    },
    /// 流す形。
    Stream(Box<Streaming<'a>>),
}

/// デコーダが標本を渡す口（モジュールの説明）。
pub struct DecodeOut<'a> {
    mode: Mode<'a>,
    mon: &'a dyn Monitor,
    channels: usize,
    /// 残したフレームの数。
    frames: u64,
    /// 頭から飛ばすフレームの残りと、残すフレームの上限の残り（`AudioBuilder::window` と同じ）。
    skip: u64,
    take: u64,
    begun: bool,
    info: Option<DecodeInfo>,
    end: Option<StreamEnd>,
}

impl<'a> DecodeOut<'a> {
    /// 組み手の形（値にする）。
    pub fn build(mon: &'a dyn Monitor) -> Self {
        Self {
            mode: Mode::Build {
                builder: None,
                value: None,
            },
            mon,
            channels: 0,
            frames: 0,
            skip: 0,
            take: u64::MAX,
            begun: false,
            info: None,
            end: None,
        }
    }

    /// 流す形（窓にまとめて `sink` へ。ダイジェストは `digest` のとおり）。
    pub fn stream(sink: &'a mut dyn DecodeSink, digest: DigestMode, mon: &'a dyn Monitor) -> Self {
        Self {
            mode: Mode::Stream(Box::new(Streaming {
                sink,
                digest_mode: digest,
                digest: None,
                mispredicted: false,
                buf: Vec::new(),
                start: 0,
                rate: 0,
            })),
            mon,
            channels: 0,
            frames: 0,
            skip: 0,
            take: u64::MAX,
            begun: false,
            info: None,
            end: None,
        }
    }

    /// 読み始めた形（`begin` のあと）。
    pub fn info(&self) -> Option<DecodeInfo> {
        self.info
    }

    /// 読み始める（試すデコーダごとに。前の試みで受けたものは捨てる）。
    pub fn begin(&mut self, info: DecodeInfo) -> Result<(), EngineError> {
        self.channels = info.channels;
        self.frames = 0;
        self.skip = 0;
        self.take = u64::MAX;
        self.begun = true;
        self.info = Some(info);
        self.end = None;
        match &mut self.mode {
            Mode::Build { builder, value } => {
                *builder = Some(AudioBuilder::new(info.rate, info.channels, info.expect)?);
                *value = None;
            }
            Mode::Stream(s) => {
                // 形の確かめ（チャンネル 0 など）は組み手と同じ
                AudioBuilder::new(info.rate, info.channels, 0)?;
                s.sink.begin(&info)?;
                s.rate = info.rate;
                s.start = 0;
                s.mispredicted = false;
                let want = match s.digest_mode {
                    DigestMode::Off => None,
                    DigestMode::Expect => Some(info.expect),
                    DigestMode::Frames(n) => Some(n),
                };
                s.digest = match want {
                    Some(n) => Some(PcmTreeDigest::new(info.rate, info.channels, n)?),
                    None => None,
                };
                let cap = usize::try_from(info.expect)
                    .unwrap_or(usize::MAX)
                    .clamp(1, WINDOW_FRAMES);
                s.buf.clear();
                for _ in 0..info.channels {
                    let mut v = Vec::new();
                    v.try_reserve_exact(cap)
                        .map_err(|_| EngineError::Limit("流れの窓を確保できません".into()))?;
                    s.buf.push(v);
                }
            }
        }
        Ok(())
    }

    /// 足した標本のうち、頭の `skip` フレームを捨て、続く `take` フレームだけを残す（`AudioBuilder::window` と同じ。`begin`
    /// のあと、標本を足す前に呼ぶ）。
    pub fn window(&mut self, skip: u64, take: u64) {
        self.skip = skip;
        self.take = take;
    }

    /// 残したフレームの数。
    pub fn frames(&self) -> u64 {
        self.frames
    }

    fn check_begun(&self) -> Result<(), EngineError> {
        if self.begun {
            Ok(())
        } else {
            Err(EngineError::Internal(
                "デコードの流れが始まっていません".into(),
            ))
        }
    }

    /// チャンネルごとの標本の列（チャンネルの数だけ、同じ長さ）を足す。
    pub fn push_frames(&mut self, planes: &[&[f32]]) -> Result<(), EngineError> {
        self.check_begun()?;
        if planes.len() != self.channels {
            return Err(EngineError::Internal(format!(
                "チャンネルの数が違います（{} と {}）",
                planes.len(),
                self.channels
            )));
        }
        let n = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != n) {
            return Err(EngineError::Internal(
                "チャンネルの長さがそろっていません".into(),
            ));
        }
        let skip = usize::try_from(self.skip).unwrap_or(usize::MAX).min(n);
        self.skip -= skip as u64;
        let keep = usize::try_from(self.take)
            .unwrap_or(usize::MAX)
            .min(n - skip);
        self.take -= keep as u64;
        if keep == 0 {
            return Ok(());
        }
        let views: Vec<&[f32]> = planes.iter().map(|p| &p[skip..skip + keep]).collect();
        self.frames += keep as u64;
        let mon = self.mon;
        match &mut self.mode {
            Mode::Build { builder, .. } => builder
                .as_mut()
                .ok_or_else(|| EngineError::Internal("組み手がありません".into()))?
                .push_frames(&views),
            Mode::Stream(s) => s.push(&views, mon),
        }
    }

    /// インターリーブした標本（フレームの順に全チャンネル）を足す。長さはチャンネルの数の倍数であること。
    pub fn push_interleaved(&mut self, samples: &[f32]) -> Result<(), EngineError> {
        self.check_begun()?;
        let ch = self.channels;
        if ch == 0 || !samples.len().is_multiple_of(ch) {
            return Err(EngineError::Internal(
                "インターリーブの標本の数がチャンネルの数の倍数でありません".into(),
            ));
        }
        let n = samples.len() / ch;
        let mut planes = Vec::with_capacity(ch);
        for c in 0..ch {
            let mut p = crate::codec::try_vec(n, 0f32)?;
            for (f, d) in p.iter_mut().enumerate() {
                *d = samples[f * ch + c];
            }
            planes.push(p);
        }
        let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        self.push_frames(&views)
    }

    /// 読み終えた（メタデータは `meta`）。流す形は残りの窓を流して `sink.end` を呼ぶ。
    pub fn end(&mut self, meta: AudioMeta) -> Result<(), EngineError> {
        self.check_begun()?;
        let end = StreamEnd {
            frames: self.frames,
            meta,
        };
        let mon = self.mon;
        match &mut self.mode {
            Mode::Build { builder, value } => {
                let b = builder
                    .take()
                    .ok_or_else(|| EngineError::Internal("組み手がありません".into()))?;
                *value = Some(b.finish(end.meta.clone())?);
            }
            Mode::Stream(s) => {
                s.flush(mon)?;
                s.sink.end(&end)?;
            }
        }
        self.end = Some(end);
        Ok(())
    }

    /// 値を流す（流さないデコーダの既定の `decode_stream`）：組み手の形ならそのまま値にし、流す形なら窓ごとに読んで渡す。
    pub fn put_value(&mut self, v: &Value) -> Result<(), EngineError> {
        let Value::Audio(a) = v else {
            return Err(EngineError::Unsupported(
                "音声でない値は流して読めません".into(),
            ));
        };
        let frames = a.frames() as u64;
        self.begin(DecodeInfo {
            rate: a.rate(),
            channels: a.channels(),
            frames: Dim::Exact(frames),
            expect: frames,
        })?;
        if let Mode::Build { builder, value } = &mut self.mode {
            *builder = None;
            *value = Some((**a).clone());
            self.frames = frames;
            self.end = Some(StreamEnd {
                frames,
                meta: a.meta.clone(),
            });
            return Ok(());
        }
        for (s, e) in crate::audio::stream::stream_windows(frames, None) {
            if self.mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let planes = a.read_planes(s as usize, e as usize)?;
            let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
            self.push_frames(&views)?;
        }
        self.end(a.meta.clone())
    }

    /// 読み終えた終わり（`end` のあと）。
    pub fn stream_end(&self) -> Option<&StreamEnd> {
        self.end.as_ref()
    }

    /// 組み手の形の値（`end` のあと）。
    pub fn into_value(self) -> Result<Value, EngineError> {
        match self.mode {
            Mode::Build { value: Some(v), .. } => Ok(Value::Audio(Arc::new(v))),
            _ => Err(EngineError::Internal(
                "デコードの値がありません（組み手の形でないか、読み終えていません）".into(),
            )),
        }
    }

    /// 流す形のダイジェスト（読み終えて、見込みの長さが当たったときだけ Some）。
    pub fn digest(self) -> Result<Option<Digest>, EngineError> {
        let Some(end) = self.end else {
            return Ok(None);
        };
        match self.mode {
            Mode::Stream(s) => match s.digest {
                Some(d) if !s.mispredicted && d.is_complete() => Ok(Some(d.finish(&end.meta)?)),
                _ => Ok(None),
            },
            Mode::Build { .. } => Ok(None),
        }
    }
}

impl Streaming<'_> {
    /// 標本を窓の作業列に足し、窓が埋まるたびに流す。
    fn push(&mut self, planes: &[&[f32]], mon: &dyn Monitor) -> Result<(), EngineError> {
        let n = planes.first().map_or(0, |p| p.len());
        let mut at = 0usize;
        while at < n {
            let have = self.buf.first().map_or(0, Vec::len);
            let take = (WINDOW_FRAMES - have).min(n - at);
            for (b, p) in self.buf.iter_mut().zip(planes) {
                b.try_reserve(take)
                    .map_err(|_| EngineError::Limit("流れの窓を確保できません".into()))?;
                b.extend_from_slice(&p[at..at + take]);
            }
            at += take;
            if have + take == WINDOW_FRAMES {
                self.flush(mon)?;
            }
        }
        Ok(())
    }

    /// 作業列の窓を流す（空なら何もしない）。
    fn flush(&mut self, mon: &dyn Monitor) -> Result<(), EngineError> {
        let n = self.buf.first().map_or(0, Vec::len);
        if n == 0 {
            return Ok(());
        }
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let views: Vec<&[f32]> = self.buf.iter().map(Vec::as_slice).collect();
        self.sink.feed(self.start, &views, mon)?;
        if let Some(d) = &mut self.digest
            && !self.mispredicted
        {
            match d.push_window(self.start, &views, mon) {
                Ok(()) => {}
                // 見込みの長さを超えた：このダイジェストは使えない（呼び手の流れは続ける）
                Err(EngineError::Internal(_)) => self.mispredicted = true,
                Err(e) => return Err(e),
            }
        }
        self.start += n as u64;
        for b in &mut self.buf {
            b.clear();
        }
        Ok(())
    }
}

/// 組み手の形で `f` を回して値にする（音声のデコーダの `decode` が `decode_stream` を通すため）。
pub fn collect(
    mon: &dyn Monitor,
    f: impl FnOnce(&mut DecodeOut<'_>) -> Result<(), EngineError>,
) -> Result<Value, EngineError> {
    let mut out = DecodeOut::build(mon);
    f(&mut out)?;
    out.into_value()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    /// 窓を集める受け手。
    #[derive(Default)]
    struct Collect {
        begins: usize,
        windows: Vec<(u64, u64)>,
        planes: Vec<Vec<f32>>,
        end: Option<StreamEnd>,
    }

    impl DecodeSink for Collect {
        fn begin(&mut self, info: &DecodeInfo) -> Result<(), EngineError> {
            self.begins += 1;
            self.windows.clear();
            self.planes = vec![Vec::new(); info.channels];
            Ok(())
        }

        fn feed(
            &mut self,
            start: u64,
            planes: &[&[f32]],
            _mon: &dyn Monitor,
        ) -> Result<(), EngineError> {
            let n = planes[0].len() as u64;
            assert_eq!(start % AUDIO_CHUNK as u64, 0);
            self.windows.push((start, start + n));
            for (o, p) in self.planes.iter_mut().zip(planes) {
                o.extend_from_slice(p);
            }
            Ok(())
        }

        fn end(&mut self, end: &StreamEnd) -> Result<(), EngineError> {
            self.end = Some(end.clone());
            Ok(())
        }
    }

    /// 窓は揃って順・連結すると組み手の値と同じ・ダイジェストも同じ（`window` の切り取りを含む）。
    #[test]
    fn streamed_windows_equal_the_built_value() {
        let n = 2 * WINDOW_FRAMES + 3 * AUDIO_CHUNK + 17;
        let src: Vec<Vec<f32>> = (0..2)
            .map(|c| (0..n).map(|i| ((i * 7 + c) % 1000) as f32 * 1e-3).collect())
            .collect();
        for (skip, take) in [(0u64, u64::MAX), (1_000, (n - 5_000) as u64)] {
            let feed = |out: &mut DecodeOut<'_>| -> Result<(), EngineError> {
                out.begin(DecodeInfo {
                    rate: 8_000,
                    channels: 2,
                    frames: Dim::AtMost(n as u64),
                    expect: n as u64 - skip.min(n as u64),
                })?;
                out.window(skip, take);
                let mut at = 0;
                let mut step = 1;
                while at < n {
                    let e = (at + step).min(n);
                    let views: Vec<&[f32]> = src.iter().map(|p| &p[at..e]).collect();
                    out.push_frames(&views)?;
                    at = e;
                    step = step * 3 + 7;
                }
                out.end(AudioMeta::default())
            };
            let v = collect(&NoMonitor, feed).unwrap();
            let mut sink = Collect::default();
            let mut out = DecodeOut::stream(&mut sink, DigestMode::Expect, &NoMonitor);
            feed(&mut out).unwrap();
            let kept = out.frames();
            let digest = out.digest().unwrap();
            let Value::Audio(a) = &v else { panic!() };
            assert_eq!(kept, a.frames() as u64);
            let want = a.read_planes(0, a.frames()).unwrap();
            assert_eq!(sink.planes, want);
            assert!(sink.windows.windows(2).all(|w| w[0].1 == w[1].0));
            assert!(
                sink.windows
                    .iter()
                    .all(|(s, e)| e - s <= WINDOW_FRAMES as u64)
            );
            assert_eq!(sink.end.unwrap().frames, kept);
            let expect_hit = skip == 0;
            assert_eq!(digest, expect_hit.then(|| v.digest().unwrap()));
        }
    }
}
