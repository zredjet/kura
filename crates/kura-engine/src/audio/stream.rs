//! 音声の流れ（M13 段 1。docs/editing.md §12.1・§12.5）：値や計画の出力を、頭から順に窓（`STREAM_CHUNKS` チャンク）ごとに
//! 描いて渡す口。段 2 の流れの解析・段 3 の保存がこの口に乗る。
//!
//! - [`AudioSink`]：`begin`（形とメタデータ）→ `feed`（頭から順・重ならず・チャンクに揃えた窓の全チャンネル）→ `finish`。
//! - [`stream_value`]：値（遅延の値を含む）を窓ごとに読んで渡す。計画の出力は `Engine::stream_audio`（render/audio.rs）。
//! - [`PcmDigest`]：`kura-pcm-v1` のダイジェスト（docs/editing.md §8.6）を、チャンネルの順に区間を足して作る。`Value::digest`・
//!   `Engine::digest_audio_output`・段 3 の保存が同じ部品を通る（式は変えない）。

use crate::digest::{Digest, Hasher, canonical};
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::render::region::Spans;
use crate::value::{AUDIO_CHUNK, Audio, AudioMeta};

/// 流れの窓のチャンクの数（1 窓 = 16 チャンク = 1,048,576 フレーム。ステレオで 8 MiB）。値に効かない。
pub const STREAM_CHUNKS: usize = 16;

/// 流れの形（`begin` に渡す）。メタデータはどの窓でも同じ（区間によらない）。
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub rate: u32,
    pub channels: usize,
    pub frames: u64,
    pub meta: AudioMeta,
}

/// 流れを受けるもの。
pub trait AudioSink {
    /// 流し始める（窓の前に 1 度）。
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError>;
    /// フレーム `start` からの全チャンネルの標本（チャンネルの順・同じ長さ）。頭から順・重ならず・チャンクに揃えて呼ぶ。
    fn feed(&mut self, start: u64, planes: &[&[f32]], mon: &dyn Monitor)
    -> Result<(), EngineError>;
    /// 流し終えた（最後の窓のあとに 1 度）。
    fn finish(&mut self) -> Result<(), EngineError>;
}

/// 長さ `frames` の音声を流す窓（チャンクに揃えた [start, end)。頭から順・重ならず・`STREAM_CHUNKS` チャンクまで）。
/// `spans` があれば、それに掛かるチャンクだけ（続いたチャンクを窓にまとめる）。None は全体。
pub fn stream_windows(frames: u64, spans: Option<&Spans>) -> Vec<(u64, u64)> {
    let c = AUDIO_CHUNK as u64;
    let count = frames.div_ceil(c);
    let wanted = |k: u64| -> bool {
        spans.is_none_or(|s| {
            let (a, b) = (k * c, ((k + 1) * c).min(frames));
            s.spans().iter().any(|(x, y)| *x < b && a < *y)
        })
    };
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut run: Option<(u64, u64)> = None;
    for k in 0..count {
        if !wanted(k) {
            if let Some(r) = run.take() {
                out.push(r);
            }
            continue;
        }
        let (a, b) = (k * c, ((k + 1) * c).min(frames));
        run = match run {
            Some((s, e)) if e == a && (b - s).div_ceil(c) <= STREAM_CHUNKS as u64 => Some((s, b)),
            Some(r) => {
                out.push(r);
                Some((a, b))
            }
            None => Some((a, b)),
        };
    }
    out.extend(run);
    out
}

/// 値 `a`（`spans` の区間に掛かるチャンク。None は全体）を窓ごとに読んで `sink` に渡す。窓ごとに中止を確かめる。
pub fn stream_value(
    a: &Audio,
    spans: Option<&Spans>,
    sink: &mut dyn AudioSink,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    sink.begin(&StreamInfo {
        rate: a.rate(),
        channels: a.channels(),
        frames: a.frames() as u64,
        meta: a.meta.clone(),
    })?;
    for (s, e) in stream_windows(a.frames() as u64, spans) {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let planes = a.read_planes(s as usize, e as usize)?;
        let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        sink.feed(s, &views, mon)?;
    }
    sink.finish()
}

