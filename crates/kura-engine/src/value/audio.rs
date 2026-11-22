//! 音声の値（docs/editing.md §5.3）。すべてのチャンネルは同じ長さ。
//!
//! 中身はチャンネルごとの正規の区切りのチャンク（[k·C, min((k+1)·C, frames))。C = [`AUDIO_CHUNK`]）の表で持つ。チャンクは
//! `Arc` で共有し、操作は描いたチャンクだけを新しく作る（画像のタイルと同じ考え方）。範囲の描画で描かなかったチャンクは
//! 無効（[`Chunk::Hole`]。中身を持たず、読むと誤りで、読んだ印が立つ）。遅延の値のチャンクは源の参照
//! （[`Chunk::Source`]。読むときに源（`audio::source`）の LRU から読む）。
//!
//! 中身は読む口（[`Audio::read`]・[`Audio::chunk_data`]・[`Audio::for_each_chunk`]・[`Audio::read_planes`]）で読む。
//! どの読み方でも同じ値を返す。長い音声を組むときは [`AudioBuilder`] で標本を足していく（値と同じ量しか持たない）。
//! 区切りは正規なので、同じ長さの値は同じ形の表を持ち、等しさ・共有の比べ・継ぎはぎを番号だけで行える。

use std::fmt;
use std::ops::{Deref, Range};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::meta::AudioMeta;
use crate::audio::source::AudioSource;
use crate::error::EngineError;
use crate::monitor::Monitor;

/// 音声のチャンクのフレーム数（M13。正規の区切り [k·C, (k+1)·C)）。並列の分け方と同じく値に効かない定数で、
/// どの約束の形式（ダイジェスト・kura-peaks・配信の URL）にも出さない。`AUDIO_BLOCK`（4,096）・`SUM_BLOCK`（4,096）・
/// `DIGEST_CHUNK`（16,384）の倍数で、波形の段の最も粗いビン（65,536）と同じ。1 チャンネル 1 チャンクが f32 で 256 KiB。
pub const AUDIO_CHUNK: usize = 1 << 16;

/// チャンク 1 つ（1 チャンネル分）。
#[derive(Debug, Clone)]
pub enum Chunk {
    /// 中身（長さは正規の長さ）。
    Data(Arc<Vec<f32>>),
    /// 源の参照（遅延。読むときに源から読む。源の中身は不変なので、値の不変の約束と両立する）。
    Source(SourceRef),
    /// 無効（範囲の描画で描かなかったもの。中身を持たない。読むと誤り）。全チャンネルで同じ番号にそろう。
    Hole,
}

/// 源のチャンクの参照：源 `src` のチャンク `index` のチャンネル `ch`。
#[derive(Clone)]
pub struct SourceRef {
    pub src: Arc<dyn AudioSource>,
    pub ch: u16,
    pub index: u32,
}

impl SourceRef {
    /// 同じ源の同じチャンネル・同じチャンクなら true（中身を読まない）。
    pub fn same(&self, other: &SourceRef) -> bool {
        source_addr(&self.src) == source_addr(&other.src)
            && self.ch == other.ch
            && self.index == other.index
    }

    /// 中身（源から読む）。
    pub fn read(&self) -> Result<Arc<Vec<f32>>, EngineError> {
        self.src
            .read_chunk(self.index)?
            .get(usize::from(self.ch))
            .cloned()
            .ok_or_else(|| EngineError::Internal("音声の源のチャンネルが足りません".into()))
    }
}

impl fmt::Debug for SourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceRef")
            .field("src", &source_addr(&self.src))
            .field("ch", &self.ch)
            .field("index", &self.index)
            .finish()
    }
}

/// 源の実体の番地（共有の比べ・キャッシュの数えに使う。値に効かない）。
fn source_addr(src: &Arc<dyn AudioSource>) -> usize {
    Arc::as_ptr(src).cast::<()>().addr()
}

/// 無効なチャンクを読んだ印（範囲の描画が申告した依存の外を読んだことを、実行器とテストが見つけるため）。
#[derive(Debug, Default)]
struct HoleFlag {
    read: AtomicBool,
}

/// 音声。チャンネルが 1〜65,535、すべてのチャンネルは同じ長さ。
#[derive(Clone)]
pub struct Audio {
    rate: u32,
    frames: usize,
    /// [チャンネル][チャンクの番号]。メタデータだけを替える写しは表を共有する。
    table: Arc<Vec<Vec<Chunk>>>,
    /// 無効なチャンクがあるときだけ（読んだ印）。
    holes: Option<Arc<HoleFlag>>,
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

/// チャンクの番号の集合（`universe` 個の番号 0..universe の部分集合）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSet {
    universe: usize,
    bits: Vec<u64>,
}

