//! 音声の流れ（M13 段 1。docs/editing.md §12.1・§12.5）：値や計画の出力を、頭から順に窓（`STREAM_CHUNKS` チャンク）ごとに
//! 描いて渡す口。段 2 の流れの解析・段 3 の保存がこの口に乗る。
//!
//! - [`AudioSink`]：`begin`（形とメタデータ）→ `feed`（頭から順・重ならず・チャンクに揃えた窓の全チャンネル）→ `finish`。
//! - [`stream_value`]：値（遅延の値を含む）を窓ごとに読んで渡す。計画の出力は `Engine::stream_audio`（render/audio.rs）。
//! - [`AudioProducer`]：何度でも区間を選んで流せるもの（M13 段 3。計画の出力は `Engine::output_stream`、値は
//!   [`ValueProducer`]）。エンコーダの `encode_stream` が読む。
//! - [`PcmDigest`]：`kura-pcm-v1` のダイジェスト（docs/editing.md §8.6）を、チャンネルの順に区間を足して作る。`Value::digest`
//!   と、区間の順によらない木のダイジェスト（`audio::digest::PcmTreeDigest`。流れの窓から 1 回で取る）の小さい音声の道が
//!   同じ部品を通る（式は変えない）。

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

/// 流れの終わり（M13 段 2）：実際の長さとメタデータ。長さが上限しか分からないデコーダ（MP3・AAC）は、読み終えるまで長さが
/// 決まらない。流れの解析（`AudioStream::finish`）とデコードの受け手（`codec::stream::DecodeSink::end`）が受ける。
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEnd {
    /// 受けたフレームの数。
    pub frames: u64,
    /// メタデータ。
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

/// 何度でも、区間を選んで流せる音声（M13 段 3。docs/editing.md §12.1）：計画の出力（`Engine::output_stream`）と値
/// （[`ValueProducer`]）。エンコーダはこれを読んで書く（`Encoder::encode_stream`。Ogg Vorbis は後ろ向き・前向きの 2 回流す）。
pub trait AudioProducer {
    /// 流れの形とメタデータ（どの区間を流しても同じ。`produce` の `begin` に渡すもの）。
    fn info(&self) -> &StreamInfo;
    /// `spans` に掛かるチャンク（None は全体）を窓ごとに `sink` へ流す（`begin` → 窓 → `finish`）。何度呼んでもよく、同じ
    /// 区間なら同じ標本を渡す。
    fn produce(
        &mut self,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError>;
    /// 流した標本のダイジェスト（`kura-pcm-v1`。全体を 1 度でも流し終えていれば Some。ダイジェストを取らない生産者は None）。
    fn digest(&mut self) -> Result<Option<Digest>, EngineError> {
        Ok(None)
    }
}

/// 値（遅延の値を含む）を流す生産者（[`stream_value`] を包む。値の `encode` が流して書く口を通るための形）。
pub struct ValueProducer<'a> {
    audio: &'a Audio,
    info: StreamInfo,
}

impl<'a> ValueProducer<'a> {
    /// 値 `audio` を流す生産者。
    pub fn new(audio: &'a Audio) -> Self {
        Self {
            audio,
            info: StreamInfo {
                rate: audio.rate(),
                channels: audio.channels(),
                frames: audio.frames() as u64,
                meta: audio.meta.clone(),
            },
        }
    }
}

impl AudioProducer for ValueProducer<'_> {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn produce(
        &mut self,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        stream_value(self.audio, spans, sink, mon)
    }
}

/// 生産者を包み、流れた標本を一度だけ値のダイジェスト（`kura-pcm-v1`。区間の順によらない `PcmTreeDigest`）に足す
/// （M13 段 3。R1 を流しながら取る）。同じ区間を何度流しても（Ogg Vorbis の 2 回目）、足すのは初めて流れた部分だけ。
/// [`AudioProducer::digest`] は全体を覆い終えていれば Some。
pub struct DigestingProducer<'a> {
    inner: &'a mut dyn AudioProducer,
    state: DigestState,
}

/// ダイジェストの途中・終わり。
enum DigestState {
    Open {
        digest: Box<crate::audio::digest::PcmTreeDigest>,
        /// 足した区間（[start, end)。重ならず、隣り合うものはまとめる）。
        covered: std::collections::BTreeMap<u64, u64>,
    },
    Done(Digest),
    Taken,
}

impl<'a> DigestingProducer<'a> {
    /// `inner` を包む（形は `inner.info()`）。
    pub fn new(inner: &'a mut dyn AudioProducer) -> Result<Self, EngineError> {
        let info = inner.info();
        let digest =
            crate::audio::digest::PcmTreeDigest::new(info.rate, info.channels, info.frames)?;
        Ok(Self {
            inner,
            state: DigestState::Open {
                digest: Box::new(digest),
                covered: std::collections::BTreeMap::new(),
            },
        })
    }
}

/// [s, e) のうち `covered` にない区間（小さい順）。
fn uncovered(covered: &std::collections::BTreeMap<u64, u64>, s: u64, e: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut at = s;
    // at を含む区間があれば、その終わりまで飛ばす
    if let Some((_, pe)) = covered.range(..=at).next_back()
        && *pe > at
    {
        at = *pe;
    }
    for (&cs, &ce) in covered.range(at..e) {
        if cs > at {
            out.push((at, cs.min(e)));
        }
        at = at.max(ce);
        if at >= e {
            break;
        }
    }
    if at < e {
        out.push((at, e));
    }
    out
}