/// 音声のダイジェストで標本をまとめて足す数（確かめを呼ぶ間隔のためだけ。値に効かない）。
const BLOCK: usize = crate::value::DIGEST_CHUNK;

/// `kura-pcm-v1` のダイジェストを流して作る：blake3("kura-pcm-v1" ‖ rate ‖ channels ‖ frames ‖ チャンネル 0 の全標本 ‖
/// チャンネル 1 … ‖ メタデータの正規の JSON)。標本は f32 のビット（NaN は 0x7fc00000 にそろえる）のリトルエンディアン。
/// `push` はチャンネルの順に、各チャンネルの頭から順に区間を渡す（順でなければ `Internal`）。
pub struct PcmDigest {
    h: Hasher,
    channels: usize,
    frames: u64,
    ch: usize,
    pos: u64,
    buf: Vec<u8>,
}

impl PcmDigest {
    /// レート `rate`・`channels` チャンネル・長さ `frames` の音声のダイジェストを作り始める。
    pub fn new(rate: u32, channels: usize, frames: u64) -> Self {
        let mut h = Hasher::new("kura-pcm-v1");
        h.u32(rate).u32(channels as u32).u64(frames);
        Self {
            h,
            channels,
            frames,
            ch: 0,
            pos: 0,
            buf: Vec::new(),
        }
    }

    /// チャンネル `ch` のフレーム `start` からの標本を足す。
    pub fn push(&mut self, ch: usize, start: u64, samples: &[f32]) -> Result<(), EngineError> {
        self.push_with(ch, start, samples, &mut || Ok(()))
    }

    /// [`PcmDigest::push`] と同じに足し、各チャンネルの `BLOCK`（16,384）個ずつの塊を足し始める前に `check` を呼ぶ（中止の
    /// 確かめ。値に効かない。呼ぶ数は Σ_チャンネル ceil(フレーム数 / 16,384)）。
    pub fn push_with(
        &mut self,
        ch: usize,
        start: u64,
        samples: &[f32],
        check: &mut dyn FnMut() -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        // 今のチャンネルを足し終えていれば、次のチャンネルへ
        if ch == self.ch + 1 && self.pos == self.frames {
            self.ch += 1;
            self.pos = 0;
        }
        let end = start.checked_add(samples.len() as u64);
        if ch != self.ch
            || ch >= self.channels
            || start != self.pos
            || end.is_none_or(|e| e > self.frames)
        {
            return Err(EngineError::Internal(format!(
                "音声のダイジェストの区間が順でありません（チャンネル {ch}・{start} から。次はチャンネル {}・{}）",
                self.ch, self.pos
            )));
        }
        let mut i = 0usize;
        while i < samples.len() {
            let at = (self.pos % BLOCK as u64) as usize;
            if at == 0 {
                check()?;
            }
            let take = (BLOCK - at).min(samples.len() - i);
            self.buf.clear();
            for s in &samples[i..i + take] {
                // NaN は 1 つの値にそろえる
                let bits = if s.is_nan() { 0x7fc0_0000 } else { s.to_bits() };
                self.buf.extend_from_slice(&bits.to_le_bytes());
            }
            self.h.raw(&self.buf);
            i += take;
            self.pos += take as u64;
        }
        Ok(())
    }