impl ChunkSet {
    /// 空の集合（番号は 0..universe）。
    pub fn empty(universe: usize) -> Self {
        Self {
            universe,
            bits: vec![0; universe.div_ceil(64)],
        }
    }

    /// すべての番号。
    pub fn all(universe: usize) -> Self {
        let mut s = Self::empty(universe);
        for k in 0..universe {
            s.insert(k);
        }
        s
    }

    /// 番号の範囲（0..universe）の大きさ。
    pub fn universe(&self) -> usize {
        self.universe
    }

    /// 番号を足す（範囲の外は無視）。
    pub fn insert(&mut self, k: usize) {
        if k < self.universe
            && let Some(w) = self.bits.get_mut(k / 64)
        {
            *w |= 1 << (k % 64);
        }
    }

    /// 番号を除く。
    pub fn remove(&mut self, k: usize) {
        if let Some(w) = self.bits.get_mut(k / 64) {
            *w &= !(1 << (k % 64));
        }
    }

    /// 番号を含めば true。
    pub fn contains(&self, k: usize) -> bool {
        k < self.universe
            && self
                .bits
                .get(k / 64)
                .is_some_and(|w| (w >> (k % 64)) & 1 == 1)
    }

    /// 番号の数。
    pub fn count(&self) -> usize {
        self.bits.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// 空なら true。
    pub fn is_empty(&self) -> bool {
        self.bits.iter().all(|w| *w == 0)
    }

    /// 番号を小さい順に。
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.universe).filter(|k| self.contains(*k))
    }

    /// 和（`other` の番号を足す）。
    pub fn union_with(&mut self, other: &ChunkSet) {
        for k in other.iter() {
            self.insert(k);
        }
    }

    /// 差（`other` の番号を除く）。
    pub fn subtract(&mut self, other: &ChunkSet) {
        for k in other.iter() {
            self.remove(k);
        }
    }

    /// 共通部分（`other` にない番号を除く）。
    pub fn intersect_with(&mut self, other: &ChunkSet) {
        for k in 0..self.universe {
            if !other.contains(k) {
                self.remove(k);
            }
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

/// 長さ `frames` の音声のチャンク `k` の区間。
fn range_of(frames: usize, k: usize) -> Range<usize> {
    let start = k.saturating_mul(AUDIO_CHUNK).min(frames);
    start..start.saturating_add(AUDIO_CHUNK).min(frames)
}

/// 平面を正規のチャンクに分ける。1 チャンク以下なら写さずにそのまま使い、長ければ写す（元は呼び手が手放す）。
fn split_plane(p: Arc<Vec<f32>>) -> Result<Vec<Chunk>, EngineError> {
    if p.is_empty() {
        return Ok(Vec::new());
    }
    if p.len() <= AUDIO_CHUNK {
        return Ok(vec![Chunk::Data(p)]);
    }
    let mut out = Vec::new();
    out.try_reserve_exact(p.len().div_ceil(AUDIO_CHUNK))
        .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
    for c in p.chunks(AUDIO_CHUNK) {
        let mut v = Vec::new();
        v.try_reserve_exact(c.len())
            .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        v.extend_from_slice(c);
        out.push(Chunk::Data(Arc::new(v)));
    }
    Ok(out)
}

fn hole_error() -> EngineError {
    EngineError::Internal("範囲の描画で描かなかった音声のチャンクを読みました".into())
}

impl Audio {
    /// 音声を作る（短い音声・テスト・外のクレート用の互換の口。長い音声を組むときは [`AudioBuilder`]）。平面を正規の
    /// チャンクに分ける（1 チャンク以下のチャンネルは写さない。長いチャンネルは 1 本ずつ写して元を手放すので、写す間に
    /// 余分に持つのは 1 チャンネル分だけ）。チャンネルが 0 か 65,535 を超える、長さがそろっていない、レートが 0 ならエラー。
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
        let mut table = Vec::with_capacity(planes.len());
        for p in planes {
            table.push(split_plane(p)?);
        }
        Ok(Self {
            rate,
            frames: len,
            table: Arc::new(table),
            holes: None,
            meta,
        })
    }

    /// チャンクの表から作る。表はチャンネルの数だけの行で、各行は ⌈frames / C⌉ 個のチャンク。中身の長さは正規の長さ、
    /// 無効なチャンクは全チャンネルで同じ番号にそろうこと（合わなければ `Internal`）。
    pub fn from_chunks(
        rate: u32,
        frames: usize,
        table: Vec<Vec<Chunk>>,
        meta: AudioMeta,
    ) -> Result<Self, EngineError> {
        check_header(rate, table.len())?;
        let count = frames.div_ceil(AUDIO_CHUNK);
        let mut any_hole = false;
        for row in &table {
            if row.len() != count {
                return Err(EngineError::Internal(format!(
                    "音声のチャンクの数が長さと合いません（{} 個。{frames} フレームなら {count} 個）",
                    row.len()
                )));
            }
        }
        for row in &table {
            for (k, c) in row.iter().enumerate() {
                let hole0 = matches!(table[0][k], Chunk::Hole);
                let want = range_of(frames, k).len();
                match c {
                    Chunk::Data(v) if v.len() != want || hole0 => {
                        return Err(EngineError::Internal(format!(
                            "音声のチャンク {k} の長さか無効の位置が正しくありません"
                        )));
                    }
                    Chunk::Source(r) => {
                        let len = crate::audio::source::chunk_span(r.src.frames(), r.index)
                            .map_or(0, |(a, b)| b - a);
                        if hole0
                            || r.src.rate() != rate
                            || usize::from(r.ch) >= r.src.channels()
                            || len != want as u64
                        {
                            return Err(EngineError::Internal(format!(
                                "音声のチャンク {k} の源がレート・チャンネル・長さと合いません"
                            )));
                        }
                    }
                    Chunk::Hole if !hole0 => {
                        return Err(EngineError::Internal(format!(
                            "音声の無効なチャンク {k} がチャンネルでそろっていません"
                        )));
                    }
                    Chunk::Hole => any_hole = true,
                    Chunk::Data(_) => {}
                }
            }
        }
        Ok(Self {
            rate,
            frames,
            table: Arc::new(table),
            holes: any_hole.then(|| Arc::new(HoleFlag::default())),
            meta,
        })
    }

    /// 源の全体を遅延の値にする（全チャンクが源の参照。開くときに全体を読まない）。
    pub fn from_source(src: Arc<dyn AudioSource>, meta: AudioMeta) -> Result<Self, EngineError> {
        let frames = usize::try_from(src.frames())
            .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        let count = frames.div_ceil(AUDIO_CHUNK);
        let count32 =
            u32::try_from(count).map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        let channels = src.channels();
        check_header(src.rate(), channels)?;
        let mut table = Vec::with_capacity(channels);
        for ch in 0..channels {
            let ch16 = u16::try_from(ch)
                .map_err(|_| EngineError::Internal("チャンネルが多すぎます".into()))?;
            let mut row = Vec::new();
            row.try_reserve_exact(count)
                .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            for index in 0..count32 {
                row.push(Chunk::Source(SourceRef {
                    src: src.clone(),
                    ch: ch16,
                    index,
                }));
            }
            table.push(row);
        }
        Self::from_chunks(src.rate(), frames, table, meta)
    }

    /// サンプルレート。
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// チャンネル数（1〜65,535）。
    pub fn channels(&self) -> usize {
        self.table.len()
    }

    /// 1 チャンネルあたりのフレーム数。
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// チャンクの数（⌈frames / C⌉。長さ 0 なら 0）。
    pub fn chunk_count(&self) -> usize {
        self.frames.div_ceil(AUDIO_CHUNK)
    }

    /// チャンク `k` のフレームの区間 [k·C, min((k+1)·C, frames))（範囲の外の k は空）。
    pub fn chunk_range(&self, k: usize) -> Range<usize> {
        range_of(self.frames, k)
    }

    /// チャンネル `ch` のチャンク `k`（範囲の外は `Internal`）。
    pub fn chunk(&self, ch: usize, k: usize) -> Result<&Chunk, EngineError> {
        self.table
            .get(ch)
            .and_then(|row| row.get(k))
            .ok_or_else(|| {
                EngineError::Internal(format!(
                    "音声のチャンク（チャンネル {ch}・番号 {k}）はありません（{} チャンネル・{} 個）",
                    self.channels(),
                    self.chunk_count()
                ))
            })
    }

    /// チャンネル `ch` のフレーム `start` から `out.len()` 個を `out` に写す（範囲の外は 0）。チャンネルが範囲の外、
    /// 無効なチャンク（範囲の描画で描かなかったもの）を読んだときは `Internal`（読んだ印が立つ）。核が入力を読む口。
    pub fn read(&self, ch: usize, start: u64, out: &mut [f32]) -> Result<(), EngineError> {
        if ch >= self.channels() {
            return Err(EngineError::Internal(format!(
                "音声のチャンネル {ch} はありません（{} チャンネル）",
                self.channels()
            )));
        }
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        let mut i = 0usize;
        while i < out.len() {
            let Some(pos) = start.checked_add(i).filter(|p| *p < self.frames) else {
                out[i..].fill(0.0);
                break;
            };
            let k = pos / AUDIO_CHUNK;
            let off = pos - k * AUDIO_CHUNK;
            let data = self.chunk_data(ch, k)?;
            let take = (data.len() - off).min(out.len() - i);
            out[i..i + take].copy_from_slice(&data[off..off + take]);
            i += take;
        }
        Ok(())
    }

    /// チャンネル `ch` のチャンク `k` の中身（写さずに借りる。源の参照は源の LRU の実体。長さは正規の長さ）。範囲の外・
    /// 無効なチャンクは `Internal`（無効なら読んだ印が立つ）、源の読みの誤りはそのまま（`Io` など）。
    pub fn chunk_data(&self, ch: usize, k: usize) -> Result<ChunkData<'_>, EngineError> {
        match self.chunk(ch, k)? {
            Chunk::Data(v) => Ok(ChunkData::Borrowed(v.as_slice())),
            Chunk::Source(r) => {
                let v = r.read()?;
                if v.len() != self.chunk_range(k).len() {
                    return Err(EngineError::Internal(format!(
                        "音声の源のチャンク {} の長さが違います",
                        r.index
                    )));
                }
                Ok(ChunkData::Shared(v))
            }
            Chunk::Hole => {
                self.touch();
                Err(hole_error())
            }
        }
    }

    /// チャンネル `ch` のチャンク `k` を、実体を写さずに取り出す（中身・源の参照の写し。範囲の描画が、ずらした写しの
    /// チャンクを入力の実体で共有するのに使う）。範囲の外は `Internal`、無効なチャンクは読んだ印を立てて `Internal`。
    pub fn share_chunk(&self, ch: usize, k: usize) -> Result<Chunk, EngineError> {
        match self.chunk(ch, k)? {
            Chunk::Hole => {
                self.touch();
                Err(hole_error())
            }
            c => Ok(c.clone()),
        }
    }

    /// 無効なチャンクを読んだ印を立てる。
    fn touch(&self) {
        if let Some(h) = &self.holes {
            h.read.store(true, Ordering::Relaxed);
        }
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
        if start > end || end > self.frames {
            return Err(EngineError::Internal(format!(
                "音声の区間 [{start}, {end}) が長さ {} の外です",
                self.frames
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
    /// 1 チャンク以下のチャンネルは実体を写さずに共有する。
    pub fn to_planes(&self) -> Result<Vec<Arc<Vec<f32>>>, EngineError> {
        let mut out = Vec::with_capacity(self.channels());
        for (ch, row) in self.table.iter().enumerate() {
            match row.as_slice() {
                [] => out.push(Arc::new(Vec::new())),
                [Chunk::Data(v)] => out.push(v.clone()),
                _ => {
                    let mut v = crate::codec::try_vec(self.frames, 0f32)?;
                    self.read(ch, 0, &mut v)?;
                    out.push(Arc::new(v));
                }
            }
        }
        Ok(out)
    }

    /// 標本の実体（表）を共有したまま、メタデータだけを `meta` にした値（O(1)）。
    pub fn with_meta(&self, meta: AudioMeta) -> Self {
        Self {
            meta,
            ..self.clone()
        }
    }

    /// 有効なチャンクの番号の集合。
    pub fn valid_chunks(&self) -> ChunkSet {
        let mut s = ChunkSet::all(self.chunk_count());
        if self.holes.is_some()
            && let Some(row) = self.table.first()
        {
            for (k, c) in row.iter().enumerate() {
                if matches!(c, Chunk::Hole) {
                    s.remove(k);
                }
            }
        }
        s
    }

    /// すべてのチャンクが有効（範囲の描画で作った値でない）なら true。
    pub fn is_complete(&self) -> bool {
        self.holes.is_none()
    }

    /// 無効なチャンクを読んだことがあれば true（範囲の描画が申告した依存の外を読んだ印）。
    pub fn holes_read(&self) -> bool {
        self.holes
            .as_ref()
            .is_some_and(|h| h.read.load(Ordering::Relaxed))
    }

    /// `set` のチャンクを（全チャンネルで）無効にした値。無効にしたチャンクの中身は持たない（手放す）。読んだ印は新しく
    /// する。
    pub fn invalidate(&self, set: &ChunkSet) -> Self {
        let mut any = self.holes.is_some();
        let table: Vec<Vec<Chunk>> = self
            .table
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .map(|(k, c)| {
                        if set.contains(k) {
                            any = true;
                            Chunk::Hole
                        } else {
                            c.clone()
                        }
                    })
                    .collect()
            })
            .collect();
        Self {
            rate: self.rate,
            frames: self.frames,
            table: Arc::new(table),
            holes: any.then(|| Arc::new(HoleFlag::default())),
            meta: self.meta.clone(),
        }
    }

    /// `keep` のチャンクだけを残し、ほかを無効にした値。
    pub fn keep_only(&self, keep: &ChunkSet) -> Self {
        let mut drop = ChunkSet::all(self.chunk_count());
        drop.subtract(keep);
        self.invalidate(&drop)
    }

    /// チャンネル `ch` のチャンク `k` が、`other` の同じ位置のチャンクと同じ実体（写さずに共有したもの）なら true
    /// （中身を比べない。中身どうしは `Arc::ptr_eq`、源の参照どうしは同じ源・チャンネル・番号。どちらかが無効なら false。
    /// 変わった区間の絞り込みと継ぎはぎに使う）。
    pub fn same_chunk(&self, other: &Audio, ch: usize, k: usize) -> bool {
        match (self.chunk(ch, k), other.chunk(ch, k)) {
            (Ok(Chunk::Data(a)), Ok(Chunk::Data(b))) => Arc::ptr_eq(a, b),
            (Ok(Chunk::Source(a)), Ok(Chunk::Source(b))) => a.same(b),
            _ => false,
        }
    }

    /// 中身を持つチャンクのバイト数の和（無効なチャンク・源の参照は数えない。源の LRU は源が数える）。
    pub fn resident_bytes(&self) -> u64 {
        self.table
            .iter()
            .flatten()
            .map(|c| match c {
                Chunk::Data(v) => (v.len() as u64).saturating_mul(4),
                Chunk::Source(_) | Chunk::Hole => 0,
            })
            .fold(0u64, u64::saturating_add)
    }

    /// 中身の実体ごとの（番地, バイト数）を `out` に足す（`Value::chunks`）。中身はチャンクごと、源の参照は源 1 つにつき
    /// （源の番地, 源の LRU の上限）を 1 回（キャッシュは番地で 1 度だけ数えるので、遅延の入力を `register_external` すると
    /// 源の LRU の上限が予算に入る）。無効なチャンクは中身を持たないので数えない。
    pub(crate) fn chunks_into(&self, out: &mut Vec<(usize, u64)>) {
        let mut sources: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        for c in self.table.iter().flatten() {
            match c {
                Chunk::Data(v) => {
                    out.push((Arc::as_ptr(v).addr(), (v.len() as u64).saturating_mul(4)));
                }
                Chunk::Source(r) => {
                    let addr = source_addr(&r.src);
                    if sources.insert(addr) {
                        out.push((addr, r.src.resident_bytes()));
                    }
                }
                Chunk::Hole => {}
            }
        }
    }
}

impl PartialEq for Audio {
    /// レート・長さ・チャンネル・メタデータ・無効なチャンクの位置が同じで、有効なチャンクの中身が同じ（共有した実体は
    /// 中身を比べない。f32 の比べなので、共有していない NaN は自分と違う）。
    fn eq(&self, other: &Self) -> bool {
        if self.rate != other.rate
            || self.frames != other.frames
            || self.channels() != other.channels()
            || self.meta != other.meta
        {
            return false;
        }
        if Arc::ptr_eq(&self.table, &other.table) {
            return true;
        }
        self.table.iter().zip(other.table.iter()).all(|(ra, rb)| {
            ra.iter().zip(rb).all(|(a, b)| match (a, b) {
                (Chunk::Data(x), Chunk::Data(y)) => Arc::ptr_eq(x, y) || x == y,
                (Chunk::Source(x), Chunk::Source(y)) if x.same(y) => true,
                (Chunk::Hole, Chunk::Hole) => true,
                (Chunk::Hole, _) | (_, Chunk::Hole) => false,
                // 源の参照を含むなら読んで比べる（読めなければ等しくない）
                _ => match (content_of(a), content_of(b)) {
                    (Some(x), Some(y)) => x[..] == y[..],
                    _ => false,
                },
            })
        })
    }
}

/// 有効なチャンクの中身（源は読む。無効・読めなければ None）。
fn content_of(c: &Chunk) -> Option<ChunkData<'_>> {
    match c {
        Chunk::Data(v) => Some(ChunkData::Borrowed(v.as_slice())),
        Chunk::Source(r) => r.read().ok().map(ChunkData::Shared),
        Chunk::Hole => None,
    }
}

impl fmt::Debug for Audio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let holes = self.chunk_count() - self.valid_chunks().count();
        let sources = self.table.first().map_or(0, |row| {
            row.iter().filter(|c| matches!(c, Chunk::Source(_))).count()
        });
        f.debug_struct("Audio")
            .field("rate", &self.rate)
            .field("channels", &self.channels())
            .field("frames", &self.frames)
            .field("chunks", &self.chunk_count())
            .field("sources", &sources)
            .field("holes", &holes)
            .field("meta", &self.meta)
            .finish()
    }
}

