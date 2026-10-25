//! 音声の源（M13 段 1。docs/editing.md §9.11）：音声の値の中身を、読むときにチャンク（`AUDIO_CHUNK` フレーム）ごとに
//! 作る・読むもの。値のチャンク [`Chunk::Source`](crate::value::Chunk::Source) が源を参照する（遅延の値）。
//!
//! - 源の中身は不変（ライブラリの `files/<hash>`・一度だけ書く作業用のファイル・アンカー）。同じ番号にはいつも同じ値を返す。
//!   読んだチャンクの写しは源の中の LRU（[`ChunkLru`]）に置く。LRU は値の中身ではないので、値の不変・等しさ・ダイジェストは
//!   変わらない。
//! - エンジンはファイルを開かない（OS・DB に依らない約束）。ファイルの源（[`PcmFileSource`]）は、呼び手（kura-core）が渡す
//!   開き方の閉包（[`Opener`]）で読み口を作る。開いた読み口は使い回す。
//! - 予算には LRU の上限（[`AudioSource::resident_bytes`]）が入る（`Value::chunks` の擬似のチャンク。docs/editing.md §12.10）。

use std::collections::{BTreeMap, BTreeSet};
use std::io::SeekFrom;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::codec::ReadSeek;
use crate::codec::pcm::PcmLayout;
use crate::error::EngineError;
use crate::value::{AUDIO_CHUNK, Audio, Chunk};

/// 音声の源。
pub trait AudioSource: Send + Sync {
    /// サンプルレート。
    fn rate(&self) -> u32;
    /// チャンネル数（1〜65,535）。
    fn channels(&self) -> usize;
    /// 1 チャンネルあたりのフレーム数。
    fn frames(&self) -> u64;
    /// チャンク `index`（[index·C, min((index+1)·C, frames))）の全チャンネル（チャンネルの順。長さは正規の長さ）。
    /// 同じ番号にはいつも同じ値を返す。読めなければ `Io` など。
    fn read_chunk(&self, index: u32) -> Result<Vec<Arc<Vec<f32>>>, EngineError>;
    /// 予算で数える常駐の上限（LRU の上限。持たない源は 0）。
    fn resident_bytes(&self) -> u64;
    /// 読んだ数・当たりの数・読んだバイト数（テストで「全体を読んでいない」ことを確かめる）。
    fn stats(&self) -> SourceStats {
        SourceStats::default()
    }
}

/// 源の統計。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceStats {
    /// 下の読み口から読んだ（作った）チャンクの数（LRU に当たらなかったもの）。
    pub reads: u64,
    /// LRU に当たった数。
    pub hits: u64,
    /// 下の読み口から読んだバイト数。
    pub bytes_read: u64,
}

/// 長さ `frames` の音声のチャンク `index` の区間（範囲の外は None）。
pub fn chunk_span(frames: u64, index: u32) -> Option<(u64, u64)> {
    let start = u64::from(index).checked_mul(AUDIO_CHUNK as u64)?;
    (start < frames).then(|| (start, (start + AUDIO_CHUNK as u64).min(frames)))
}

fn chunk_bytes(chunks: &[Arc<Vec<f32>>]) -> u64 {
    chunks
        .iter()
        .map(|c| (c.len() as u64).saturating_mul(4))
        .fold(0u64, u64::saturating_add)
}

/// 読んだチャンクの写しの LRU（上限はバイト）。時刻を使わず、使った順の番号で追い出す（`NodeCache` と同じ）。
/// 鍵は `Mutex` で守り、読む間は鍵を外す。[`ChunkLru::get_or_load`] は同じ番号を同時に読もうとしたら、先に読み始めたものを
/// 待って使う（同じチャンクを二度読まない。M13 段 1。チャンネルごと・隣のチャンクの仕事が同じ源のチャンクを読むため）。
pub struct ChunkLru {
    cap: u64,
    inner: Mutex<LruInner>,
    /// 読み終えた知らせ（読んでいる番号が減ったとき）。
    ready: Condvar,
}

/// チャンク 1 つの全チャンネル（チャンネルの順）。
pub type ChunkPlanes = Vec<Arc<Vec<f32>>>;

/// LRU の項目：（使った順の番号, 全チャンネル, バイト数）。
type LruEntry = (u64, Vec<Arc<Vec<f32>>>, u64);

#[derive(Default)]
struct LruInner {
    tick: u64,
    bytes: u64,
    /// 番号 → 項目。
    map: BTreeMap<u32, LruEntry>,
    /// 使った順の番号 → 番号。
    order: BTreeMap<u64, u32>,
    /// 今読んでいる番号（`get_or_load`）。
    pending: BTreeSet<u32>,
}

/// 読んでいる印を、読み終えたら（失敗・中断しても）外して知らせる。
struct Loading<'a> {
    lru: &'a ChunkLru,
    index: u32,
}

impl Drop for Loading<'_> {
    fn drop(&mut self) {
        self.lru.lock().pending.remove(&self.index);
        self.lru.ready.notify_all();
    }
}

