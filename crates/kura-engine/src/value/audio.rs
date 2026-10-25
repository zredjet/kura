//! 音声の値（docs/editing.md §5.3）。すべてのチャンネルは同じ長さ。
//!
//! 中身は読む口（[`Audio::read`]・[`Audio::chunk_data`]・[`Audio::for_each_chunk`]・[`Audio::read_planes`]）で読む。
//! チャンクは正規の区切り [k·C, min((k+1)·C, frames))（C = [`AUDIO_CHUNK`]）で、どの読み方でも同じ値を返す。
//! 長い音声を組むときは [`AudioBuilder`] で標本を足していく（値と同じ量しか持たない）。

use std::ops::{Deref, Range};
use std::sync::Arc;

use super::meta::AudioMeta;
use crate::error::EngineError;
use crate::monitor::Monitor;

/// 音声のチャンクのフレーム数（M13。正規の区切り [k·C, (k+1)·C)）。並列の分け方と同じく値に効かない定数で、
/// どの約束の形式（ダイジェスト・kura-peaks・配信の URL）にも出さない。`AUDIO_BLOCK`（4,096）・`SUM_BLOCK`（4,096）・
/// `DIGEST_CHUNK`（16,384）の倍数で、波形の段の最も粗いビン（65,536）と同じ。
pub const AUDIO_CHUNK: usize = 1 << 16;

/// 音声。チャンネルが 1〜65,535、すべてのチャンネルは同じ長さ。
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    rate: u32,
    planes: Vec<Arc<Vec<f32>>>,
    pub meta: AudioMeta,
}

/// 写さずに借りたチャンクの中身（`Deref` で `[f32]`）。
#[derive(Debug, Clone)]
pub enum ChunkData<'a> {
    /// 値の中身を借りたもの。
    Borrowed(&'a [f32]),
    /// 共有の実体（源から読んだものなど）。
    Shared(Arc<Vec<f32>>),
}

impl Deref for ChunkData<'_> {
    type Target = [f32];

    fn deref(&self) -> &[f32] {
        match self {
            Self::Borrowed(s) => s,
            Self::Shared(v) => v.as_slice(),
        }
    }
}

fn check_header(rate: u32, channels: usize) -> Result<(), EngineError> {
    if rate == 0 || channels == 0 || channels > usize::from(u16::MAX) {
        return Err(EngineError::Internal(
            "音声のレート・チャンネルの数が正しくありません".into(),
        ));
    }
    Ok(())
}

impl Audio {
    /// 音声を作る（短い音声・テスト・外のクレート用の互換の口。長い音声を組むときは [`AudioBuilder`]）。チャンネルが 0 か
    /// 65,535 を超える、長さがそろっていない、レートが 0 ならエラー。
    pub fn new(
        rate: u32,
        planes: Vec<Arc<Vec<f32>>>,
        meta: AudioMeta,
    ) -> Result<Self, EngineError> {
        check_header(rate, planes.len())?;
        let len = planes[0].len();
        if planes.iter().any(|p| p.len() != len) {
            return Err(EngineError::Internal(
                "チャンネルの長さがそろっていません".into(),
            ));
        }
        Ok(Self { rate, planes, meta })
    }

    /// サンプルレート。
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// チャンネル数（1〜65,535）。
    pub fn channels(&self) -> usize {
        self.planes.len()
    }

    /// 1 チャンネルあたりのフレーム数。
    pub fn frames(&self) -> usize {
        self.planes[0].len()
    }

    /// チャンクの数（⌈frames / C⌉。長さ 0 なら 0）。
    pub fn chunk_count(&self) -> usize {
        self.frames().div_ceil(AUDIO_CHUNK)
    }

    /// チャンク `k` のフレームの区間 [k·C, min((k+1)·C, frames))（範囲の外の k は空）。
    pub fn chunk_range(&self, k: usize) -> Range<usize> {
        let n = self.frames();
        let start = k.saturating_mul(AUDIO_CHUNK).min(n);
        start..start.saturating_add(AUDIO_CHUNK).min(n)
    }

    fn plane(&self, ch: usize) -> Result<&Arc<Vec<f32>>, EngineError> {
        self.planes.get(ch).ok_or_else(|| {
            EngineError::Internal(format!(
                "音声のチャンネル {ch} はありません（{} チャンネル）",
                self.planes.len()
            ))
        })
    }

    /// チャンネル `ch` のフレーム `start` から `out.len()` 個を `out` に写す（範囲の外は 0）。チャンネルが範囲の外なら
    /// `Internal`。核が入力を読む口（範囲の描画で描かなかったチャンクを読めば `Internal`、源の読みの誤りは `Io`）。
    pub fn read(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
        let plane = self.plane(ch)?;
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        for (i, o) in out.iter_mut().enumerate() {
            *o = start
                .checked_add(i)
                .and_then(|k| plane.get(k))
                .copied()
                .unwrap_or(0.0);
        }
        Ok(())
    }