    /// 全標本を足したあと、メタデータを足してダイジェストを返す（足りなければ `Internal`）。
    pub fn finish(mut self, meta: &AudioMeta) -> Result<Digest, EngineError> {
        let done = self.frames == 0 || (self.ch + 1 == self.channels && self.pos == self.frames);
        if !done {
            return Err(EngineError::Internal(format!(
                "音声のダイジェストの標本が足りません（チャンネル {}・{} / {}）",
                self.ch, self.pos, self.frames
            )));
        }
        self.h.bytes_with_len(&canonical(meta)?);
        Ok(self.h.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use std::sync::Arc;

    #[test]
    fn windows_are_aligned_ordered_and_bounded() {
        let c = AUDIO_CHUNK as u64;
        let n = 40 * c + 5;
        let all = stream_windows(n, None);
        assert_eq!(all.first(), Some(&(0, 16 * c)));
        assert_eq!(all.last(), Some(&(32 * c, n)));
        assert_eq!(all.len(), 3);
        let s = Spans::one(c + 3, c + 4);
        assert_eq!(stream_windows(n, Some(&s)), vec![(c, 2 * c)]);
        let mut two = Spans::one(0, 1);
        two.push(5 * c, 7 * c + 1);
        assert_eq!(stream_windows(n, Some(&two)), vec![(0, c), (5 * c, 8 * c)]);
        assert!(stream_windows(0, None).is_empty());
    }

    /// 流して作るダイジェストは、値のダイジェストと同じ（区間の切り方によらない）。順でない区間は断る。
    #[test]
    fn pcm_digest_equals_value_digest() {
        let frames = 2 * AUDIO_CHUNK + 37;
        let planes: Vec<Arc<Vec<f32>>> = (0..2)
            .map(|c| {
                Arc::new(
                    (0..frames)
                        .map(|i| {
                            if i == 9 {
                                f32::NAN
                            } else {
                                (i + c) as f32 * 1e-5
                            }
                        })
                        .collect(),
                )
            })
            .collect();
        let a = Audio::new(8_000, planes.clone(), AudioMeta::default()).unwrap();
        let want = crate::value::Value::Audio(Arc::new(a.clone()))
            .digest()
            .unwrap();
        for step in [1usize, 7, 16_384, AUDIO_CHUNK + 1, frames] {
            let mut d = PcmDigest::new(8_000, 2, frames as u64);
            for (ch, p) in planes.iter().enumerate() {
                let mut at = 0;
                while at < frames {
                    let end = (at + step).min(frames);
                    d.push(ch, at as u64, &p[at..end]).unwrap();
                    at = end;
                }
            }
            assert_eq!(d.finish(&a.meta).unwrap(), want, "{step}");
        }
        let mut d = PcmDigest::new(8_000, 2, frames as u64);
        assert!(d.push(1, 0, &planes[1][..3]).is_err());
        assert!(d.push(0, 1, &planes[0][..3]).is_err());
        d.push(0, 0, &planes[0][..3]).unwrap();
        assert!(d.finish(&a.meta).is_err());
        // 空の音声
        let empty = Audio::new(8_000, vec![Arc::new(Vec::new())], AudioMeta::default()).unwrap();
        let want = crate::value::Value::Audio(Arc::new(empty.clone()))
            .digest()
            .unwrap();
        assert_eq!(
            PcmDigest::new(8_000, 1, 0).finish(&empty.meta).unwrap(),
            want
        );
        // 流れで読む
        struct Collect(Vec<Vec<f32>>, Option<StreamInfo>);
        impl AudioSink for Collect {
            fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
                self.1 = Some(info.clone());
                Ok(())
            }
            fn feed(
                &mut self,
                _start: u64,
                planes: &[&[f32]],
                _: &dyn Monitor,
            ) -> Result<(), EngineError> {
                for (o, p) in self.0.iter_mut().zip(planes) {
                    o.extend_from_slice(p);
                }
                Ok(())
            }
            fn finish(&mut self) -> Result<(), EngineError> {
                Ok(())
            }
        }
        let mut sink = Collect(vec![Vec::new(); 2], None);
        stream_value(&a, None, &mut sink, &NoMonitor).unwrap();
        assert_eq!(sink.0[0].len(), frames);
        assert!(
            sink.0[1]
                .iter()
                .zip(planes[1].iter())
                .all(|(x, y)| x.to_bits() == y.to_bits())
        );
        assert_eq!(sink.1.unwrap().frames, frames as u64);
    }
}