impl ChunkLru {
    /// 上限 `cap` バイトの LRU。
    pub fn new(cap: u64) -> Self {
        Self {
            cap,
            inner: Mutex::new(LruInner::default()),
            ready: Condvar::new(),
        }
    }

    /// 上限（バイト）。
    pub fn capacity(&self) -> u64 {
        self.cap
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LruInner> {
        // 鍵を持ったまま panic しても中身は壊れない（数えの途中で止まらない作り）ので、毒は無視する
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 今持っているバイト数（上限を超えない）。
    pub fn resident(&self) -> u64 {
        self.lock().bytes
    }

    /// チャンク `index` の写し（あれば。使った順を新しくする）。
    pub fn get(&self, index: u32) -> Option<Vec<Arc<Vec<f32>>>> {
        Self::touch(&mut self.lock(), index)
    }

    fn touch(g: &mut LruInner, index: u32) -> Option<Vec<Arc<Vec<f32>>>> {
        g.tick += 1;
        let tick = g.tick;
        let LruInner { map, order, .. } = g;
        let entry = map.get_mut(&index)?;
        order.remove(&entry.0);
        entry.0 = tick;
        order.insert(tick, index);
        Some(entry.1.clone())
    }

    /// チャンク `index` の写しを返す（なければ `load` で読んで入れる）。同じ番号をほかが読んでいれば、読み終えるのを待って
    /// 使う（読み終えた値が LRU に入らなかったときは自分で読む）。返すのは（全チャンネル, LRU に当たったか）。待つのは
    /// 同じ番号を読むほかの仕事だけで、読む閉包は描画の仕事に依らないので詰まらない。
    pub fn get_or_load(
        &self,
        index: u32,
        load: impl FnOnce() -> Result<ChunkPlanes, EngineError>,
    ) -> Result<(ChunkPlanes, bool), EngineError> {
        let mut g = self.lock();
        loop {
            if let Some(hit) = Self::touch(&mut g, index) {
                return Ok((hit, true));
            }
            if !g.pending.contains(&index) {
                break;
            }
            g = self.ready.wait(g).unwrap_or_else(|p| p.into_inner());
        }
        g.pending.insert(index);
        drop(g);
        let loading = Loading { lru: self, index };
        let chunks = load()?;
        self.insert(index, chunks.clone());
        drop(loading);
        Ok((chunks, false))
    }

    /// チャンク `index` の写しを入れる（上限を超えるなら古いものから追い出す。1 つで上限を超えるものは入れない）。
    pub fn insert(&self, index: u32, chunks: Vec<Arc<Vec<f32>>>) {
        let bytes = chunk_bytes(&chunks);
        if bytes > self.cap {
            return;
        }
        let mut g = self.lock();
        g.tick += 1;
        let tick = g.tick;
        if let Some((old_tick, _, old_bytes)) = g.map.remove(&index) {
            g.order.remove(&old_tick);
            g.bytes -= old_bytes;
        }
        while g.bytes + bytes > self.cap {
            let Some((&t, &i)) = g.order.iter().next() else {
                break;
            };
            g.order.remove(&t);
            if let Some((_, _, b)) = g.map.remove(&i) {
                g.bytes -= b;
            }
        }
        g.map.insert(index, (tick, chunks, bytes));
        g.order.insert(tick, index);
        g.bytes += bytes;
    }
}

/// 源の読み口を開く閉包（呼び手が渡す。エンジンはパスを知らない）。
pub type Opener = Arc<dyn Fn() -> Result<Box<dyn ReadSeek + Send>, EngineError> + Send + Sync>;

/// 統計の数え手。
#[derive(Default)]
struct Counters {
    reads: AtomicU64,
    hits: AtomicU64,
    bytes_read: AtomicU64,
}

impl Counters {
    fn get(&self) -> SourceStats {
        SourceStats {
            reads: self.reads.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
        }
    }
}

/// 直接シークして読むファイルの源（WAV・AIFF・アンカーの中の f32 WAV）。標本の変換は `decode` と同じ関数
/// （[`PcmLayout::decode_frames`]）を通るので、値は `decode` と同じ。
pub struct PcmFileSource {
    rate: u32,
    layout: PcmLayout,
    opener: Opener,
    /// 開いた読み口（使い回す）。
    pool: Mutex<Vec<Box<dyn ReadSeek + Send>>>,
    lru: ChunkLru,
    counters: Counters,
}

impl PcmFileSource {
    /// 源を作る（読み口はまだ開かない）。`lru_bytes` は LRU の上限。
    pub fn new(rate: u32, layout: PcmLayout, opener: Opener, lru_bytes: u64) -> Self {
        Self {
            rate,
            layout,
            opener,
            pool: Mutex::new(Vec::new()),
            lru: ChunkLru::new(lru_bytes),
            counters: Counters::default(),
        }
    }

    /// 開いた読み口を預ける（ヘッダを読むのに開いたものを使い回す）。
    pub fn give(&self, src: Box<dyn ReadSeek + Send>) {
        self.pool
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(src);
    }

    fn take(&self) -> Result<Box<dyn ReadSeek + Send>, EngineError> {
        let pooled = self.pool.lock().unwrap_or_else(|p| p.into_inner()).pop();
        match pooled {
            Some(s) => Ok(s),
            None => (self.opener)(),
        }
    }
}

impl AudioSource for PcmFileSource {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> usize {
        usize::from(self.layout.channels)
    }

    fn frames(&self) -> u64 {
        self.layout.frames
    }

    fn read_chunk(&self, index: u32) -> Result<Vec<Arc<Vec<f32>>>, EngineError> {
        let (chunks, hit) = self.lru.get_or_load(index, || self.load(index))?;
        if hit {
            self.counters.hits.fetch_add(1, Ordering::Relaxed);
        }
        Ok(chunks)
    }

    fn resident_bytes(&self) -> u64 {
        self.lru.capacity()
    }

    fn stats(&self) -> SourceStats {
        self.counters.get()
    }
}

impl PcmFileSource {
    /// チャンク `index` を下の読み口から読む（1 回の読みで全チャンネル）。
    fn load(&self, index: u32) -> Result<Vec<Arc<Vec<f32>>>, EngineError> {
        let (start, end) = chunk_span(self.layout.frames, index).ok_or_else(|| {
            EngineError::Internal(format!("音声の源のチャンク {index} は範囲の外です"))
        })?;
        let block = self.layout.block() as u64;
        let n = usize::try_from(end - start)
            .map_err(|_| EngineError::Limit("音声のチャンクが大きすぎます".into()))?;
        let len = usize::try_from((end - start).saturating_mul(block))
            .map_err(|_| EngineError::Limit("音声のチャンクが大きすぎます".into()))?;
        let at = start
            .checked_mul(block)
            .and_then(|o| o.checked_add(self.layout.data_offset))
            .ok_or_else(|| EngineError::Limit("音声の源の位置があふれます".into()))?;
        let mut bytes = crate::codec::try_vec(len, 0u8)?;
        let mut src = self.take()?;
        src.seek(SeekFrom::Start(at))?;
        src.read_exact(&mut bytes)?;
        self.give(src);
        let mut planes: Vec<Vec<f32>> = Vec::with_capacity(self.channels());
        for _ in 0..self.channels() {
            let mut p = Vec::new();
            p.try_reserve_exact(n)
                .map_err(|_| EngineError::Limit("音声のチャンクを確保できません".into()))?;
            planes.push(p);
        }
        self.layout.decode_frames(&bytes, &mut planes)?;
        let chunks: Vec<Arc<Vec<f32>>> = planes.into_iter().map(Arc::new).collect();
        self.counters.reads.fetch_add(1, Ordering::Relaxed);
        self.counters
            .bytes_read
            .fetch_add(len as u64, Ordering::Relaxed);
        Ok(chunks)
    }
}

/// 値を包んだ源（テストで遅延の入力を作る。中身の実体を写さずに返す）。
pub struct MemorySource {
    audio: Arc<Audio>,
    counters: Counters,
}

impl MemorySource {
    /// 完全な値（無効なチャンクのない値）を包む。
    pub fn new(audio: Arc<Audio>) -> Result<Self, EngineError> {
        if !audio.is_complete() {
            return Err(EngineError::Internal(
                "範囲だけを描いた値は源にできません".into(),
            ));
        }
        Ok(Self {
            audio,
            counters: Counters::default(),
        })
    }
}

impl AudioSource for MemorySource {
    fn rate(&self) -> u32 {
        self.audio.rate()
    }

    fn channels(&self) -> usize {
        self.audio.channels()
    }

    fn frames(&self) -> u64 {
        self.audio.frames() as u64
    }

    fn read_chunk(&self, index: u32) -> Result<Vec<Arc<Vec<f32>>>, EngineError> {
        let k = index as usize;
        let mut out = Vec::with_capacity(self.audio.channels());
        for ch in 0..self.audio.channels() {
            out.push(match self.audio.chunk(ch, k)? {
                Chunk::Data(v) => v.clone(),
                Chunk::Source(r) => r
                    .src
                    .read_chunk(r.index)?
                    .get(usize::from(r.ch))
                    .cloned()
                    .ok_or_else(|| {
                        EngineError::Internal("音声の源のチャンネルが足りません".into())
                    })?,
                Chunk::Hole => {
                    return Err(EngineError::Internal(
                        "範囲だけを描いた値は源にできません".into(),
                    ));
                }
            });
        }
        self.counters.reads.fetch_add(1, Ordering::Relaxed);
        Ok(out)
    }

    fn resident_bytes(&self) -> u64 {
        self.audio.resident_bytes()
    }

    fn stats(&self) -> SourceStats {
        self.counters.get()
    }
}