    /// チャンネル `ch` のチャンク `k` の中身（写さずに借りる。長さは正規の長さ）。範囲の外は `Internal`。
    pub fn chunk_data(&self, ch: usize, k: usize) -> Result<ChunkData<'_>, EngineError> {
        let plane = self.plane(ch)?;
        if k >= self.chunk_count() {
            return Err(EngineError::Internal(format!(
                "音声のチャンク {k} はありません（{} 個）",
                self.chunk_count()
            )));
        }
        Ok(ChunkData::Borrowed(&plane[self.chunk_range(k)]))
    }

    /// チャンクを番号の順に、全チャンネルの中身（チャンネルの順）を渡して `f(番号, 中身)` を呼ぶ。チャンクごとに中止を
    /// 確かめる。`f` が `Err` を返したらそこで止める。
    pub fn for_each_chunk(
        &self,
        mon: &dyn Monitor,
        mut f: impl FnMut(usize, &[ChunkData<'_>]) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        let mut data = Vec::with_capacity(self.channels());
        for k in 0..self.chunk_count() {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            data.clear();
            for ch in 0..self.channels() {
                data.push(self.chunk_data(ch, k)?);
            }
            f(k, &data)?;
        }
        Ok(())
    }

    /// フレームの区間 [start, end) を、チャンネルごとの連続した列にして返す（区間は長さの中であること）。
    pub fn read_planes(&self, start: usize, end: usize) -> Result<Vec<Vec<f32>>, EngineError> {
        if start > end || end > self.frames() {
            return Err(EngineError::Internal(format!(
                "音声の区間 [{start}, {end}) が長さ {} の外です",
                self.frames()
            )));
        }
        let mut out = Vec::with_capacity(self.channels());
        for ch in 0..self.channels() {
            let mut v = crate::codec::try_vec(end - start, 0f32)?;
            self.read(ch, start as u64, &mut v)?;
            out.push(v);
        }
        Ok(out)
    }

    /// 全体をチャンネルごとの連続した列にする（テストと短い音声だけ。本番のコードでは使わない：`tests/rules.rs`）。
    pub fn to_planes(&self) -> Result<Vec<Arc<Vec<f32>>>, EngineError> {
        Ok(self.planes.clone())
    }

    /// 標本の実体を共有したまま、メタデータだけを `meta` にした値。
    pub fn with_meta(&self, meta: AudioMeta) -> Self {
        Self {
            rate: self.rate,
            planes: self.planes.clone(),
            meta,
        }
    }

    /// チャンネル `ch` のチャンク `k` が、`other` の同じ位置のチャンクと同じ実体（写さずに共有したもの）なら true
    /// （中身を比べない。変わった区間の絞り込みに使う）。
    pub(crate) fn same_chunk(&self, other: &Audio, ch: usize, k: usize) -> bool {
        match (self.planes.get(ch), other.planes.get(ch)) {
            (Some(a), Some(b)) => {
                Arc::ptr_eq(a, b) && k < self.chunk_count() && self.frames() == other.frames()
            }
            _ => false,
        }
    }

    /// 中身の実体ごとの（番地, バイト数）を `out` に足す（`Value::chunks`）。
    pub(crate) fn chunks_into(&self, out: &mut Vec<(usize, u64)>) {
        for p in &self.planes {
            out.push((Arc::as_ptr(p).addr(), (p.len() as u64).saturating_mul(4)));
        }
    }
}

/// 標本を足していって音声を組む（デコーダ用。値と同じ量しか持たない）。`window` で、足した標本の頭を飛ばし、長さを
/// 切れる（編集リストのある AAC など）。
pub struct AudioBuilder {
    rate: u32,
    planes: Vec<Vec<f32>>,
    /// 頭から飛ばすフレームの残り。
    skip: u64,
    /// 残すフレームの上限の残り。
    take: u64,
}

impl AudioBuilder {
    /// レート `rate`・`channels` チャンネルの音声を組み始める。`frames_hint` は見込みの長さ（上限。確保の目安だけで値に
    /// 効かない）。レート・チャンネルの数が正しくなければエラー。
    pub fn new(rate: u32, channels: usize, frames_hint: u64) -> Result<Self, EngineError> {
        check_header(rate, channels)?;
        let hint = usize::try_from(frames_hint).unwrap_or(usize::MAX);
        let mut planes = Vec::with_capacity(channels);
        for _ in 0..channels {
            let mut p = Vec::new();
            p.try_reserve_exact(hint)
                .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            planes.push(p);
        }
        Ok(Self {
            rate,
            planes,
            skip: 0,
            take: u64::MAX,
        })
    }

    /// 足した標本のうち、頭の `skip` フレームを捨て、続く `take` フレームだけを残す。
    #[must_use]
    pub fn window(mut self, skip: u64, take: u64) -> Self {
        self.skip = skip;
        self.take = take;
        self
    }

    /// 残したフレームの数。
    pub fn frames(&self) -> u64 {
        self.planes.first().map_or(0, |p| p.len() as u64)
    }

    /// チャンネルごとの標本の列（チャンネルの数だけ、同じ長さ）を足す。
    pub fn push_frames(&mut self, planes: &[&[f32]]) -> Result<(), EngineError> {
        if planes.len() != self.planes.len() {
            return Err(EngineError::Internal(format!(
                "チャンネルの数が違います（{} と {}）",
                planes.len(),
                self.planes.len()
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
        for (dst, src) in self.planes.iter_mut().zip(planes) {
            dst.try_reserve(keep)
                .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            dst.extend_from_slice(&src[skip..skip + keep]);
        }
        Ok(())
    }

    /// インターリーブした標本（フレームの順に全チャンネル）を足す。長さはチャンネルの数の倍数であること。
    pub fn push_interleaved(&mut self, samples: &[f32]) -> Result<(), EngineError> {
        let ch = self.planes.len();
        if !samples.len().is_multiple_of(ch) {
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

    /// 組み終えて値にする（メタデータは `meta`）。
    pub fn finish(self, meta: AudioMeta) -> Result<Audio, EngineError> {
        Audio::new(
            self.rate,
            self.planes.into_iter().map(Arc::new).collect(),
            meta,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    fn ramp(channels: usize, frames: usize) -> Vec<Vec<f32>> {
        (0..channels)
            .map(|c| (0..frames).map(|i| (i * (c + 1)) as f32 * 1e-6).collect())
            .collect()
    }

    /// どの読む口も、平面から作った値と同じ中身を返す（チャンクの境をまたぐ長さで）。
    #[test]
    fn read_paths_agree() {
        for frames in [
            0usize,
            1,
            AUDIO_CHUNK - 1,
            AUDIO_CHUNK,
            AUDIO_CHUNK + 1,
            2 * AUDIO_CHUNK + 37,
        ] {
            let planes = ramp(2, frames);
            let a = Audio::new(
                8_000,
                planes.iter().cloned().map(Arc::new).collect(),
                AudioMeta::default(),
            )
            .unwrap();
            assert_eq!(a.chunk_count(), frames.div_ceil(AUDIO_CHUNK));
            let mut seen = vec![Vec::new(); 2];
            a.for_each_chunk(&NoMonitor, |k, data| {
                assert_eq!(data.len(), 2);
                for (ch, d) in data.iter().enumerate() {
                    assert_eq!(d.len(), a.chunk_range(k).len());
                    assert_eq!(&d[..], &*a.chunk_data(ch, k).unwrap());
                    seen[ch].extend_from_slice(d);
                }
                Ok(())
            })
            .unwrap();
            assert_eq!(seen, planes);
            assert_eq!(a.read_planes(0, frames).unwrap(), planes);
            if frames > 10 {
                let part = a.read_planes(3, frames - 2).unwrap();
                assert_eq!(part[1], planes[1][3..frames - 2]);
            }
            assert!(a.read_planes(0, frames + 1).is_err());
            let mut out = vec![9.0f32; 5];
            a.read(1, frames as u64 - frames.min(2) as u64, &mut out)
                .unwrap();
            assert_eq!(&out[frames.min(2)..], &[0.0; 5][frames.min(2)..]);
            assert!(a.read(2, 0, &mut out).is_err());
            assert!(a.chunk_data(0, a.chunk_count()).is_err());
        }
    }

    /// 組み手：平面・インターリーブで足しても、窓で切っても、平面から作った値と同じ。
    #[test]
    fn builder_matches_planes() {
        let frames = 2 * AUDIO_CHUNK + 37;
        let planes = ramp(3, frames);
        let want = Audio::new(
            44_100,
            planes.iter().cloned().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap();
        let mut b = AudioBuilder::new(44_100, 3, frames as u64).unwrap();
        let mut at = 0;
        for step in [1usize, 7, 4_096, AUDIO_CHUNK, usize::MAX].iter().cycle() {
            if at >= frames {
                break;
            }
            let end = at.saturating_add(*step).min(frames);
            let views: Vec<&[f32]> = planes.iter().map(|p| &p[at..end]).collect();
            b.push_frames(&views).unwrap();
            at = end;
        }
        assert_eq!(b.frames(), frames as u64);
        assert_eq!(b.finish(AudioMeta::default()).unwrap(), want);

        let mut inter = Vec::new();
        for f in 0..frames {
            for p in &planes {
                inter.push(p[f]);
            }
        }
        let mut b = AudioBuilder::new(44_100, 3, 0).unwrap().window(5, 70_000);
        for part in inter.chunks(3 * 1_000) {
            b.push_interleaved(part).unwrap();
        }
        let got = b.finish(AudioMeta::default()).unwrap();
        let want: Vec<Arc<Vec<f32>>> = planes
            .iter()
            .map(|p| Arc::new(p[5..70_005].to_vec()))
            .collect();
        assert_eq!(got.to_planes().unwrap(), want);
        assert!(AudioBuilder::new(0, 1, 0).is_err());
        assert!(AudioBuilder::new(1, 0, 0).is_err());
        let mut b = AudioBuilder::new(1, 2, 0).unwrap();
        assert!(b.push_frames(&[&[0.0]]).is_err());
        assert!(b.push_frames(&[&[0.0], &[]]).is_err());
        assert!(b.push_interleaved(&[0.0]).is_err());
    }
}