/// 標本を足していって音声を組む（デコーダ用）。チャンクを `try_reserve_exact` で取って埋めていくので、値と同じ量しか
/// 持たない。`window` で、足した標本の頭を飛ばし、長さを切れる（編集リストのある AAC など）。
pub struct AudioBuilder {
    rate: u32,
    /// できあがったチャンク（チャンネルごと）。
    table: Vec<Vec<Chunk>>,
    /// 埋めている途中のチャンク（チャンネルごと。長さは C 未満でそろう）。
    cur: Vec<Vec<f32>>,
    /// 残したフレームの数。
    frames: u64,
    /// 見込みの長さ（途中のチャンクの確保の目安だけ）。
    hint: u64,
    /// 頭から飛ばすフレームの残り。
    skip: u64,
    /// 残すフレームの上限の残り。
    take: u64,
}

impl AudioBuilder {
    /// レート `rate`・`channels` チャンネルの音声を組み始める。`frames_hint` は見込みの長さ（確保の目安だけで値に
    /// 効かない）。レート・チャンネルの数が正しくなければエラー。
    pub fn new(rate: u32, channels: usize, frames_hint: u64) -> Result<Self, EngineError> {
        check_header(rate, channels)?;
        Ok(Self {
            rate,
            table: vec![Vec::new(); channels],
            cur: vec![Vec::new(); channels],
            frames: 0,
            hint: frames_hint,
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
        self.frames
    }

    /// 途中のチャンクがいっぱいなら、できあがったチャンクに移して次を確保する。
    fn reserve_next(&mut self) -> Result<(), EngineError> {
        // 次のチャンクの大きさの見込み（見込みの長さの残りか C の小さいほう。足りなければ C まで伸ばす）
        let left = self.hint.saturating_sub(self.frames);
        let want = usize::try_from(left)
            .unwrap_or(usize::MAX)
            .clamp(1, AUDIO_CHUNK);
        for (row, cur) in self.table.iter_mut().zip(self.cur.iter_mut()) {
            if cur.len() == AUDIO_CHUNK {
                let mut next = Vec::new();
                next.try_reserve_exact(want)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                row.try_reserve(1)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                row.push(Chunk::Data(Arc::new(std::mem::replace(cur, next))));
            } else if cur.capacity() == 0 {
                cur.try_reserve_exact(want)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            }
        }
        Ok(())
    }

    /// チャンネルごとの標本の列（チャンネルの数だけ、同じ長さ）を足す。
    pub fn push_frames(&mut self, planes: &[&[f32]]) -> Result<(), EngineError> {
        if planes.len() != self.cur.len() {
            return Err(EngineError::Internal(format!(
                "チャンネルの数が違います（{} と {}）",
                planes.len(),
                self.cur.len()
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
        let mut at = skip;
        let end = skip + keep;
        while at < end {
            self.reserve_next()?;
            let room = AUDIO_CHUNK - self.cur.first().map_or(0, Vec::len);
            let m = room.min(end - at);
            for (cur, src) in self.cur.iter_mut().zip(planes) {
                cur.try_reserve(m)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                cur.extend_from_slice(&src[at..at + m]);
            }
            at += m;
            self.frames += m as u64;
        }
        Ok(())
    }

    /// インターリーブした標本（フレームの順に全チャンネル）を足す。長さはチャンネルの数の倍数であること。
    pub fn push_interleaved(&mut self, samples: &[f32]) -> Result<(), EngineError> {
        let ch = self.cur.len();
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
    pub fn finish(mut self, meta: AudioMeta) -> Result<Audio, EngineError> {
        for (row, cur) in self.table.iter_mut().zip(self.cur) {
            if !cur.is_empty() {
                row.push(Chunk::Data(Arc::new(cur)));
            }
        }
        let frames = usize::try_from(self.frames)
            .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
        Audio::from_chunks(self.rate, frames, self.table, meta)
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

    fn value(planes: &[Vec<f32>]) -> Audio {
        Audio::new(
            8_000,
            planes.iter().cloned().map(Arc::new).collect(),
            AudioMeta::default(),
        )
        .unwrap()
    }

    /// 表は正規の区切り：チャンク k は [k·C, min((k+1)·C, frames))。1 チャンク以下のチャンネルは写さずに共有し、
    /// 長いチャンネルは C ずつに分ける。
    #[test]
    fn table_is_canonical_and_short_planes_are_shared() {
        let short = Arc::new(vec![0.25f32; 100]);
        let a = Audio::new(8_000, vec![short.clone()], AudioMeta::default()).unwrap();
        let Chunk::Data(c) = a.chunk(0, 0).unwrap() else {
            panic!()
        };
        assert!(Arc::ptr_eq(c, &short));
        assert!(Arc::ptr_eq(&a.to_planes().unwrap()[0], &short));
        let frames = 2 * AUDIO_CHUNK + 37;
        let a = value(&ramp(2, frames));
        assert_eq!(a.chunk_count(), 3);
        for k in 0..3 {
            let Chunk::Data(c) = a.chunk(1, k).unwrap() else {
                panic!()
            };
            assert_eq!(c.len(), a.chunk_range(k).len());
        }
        assert_eq!(a.chunk_range(2), 2 * AUDIO_CHUNK..frames);
        assert!(a.chunk(2, 0).is_err() && a.chunk(0, 3).is_err());
        assert_eq!(a.resident_bytes(), 2 * frames as u64 * 4);
        let empty = value(&[Vec::new()]);
        assert_eq!(empty.chunk_count(), 0);
        assert_eq!(empty.to_planes().unwrap()[0].len(), 0);
    }

    /// `from_chunks` は不変条件（チャンクの数・正規の長さ・無効の位置がチャンネルでそろう）を確かめる。
    #[test]
    fn from_chunks_checks_invariants() {
        let c = |n: usize| Chunk::Data(Arc::new(vec![0.0; n]));
        let n = AUDIO_CHUNK + 5;
        let m = AudioMeta::default;
        assert!(Audio::from_chunks(8_000, n, vec![vec![c(AUDIO_CHUNK), c(5)]], m()).is_ok());
        // 数が違う・長さが違う
        assert!(Audio::from_chunks(8_000, n, vec![vec![c(AUDIO_CHUNK)]], m()).is_err());
        assert!(Audio::from_chunks(8_000, n, vec![vec![c(AUDIO_CHUNK), c(6)]], m()).is_err());
        assert!(Audio::from_chunks(8_000, n, vec![vec![c(5), c(AUDIO_CHUNK)]], m()).is_err());
        // 無効はチャンネルでそろう
        let ok = Audio::from_chunks(
            8_000,
            n,
            vec![vec![Chunk::Hole, c(5)], vec![Chunk::Hole, c(5)]],
            m(),
        )
        .unwrap();
        assert!(!ok.is_complete());
        assert!(
            Audio::from_chunks(
                8_000,
                n,
                vec![vec![Chunk::Hole, c(5)], vec![c(AUDIO_CHUNK), c(5)]],
                m()
            )
            .is_err()
        );
        assert!(
            Audio::from_chunks(
                8_000,
                n,
                vec![vec![c(AUDIO_CHUNK), c(5)], vec![Chunk::Hole, c(5)]],
                m()
            )
            .is_err()
        );
        assert!(Audio::from_chunks(0, 0, vec![vec![]], m()).is_err());
        assert!(Audio::from_chunks(8_000, 0, vec![], m()).is_err());
    }

    /// 無効なチャンク：読むと誤りで印が立ち、有効なチャンクはそのまま読める。中身は持たない（数えない）。`keep_only` は
    /// 残したチャンクの実体を共有する。
    #[test]
    fn holes_are_refused_and_marked() {
        let frames = 3 * AUDIO_CHUNK;
        let planes = ramp(2, frames);
        let full = value(&planes);
        let mut keep = ChunkSet::empty(3);
        keep.insert(1);
        let part = full.keep_only(&keep);
        assert!(!part.is_complete() && full.is_complete());
        assert_eq!(part.valid_chunks(), keep);
        assert_eq!(part.resident_bytes(), 2 * AUDIO_CHUNK as u64 * 4);
        assert!(part.same_chunk(&full, 1, 1) && !part.same_chunk(&full, 1, 0));
        // 有効なチャンクの中は読める（印は立たない）
        let mut out = vec![0f32; 10];
        part.read(1, AUDIO_CHUNK as u64 + 3, &mut out).unwrap();
        assert_eq!(out, planes[1][AUDIO_CHUNK + 3..AUDIO_CHUNK + 13]);
        assert!(!part.holes_read());
        // 境をまたいで無効なチャンクに入ると誤り・印
        assert!(part.read(0, 2 * AUDIO_CHUNK as u64 - 2, &mut out).is_err());
        assert!(part.holes_read());
        let again = full.keep_only(&keep);
        assert!(again.chunk_data(0, 0).is_err() && again.holes_read());
        assert!(
            full.keep_only(&keep)
                .for_each_chunk(&NoMonitor, |_, _| Ok(()))
                .is_err()
        );
        // 全部を有効にすれば完全な値と同じ（印は新しい）
        let none = full.invalidate(&ChunkSet::empty(3));
        assert!(none.is_complete());
        assert_eq!(none, full);
        // 無効の位置も等しさに入る
        let mut other = ChunkSet::empty(3);
        other.insert(2);
        assert_ne!(part, full.keep_only(&other));
        assert_eq!(part, full.keep_only(&keep));
    }

    /// メタデータだけを替える写しは表を共有する（O(1)）。`Debug` は標本を出さない。
    #[test]
    fn with_meta_shares_and_debug_is_short() {
        let a = value(&ramp(2, AUDIO_CHUNK + 1));
        let meta = AudioMeta {
            loop_range: Some(crate::value::LoopRange { start: 1, end: 9 }),
            ..AudioMeta::default()
        };
        let b = a.with_meta(meta.clone());
        assert!(Arc::ptr_eq(&a.table, &b.table));
        assert_eq!(b.meta, meta);
        assert_ne!(a, b);
        let text = format!("{a:?}");
        assert!(text.len() < 400, "{text}");
        assert!(text.contains("chunks: 2"), "{text}");
    }

    /// 源の参照のチャンク：`from_chunks` は源のレート・チャンネル・長さを確かめ、別の番号の源のチャンクも長さが合えば
    /// 置ける（ずらした写しの共有）。数え（`Value::chunks`）は源 1 つにつき LRU の上限を 1 回、中身は数えない。
    #[test]
    fn source_chunks_are_checked_and_counted_once() {
        use crate::audio::source::{AudioSource, MemorySource};
        let frames = 3 * AUDIO_CHUNK;
        let base = Arc::new(value(&ramp(2, frames)));
        let src: Arc<dyn AudioSource> = Arc::new(MemorySource::new(base.clone()).unwrap());
        let lazy = Audio::from_source(src.clone(), AudioMeta::default()).unwrap();
        assert_eq!(lazy, *base);
        let mut out = Vec::new();
        lazy.chunks_into(&mut out);
        assert_eq!(out.len(), 1, "源 1 つにつき 1 回");
        assert_eq!(out[0].1, src.resident_bytes());
        assert_eq!(lazy.resident_bytes(), 0);
        let r = |ch: u16, index: u32| {
            Chunk::Source(SourceRef {
                src: src.clone(),
                ch,
                index,
            })
        };
        // 源のチャンク 1・2 を、長さ 2C の値のチャンク 0・1 に置く（ずらした写し）
        let shifted = Audio::from_chunks(
            8_000,
            2 * AUDIO_CHUNK,
            vec![vec![r(0, 1), r(0, 2)], vec![r(1, 1), r(1, 2)]],
            AudioMeta::default(),
        )
        .unwrap();
        assert_eq!(
            shifted.read_planes(0, 2 * AUDIO_CHUNK).unwrap()[1],
            ramp(2, frames)[1][AUDIO_CHUNK..]
        );
        // 長さ・チャンネル・レートが合わない源は断る
        let m = AudioMeta::default;
        assert!(
            Audio::from_chunks(8_000, AUDIO_CHUNK + 1, vec![vec![r(0, 0), r(0, 1)]], m()).is_err()
        );
        assert!(Audio::from_chunks(8_000, AUDIO_CHUNK, vec![vec![r(2, 0)]], m()).is_err());
        assert!(Audio::from_chunks(44_100, AUDIO_CHUNK, vec![vec![r(0, 0)]], m()).is_err());
        assert!(Audio::from_chunks(8_000, AUDIO_CHUNK, vec![vec![r(0, 0)]], m()).is_ok());
        // 同じ源・同じ番号どうしは共有、中身が同じでも別の番号は共有でない
        let one = Audio::from_chunks(8_000, AUDIO_CHUNK, vec![vec![r(0, 0)]], m()).unwrap();
        let again = Audio::from_chunks(8_000, AUDIO_CHUNK, vec![vec![r(0, 0)]], m()).unwrap();
        assert!(one.same_chunk(&again, 0, 0));
        assert!(!one.same_chunk(&base, 0, 0));
        assert!(format!("{lazy:?}").contains("sources: 3"));
    }

    #[test]
    fn chunk_sets() {
        let mut s = ChunkSet::empty(130);
        for k in [0, 63, 64, 129, 500] {
            s.insert(k);
        }
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![0, 63, 64, 129]);
        assert_eq!(s.count(), 4);
        let mut t = ChunkSet::all(130);
        assert_eq!(t.count(), 130);
        t.subtract(&s);
        assert_eq!(t.count(), 126);
        assert!(!t.contains(63) && t.contains(62));
        t.intersect_with(&s);
        assert!(t.is_empty());
        t.union_with(&s);
        assert_eq!(t, s);
        assert_eq!(s.universe(), 130);
    }
}