/// [s, e) を `covered` に足す（隣り合う区間はまとめる。重なりは呼び手が除いてある）。
fn cover(covered: &mut std::collections::BTreeMap<u64, u64>, s: u64, e: u64) {
    if s >= e {
        return;
    }
    let (mut start, mut end) = (s, e);
    if let Some((ps, pe)) = covered.range(..=s).next_back().map(|(a, b)| (*a, *b))
        && pe >= s
    {
        covered.remove(&ps);
        start = ps;
        end = end.max(pe);
    }
    while let Some((ns, ne)) = covered.range(start..=end).next().map(|(a, b)| (*a, *b)) {
        covered.remove(&ns);
        end = end.max(ne);
    }
    covered.insert(start, end);
}

/// 流れの窓をダイジェストに足してから、呼び手の受け手へ渡す包み。
struct DigestSink<'s> {
    sink: &'s mut dyn AudioSink,
    state: &'s mut DigestState,
}

impl AudioSink for DigestSink<'_> {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        self.sink.begin(info)
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        if let DigestState::Open { digest, covered } = self.state {
            let n = planes.first().map_or(0, |p| p.len()) as u64;
            for (a, b) in uncovered(covered, start, start + n) {
                let (x, y) = ((a - start) as usize, (b - start) as usize);
                let parts: Vec<&[f32]> = planes.iter().map(|p| &p[x..y]).collect();
                digest.push_window(a, &parts, mon)?;
                cover(covered, a, b);
            }
        }
        self.sink.feed(start, planes, mon)
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        self.sink.finish()
    }
}

impl AudioProducer for DigestingProducer<'_> {
    fn info(&self) -> &StreamInfo {
        self.inner.info()
    }

    fn produce(
        &mut self,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let mut wrap = DigestSink {
            sink,
            state: &mut self.state,
        };
        self.inner.produce(spans, &mut wrap, mon)
    }

    fn digest(&mut self) -> Result<Option<Digest>, EngineError> {
        match std::mem::replace(&mut self.state, DigestState::Taken) {
            DigestState::Open { digest, covered } => {
                if digest.is_complete() {
                    let d = digest.finish(&self.inner.info().meta)?;
                    self.state = DigestState::Done(d);
                    Ok(Some(d))
                } else {
                    self.state = DigestState::Open { digest, covered };
                    Ok(None)
                }
            }
            DigestState::Done(d) => {
                self.state = DigestState::Done(d);
                Ok(Some(d))
            }
            DigestState::Taken => Err(EngineError::Internal(
                "流れのダイジェストの状態が失われました".into(),
            )),
        }
    }
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

    /// 区間の表：覆っていない部分と、まとめて足す形。
    #[test]
    fn uncovered_and_cover_track_spans() {
        let mut m = std::collections::BTreeMap::new();
        assert_eq!(uncovered(&m, 0, 10), vec![(0, 10)]);
        cover(&mut m, 2, 4);
        cover(&mut m, 6, 8);
        assert_eq!(uncovered(&m, 0, 10), vec![(0, 2), (4, 6), (8, 10)]);
        assert_eq!(uncovered(&m, 3, 7), vec![(4, 6)]);
        cover(&mut m, 4, 6);
        assert_eq!(m.len(), 1);
        assert_eq!(uncovered(&m, 2, 8), vec![]);
        cover(&mut m, 0, 2);
        cover(&mut m, 8, 10);
        assert_eq!(m.into_iter().collect::<Vec<_>>(), vec![(0, 10)]);
    }

    /// ダイジェストの包み：後ろから・何度流しても、値のダイジェストと同じ（覆い終えるまでは None）。
    #[test]
    fn digesting_producer_counts_each_frame_once() {
        let frames = 3 * AUDIO_CHUNK + 5;
        let planes: Vec<Arc<Vec<f32>>> = (0..2)
            .map(|c| {
                Arc::new(
                    (0..frames)
                        .map(|i| ((i * 7 + c) % 13) as f32 * 0.01)
                        .collect(),
                )
            })
            .collect();
        let a = Audio::new(8_000, planes, AudioMeta::default()).unwrap();
        let want = crate::value::Value::Audio(Arc::new(a.clone()))
            .digest()
            .unwrap();
        struct Null;
        impl AudioSink for Null {
            fn begin(&mut self, _: &StreamInfo) -> Result<(), EngineError> {
                Ok(())
            }
            fn feed(&mut self, _: u64, _: &[&[f32]], _: &dyn Monitor) -> Result<(), EngineError> {
                Ok(())
            }
            fn finish(&mut self) -> Result<(), EngineError> {
                Ok(())
            }
        }
        let mut inner = ValueProducer::new(&a);
        let mut d = DigestingProducer::new(&mut inner).unwrap();
        let c = AUDIO_CHUNK as u64;
        d.produce(Some(&Spans::one(3 * c, 3 * c + 1)), &mut Null, &NoMonitor)
            .unwrap();
        assert_eq!(d.digest().unwrap(), None);
        d.produce(Some(&Spans::one(c, 2 * c + 1)), &mut Null, &NoMonitor)
            .unwrap();
        d.produce(None, &mut Null, &NoMonitor).unwrap();
        assert_eq!(d.digest().unwrap(), Some(want));
        d.produce(None, &mut Null, &NoMonitor).unwrap();
        assert_eq!(d.digest().unwrap(), Some(want));
    }

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
