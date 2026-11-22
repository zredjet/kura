//! 音声の源（M13 段 1。docs/editing.md §9.11）：音声の値の中身を、読むときにチャンク（`AUDIO_CHUNK` フレーム）ごとに
//! 作る・読むもの。値のチャンク [`Chunk::Source`](crate::value::Chunk::Source) が源を参照する（遅延の値）。
//!
//! - 源の中身は不変（ライブラリの `files/<hash>`・一度だけ書く作業用のファイル・アンカー）。同じ番号にはいつも同じ値を返す。
//!   読んだチャンクの写しは源の中の LRU（[`ChunkLru`]）に置く。LRU は値の中身ではないので、値の不変・等しさ・ダイジェストは
//!   変わらない。
//! - エンジンはファイルを開かない（OS・DB に依らない約束）。ファイルの源（[`PcmFileSource`]）は、呼び手（kura-core）が渡す
//!   開き方の閉包（[`Opener`]）で読み口を作る。開いた読み口は使い回す。
//! - 予算には LRU の上限（[`AudioSource::resident_bytes`]）が入る（`Value::chunks` の擬似のチャンク。docs/editing.md §12.10）。
//! - 読み方（M13 段 7。[`ReadMode`]）：手・表示・PCM の読みは LRU に入れる（[`ReadMode::Keep`]。既定）。流して読む読み
//!   （エディタの裏の仕事・計りの窓。呼び手が [`read_through`] で包む）は、LRU にあれば使い（使った順は変えない）、なければ
//!   読むが LRU に入れず、上限の内の小さな流しの段に置く（[`ReadMode::Through`]。読み通し）。全体を流しても、見えている
//!   区間のチャンクを LRU から追い出さない。流しの段は窓の中の同じチャンクの読み（チャンネルごと・隣のチャンクの核）を
//!   1 回にまとめる。読み方は読むスレッドの印で、`determinism::par_blocks` が仕事に引き継ぐ。値には効かない（どちらも
//!   同じ中身を返す）。
//! - 上限に音声の全体が収まる源は段を分けない（[`ChunkLru::for_source`]）：どのチャンクも読み方によらず LRU に入り、何も
//!   追い出さないので、2 回目からは読まない（段 7 の検証の直し）。

use std::cell::Cell;
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
    /// 読み通し（[`ReadMode::Through`]）で読み、LRU に入れなかったチャンクの数（`reads` の内）。
    pub streamed: u64,
}

/// チャンクの読み方（モジュールの説明。値に効かない）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadMode {
    /// LRU に入れる（手・表示・PCM の読み。既定）。
    #[default]
    Keep,
    /// 読み通し：LRU にあれば使い（使った順は変えない）、なければ読んで流しの段に置く（LRU に入れない）。
    Through,
}

thread_local! {
    /// このスレッドの読み方（[`with_read_mode`] が決め、`par_blocks` が仕事に引き継ぐ）。
    static READ_MODE: Cell<ReadMode> = const { Cell::new(ReadMode::Keep) };
}

/// このスレッドの今の読み方。
pub fn read_mode() -> ReadMode {
    READ_MODE.with(Cell::get)
}

/// 読み方を元に戻す印（panic でも戻す）。
struct RestoreMode(ReadMode);

impl Drop for RestoreMode {
    fn drop(&mut self) {
        READ_MODE.with(|m| m.set(self.0));
    }
}

/// 読み方を `mode` にして `f` を呼び、終わったら（panic でも）元に戻す。`determinism::par_blocks` の仕事はこの読み方を
/// 引き継ぐ。呼び手のプールの入口（kura-core の `EngineRuntime`）は `Keep` で包み、ほかの仕事の読み方を持ち込まない。
pub fn with_read_mode<R>(mode: ReadMode, f: impl FnOnce() -> R) -> R {
    let _restore = RestoreMode(READ_MODE.with(|m| m.replace(mode)));
    f()
}

/// 流して読む読み（エディタの裏の仕事・計りの窓）で `f` を呼ぶ（[`ReadMode::Through`]）。
pub fn read_through<R>(f: impl FnOnce() -> R) -> R {
    with_read_mode(ReadMode::Through, f)
}

/// 流しの段に置けるチャンクの数（流れの窓 `STREAM_CHUNKS` ＋ 両隣の 1 チャンクずつ）。
pub const STREAM_TIER_CHUNKS: u64 = crate::audio::stream::STREAM_CHUNKS as u64 + 2;

/// 手の読みが一度に読むチャンクの数の上限（エディタの表示の区間。kura-core の `MAX_VIEW_CHUNKS`）。上限にこの分が収まる LRU は、
/// 主の段にも収める（[`stream_tier_bytes`]）。手のたびに区間を読み直さないため。
pub const KEPT_VIEW_CHUNKS: u64 = 48;

/// 上限 `cap` の LRU に `channels` チャンネルのチャンクを置くときの流しの段の上限（`cap` の内）：
/// min(`cap` / 4, [`STREAM_TIER_CHUNKS`] × チャンネル × 256 KiB) を、[`KEPT_VIEW_CHUNKS`] が `cap` に収まるなら主の段に収まる
/// 大きさ（`cap` − その分）で頭打ちにしたもの。チャンク 1 つ（全チャンネル）も置けなければ 0（流しの段を持たない。読み通しは
/// LRU に入れずに読むだけ）。
pub fn stream_tier_bytes(cap: u64, channels: usize) -> u64 {
    let one = (AUDIO_CHUNK as u64)
        .saturating_mul(4)
        .saturating_mul(channels.max(1) as u64);
    let mut tier = (cap / 4).min(STREAM_TIER_CHUNKS.saturating_mul(one));
    if let Some(spare) = cap.checked_sub(KEPT_VIEW_CHUNKS.saturating_mul(one)) {
        tier = tier.min(spare);
    }
    if tier < one { 0 } else { tier }
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
///
/// 上限 `cap` の内を、手・表示の読み（[`ReadMode::Keep`]）の主の段（`cap` − 流しの段の上限）と、読み通し
/// （[`ReadMode::Through`]）の流しの段に分ける（M13 段 7。[`ChunkLru::for_channels`]。`new` は流しの段を持たない）。読み通しは
/// 主の段を変えない（当たっても使った順を変えず、読んだものは流しの段に置く）。流しの段のチャンクを手が読めば主の段へ移す。
/// 上限に音声の全体が収まる源（[`ChunkLru::for_source`]）は段を分けない。
pub struct ChunkLru {
    cap: u64,
    /// 流しの段の上限（`cap` の内）。
    stream_cap: u64,
    /// 読み通しで読んだものも主の段に入れる（上限に全体が収まり、何も追い出さない）。
    keep_all: bool,
    inner: Mutex<LruInner>,
    /// 読み終えた知らせ（読んでいる番号が減ったとき）。
    ready: Condvar,
    /// 読み通しで読んだ（主の段に入れなかった）チャンクの数。
    streamed: AtomicU64,
}

/// チャンク 1 つの全チャンネル（チャンネルの順）。
pub type ChunkPlanes = Vec<Arc<Vec<f32>>>;

/// LRU の項目：（使った順の番号, 全チャンネル, バイト数）。
type LruEntry = (u64, Vec<Arc<Vec<f32>>>, u64);

/// 段 1 つ（主の段・流しの段）。
#[derive(Default)]
struct Tier {
    bytes: u64,
    /// 番号 → 項目。
    map: BTreeMap<u32, LruEntry>,
    /// 使った順の番号 → 番号。
    order: BTreeMap<u64, u32>,
}

impl Tier {
    /// 番号 `index` の写し（あれば。使った順を `tick` にする）。
    fn touch(&mut self, index: u32, tick: u64) -> Option<ChunkPlanes> {
        let entry = self.map.get_mut(&index)?;
        self.order.remove(&entry.0);
        entry.0 = tick;
        self.order.insert(tick, index);
        Some(entry.1.clone())
    }

    /// 番号 `index` の写し（あれば。使った順は変えない）。
    fn peek(&self, index: u32) -> Option<ChunkPlanes> {
        self.map.get(&index).map(|e| e.1.clone())
    }

    /// 番号 `index` を取り出す（全チャンネルとバイト数）。
    fn take(&mut self, index: u32) -> Option<(ChunkPlanes, u64)> {
        let (tick, planes, bytes) = self.map.remove(&index)?;
        self.order.remove(&tick);
        self.bytes -= bytes;
        Some((planes, bytes))
    }

    /// 入れる（上限 `cap` を超えるなら古いものから追い出す。1 つで上限を超えるものは入れない）。
    fn put(&mut self, index: u32, planes: ChunkPlanes, bytes: u64, tick: u64, cap: u64) {
        if bytes > cap {
            return;
        }
        self.take(index);
        while self.bytes + bytes > cap {
            let Some((&t, &i)) = self.order.iter().next() else {
                break;
            };
            self.order.remove(&t);
            if let Some((_, _, b)) = self.map.remove(&i) {
                self.bytes -= b;
            }
        }
        self.map.insert(index, (tick, planes, bytes));
        self.order.insert(tick, index);
        self.bytes += bytes;
    }

    /// 番号を使った順（古い順）に。
    fn keys(&self) -> Vec<u32> {
        self.order.values().copied().collect()
    }
}

#[derive(Default)]
struct LruInner {
    tick: u64,
    /// 主の段（手・表示の読み）。
    main: Tier,
    /// 流しの段（読み通しの読み）。
    stream: Tier,
    /// 今読んでいる番号（`get_or_load`）。
    pending: BTreeSet<u32>,
}

impl LruInner {
    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }
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
    /// 上限 `cap` バイトの LRU（流しの段を持たない）。
    pub fn new(cap: u64) -> Self {
        Self::with_stream(cap, 0)
    }

    /// 上限 `cap` バイトの LRU のうち、`channels` チャンネルの読み通しに [`stream_tier_bytes`] を流しの段にするもの（源が使う）。
    pub fn for_channels(cap: u64, channels: usize) -> Self {
        Self::with_stream(cap, stream_tier_bytes(cap, channels))
    }

    /// `channels` チャンネル・`frames` フレームの音声の源の LRU（上限 `cap` バイト）。上限に全体が収まるなら段を分けず、どの
    /// チャンクも読み方によらず主の段に入れる（何も追い出さないので、読み通しでも見えている区間は残り、2 回目からは読まない）。
    /// 収まらなければ [`ChunkLru::for_channels`]。
    pub fn for_source(cap: u64, channels: usize, frames: u64) -> Self {
        let whole = frames
            .saturating_mul(channels.max(1) as u64)
            .saturating_mul(4);
        if cap >= whole {
            Self {
                keep_all: true,
                ..Self::new(cap)
            }
        } else {
            Self::for_channels(cap, channels)
        }
    }

    /// 上限 `cap` バイトのうち `stream_cap`（`cap` で頭打ち）を流しの段にした LRU。
    pub fn with_stream(cap: u64, stream_cap: u64) -> Self {
        Self {
            cap,
            stream_cap: stream_cap.min(cap),
            keep_all: false,
            inner: Mutex::new(LruInner::default()),
            ready: Condvar::new(),
            streamed: AtomicU64::new(0),
        }
    }

    /// 上限（バイト。主の段と流しの段の合計）。
    pub fn capacity(&self) -> u64 {
        self.cap
    }

    /// 流しの段の上限（バイト）。
    pub fn stream_capacity(&self) -> u64 {
        self.stream_cap
    }

    /// 主の段の上限（バイト）。
    fn main_cap(&self) -> u64 {
        self.cap - self.stream_cap
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LruInner> {
        // 鍵を持ったまま panic しても中身は壊れない（数えの途中で止まらない作り）ので、毒は無視する
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 今持っているバイト数（主の段と流しの段の合計。上限を超えない）。
    pub fn resident(&self) -> u64 {
        let g = self.lock();
        g.main.bytes + g.stream.bytes
    }

    /// 主の段の番号（使った順。古いものから）。
    pub fn kept(&self) -> Vec<u32> {
        self.lock().main.keys()
    }

    /// 流しの段の番号（使った順。古いものから）。
    pub fn streaming(&self) -> Vec<u32> {
        self.lock().stream.keys()
    }

    /// 読み通しで読んだ（主の段に入れなかった）チャンクの数。
    pub fn streamed(&self) -> u64 {
        self.streamed.load(Ordering::Relaxed)
    }

    /// チャンク `index` の写し（あれば。このスレッドの読み方で引く。[`ChunkLru::get_or_load`] の当たりと同じ）。
    pub fn get(&self, index: u32) -> Option<Vec<Arc<Vec<f32>>>> {
        let mode = read_mode();
        self.hit(&mut self.lock(), index, mode)
    }

    /// 当たり：手の読みは主の段（使った順を新しくする）、流しの段にあれば主の段へ移す。読み通しは主の段を見て（使った順は
    /// 変えない）、なければ流しの段。
    fn hit(&self, g: &mut LruInner, index: u32, mode: ReadMode) -> Option<ChunkPlanes> {
        let tick = g.next_tick();
        match mode {
            ReadMode::Keep => {
                if let Some(hit) = g.main.touch(index, tick) {
                    return Some(hit);
                }
                let (planes, bytes) = g.stream.take(index)?;
                let cap = self.main_cap();
                g.main.put(index, planes.clone(), bytes, tick, cap);
                Some(planes)
            }
            ReadMode::Through => g.main.peek(index).or_else(|| g.stream.touch(index, tick)),
        }
    }

    /// チャンク `index` の写しを返す（なければ `load` で読んで入れる。入れる段はこのスレッドの読み方で決まる：手の読みは
    /// 主の段、読み通しは流しの段）。同じ番号をほかが読んでいれば、読み終えるのを待って使う（読み終えた値が LRU に
    /// 入らなかったときは自分で読む）。返すのは（全チャンネル, LRU に当たったか）。待つのは同じ番号を読むほかの仕事だけで、
    /// 読む閉包は描画の仕事に依らないので詰まらない。
    pub fn get_or_load(
        &self,
        index: u32,
        load: impl FnOnce() -> Result<ChunkPlanes, EngineError>,
    ) -> Result<(ChunkPlanes, bool), EngineError> {
        let mode = read_mode();
        let mut g = self.lock();
        loop {
            if let Some(hit) = self.hit(&mut g, index, mode) {
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
        match mode {
            ReadMode::Through if !self.keep_all => {
                self.streamed.fetch_add(1, Ordering::Relaxed);
                let bytes = chunk_bytes(&chunks);
                let mut g = self.lock();
                let tick = g.next_tick();
                g.stream
                    .put(index, chunks.clone(), bytes, tick, self.stream_cap);
            }
            _ => self.insert(index, chunks.clone()),
        }
        drop(loading);
        Ok((chunks, false))
    }

    /// チャンク `index` の写しを主の段に入れる（上限を超えるなら古いものから追い出す。1 つで上限を超えるものは入れない）。
    pub fn insert(&self, index: u32, chunks: Vec<Arc<Vec<f32>>>) {
        let bytes = chunk_bytes(&chunks);
        let cap = self.main_cap();
        let mut g = self.lock();
        g.stream.take(index);
        let tick = g.next_tick();
        g.main.put(index, chunks, bytes, tick, cap);
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
            streamed: 0,
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
    /// 源を作る（読み口はまだ開かない）。`lru_bytes` は LRU の上限（流しの段を含む。[`ChunkLru::for_source`]）。
    pub fn new(rate: u32, layout: PcmLayout, opener: Opener, lru_bytes: u64) -> Self {
        let channels = usize::from(layout.channels);
        let lru = ChunkLru::for_source(lru_bytes, channels, layout.frames);
        Self {
            rate,
            layout,
            opener,
            pool: Mutex::new(Vec::new()),
            lru,
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
        SourceStats {
            streamed: self.lru.streamed(),
            ..self.counters.get()
        }
    }
}

impl PcmFileSource {
    /// 読んだチャンクの LRU（テストと測るため）。
    pub fn lru(&self) -> &ChunkLru {
        &self.lru
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// 1,000 フレーム・ステレオのチャンク（中身は `v`）。
    fn planes(v: f32) -> ChunkPlanes {
        vec![Arc::new(vec![v; 1_000]), Arc::new(vec![v; 1_000])]
    }

    const ONE: u64 = 2 * 4 * 1_000;

    fn load(lru: &ChunkLru, k: u32) -> (f32, bool) {
        let (p, hit) = lru.get_or_load(k, || Ok(planes(k as f32))).unwrap();
        (p[0][0], hit)
    }

    fn hit_only(lru: &ChunkLru, k: u32) -> f32 {
        let (p, hit) = lru
            .get_or_load(k, || panic!("{k} は読まずに当たるはず"))
            .unwrap();
        assert!(hit);
        p[1][0]
    }

    /// 読み通しは主の段を変えない：当たっても使った順を変えず、読んだチャンクは流しの段（上限の内）に置き、流しの段に当たれば
    /// 読まない。手の読みが流しの段のチャンクを読めば、読まずに主の段へ移す（主の段の古いものを追い出す）。
    #[test]
    fn read_through_leaves_the_kept_chunks_alone() {
        let lru = ChunkLru::with_stream(6 * ONE, 2 * ONE);
        assert_eq!(lru.capacity(), 6 * ONE);
        assert_eq!(lru.stream_capacity(), 2 * ONE);
        for k in 0..4 {
            assert_eq!(load(&lru, k), (k as f32, false));
        }
        assert_eq!(lru.kept(), vec![0, 1, 2, 3]);
        assert_eq!(read_mode(), ReadMode::Keep);
        read_through(|| {
            assert_eq!(read_mode(), ReadMode::Through);
            assert_eq!(hit_only(&lru, 1), 1.0, "主の段に当たる");
            for k in 10..16 {
                assert_eq!(load(&lru, k), (k as f32, false));
                assert!(lru.resident() <= lru.capacity());
            }
            assert_eq!(hit_only(&lru, 15), 15.0, "流しの段に当たる");
            assert_eq!(lru.get(14).map(|p| p[0][0]), Some(14.0));
            assert!(lru.get(10).is_none(), "流しの段の古いものは追い出した");
        });
        assert_eq!(read_mode(), ReadMode::Keep);
        assert_eq!(lru.kept(), vec![0, 1, 2, 3], "主の段の中身も順も同じ");
        assert_eq!(lru.streaming(), vec![15, 14], "流しの段の中は使った順");
        assert_eq!(lru.streamed(), 6);
        assert_eq!(lru.resident(), 6 * ONE);
        // 手の読み：流しの段のチャンクを主の段へ移す（読まない）
        assert_eq!(hit_only(&lru, 15), 15.0);
        assert_eq!(lru.kept(), vec![1, 2, 3, 15]);
        assert_eq!(lru.streaming(), vec![14]);
        // 手の読みで入れれば主の段（流しの段の同じ番号は外す）
        lru.insert(14, planes(14.0));
        assert_eq!(lru.kept(), vec![2, 3, 15, 14]);
        assert!(lru.streaming().is_empty());
        assert_eq!(lru.streamed(), 6);
    }

    /// 流しの段を持たない LRU（`new`）の読み通しは、LRU に入れずに読むだけ。中断（panic）しても読み方は戻る。
    #[test]
    fn read_through_without_a_stream_tier_reads_every_time() {
        let lru = ChunkLru::new(4 * ONE);
        load(&lru, 0);
        read_through(|| {
            assert_eq!(load(&lru, 5), (5.0, false));
            assert_eq!(load(&lru, 5), (5.0, false));
            assert_eq!(hit_only(&lru, 0), 0.0);
        });
        assert_eq!(lru.kept(), vec![0]);
        assert_eq!(lru.resident(), ONE);
        assert_eq!(lru.streamed(), 2);
        let r = std::panic::catch_unwind(|| read_through(|| panic!("中断")));
        assert!(r.is_err());
        assert_eq!(read_mode(), ReadMode::Keep, "panic でも戻る");
        assert_eq!(
            with_read_mode(ReadMode::Through, || with_read_mode(
                ReadMode::Keep,
                read_mode
            )),
            ReadMode::Keep
        );
        assert_eq!(read_mode(), ReadMode::Keep);
    }

    /// 流しの段の上限：上限の 1/4 と、流れの窓 ＋ 2 チャンク（全チャンネル）の小さいほう。チャンク 1 つも置けなければ 0。
    #[test]
    fn stream_tier_is_a_quarter_up_to_a_window() {
        let one = |ch: u64| AUDIO_CHUNK as u64 * 4 * ch;
        assert_eq!(STREAM_TIER_CHUNKS, 18);
        assert_eq!(stream_tier_bytes(64 << 20, 2), 18 * one(2));
        assert_eq!(stream_tier_bytes(64 << 20, 8), 16 << 20);
        assert_eq!(stream_tier_bytes(8 << 20, 1), 2 << 20);
        assert_eq!(stream_tier_bytes(4 * one(2), 2), one(2));
        assert_eq!(stream_tier_bytes(4 * one(2) - 1, 2), 0);
        assert_eq!(stream_tier_bytes(0, 2), 0);
        let lru = ChunkLru::for_channels(64 << 20, 2);
        assert_eq!(lru.capacity(), 64 << 20);
        assert_eq!(lru.stream_capacity(), 18 * one(2));
        // 区間（48 チャンク）が上限に収まるなら、主の段にも収める（5 チャンネルの 64 MiB は区間 60 MiB）
        assert_eq!(stream_tier_bytes(64 << 20, 5), 4 << 20);
        assert_eq!(stream_tier_bytes(48 * one(5), 5), 0);
        assert_eq!(stream_tier_bytes(48 * one(5) - 1, 5), (48 * one(5) - 1) / 4);
        assert_eq!(stream_tier_bytes(64 * one(5), 5), 16 * one(5));
        for ch in 1..=64u64 {
            for cap in [1u64, 4, 16, 47, 48, 49, 63, 64, 65, 66, 72, 100] {
                let cap = cap * one(ch);
                let tier = stream_tier_bytes(cap, ch as usize);
                assert!(tier * 4 <= cap && tier <= 18 * one(ch), "{ch} {cap}");
                if cap >= 48 * one(ch) {
                    assert!(cap - tier >= 48 * one(ch), "{ch} チャンネル・{cap}");
                }
            }
        }
    }

    /// 上限に全体が収まる源は段を分けない：どのチャンクも読み方によらず主の段に入り（最後の半端なチャンクも）、2 回目からは
    /// 読まない。読み通しの当たりは使った順を変えない。全体に 1 バイト足りなければ段を分ける。
    #[test]
    fn a_source_that_fits_the_lru_keeps_every_chunk() {
        let c = AUDIO_CHUNK;
        let tail = 17usize;
        let chunks = 6u32;
        let frames = (u64::from(chunks) - 1) * c as u64 + tail as u64;
        let whole = frames * 2 * 4;
        let body = Arc::new(vec![0f32; c]);
        let short = Arc::new(vec![0f32; tail]);
        let of = |k: u32| -> ChunkPlanes {
            let p = if k + 1 == chunks { &short } else { &body };
            vec![p.clone(), p.clone()]
        };
        let lru = ChunkLru::for_source(whole, 2, frames);
        assert_eq!(lru.capacity(), whole);
        assert_eq!(lru.stream_capacity(), 0);
        for k in 0..chunks {
            let (_, hit) = lru.get_or_load(k, || Ok(of(k))).unwrap();
            assert!(!hit);
        }
        assert_eq!(lru.resident(), whole);
        assert_eq!(lru.kept(), (0..chunks).collect::<Vec<_>>());
        for k in 0..chunks {
            lru.get_or_load(k, || panic!("{k} は読まずに当たるはず"))
                .unwrap();
        }
        // 読み通し：読んだものも主の段に入り（流しの段に置かない）、当たりは使った順を変えない
        let lru = ChunkLru::for_source(whole, 2, frames);
        read_through(|| {
            for k in 0..chunks {
                lru.get_or_load(k, || Ok(of(k))).unwrap();
            }
            let (_, hit) = lru.get_or_load(0, || panic!("当たるはず")).unwrap();
            assert!(hit);
        });
        assert_eq!(lru.kept(), (0..chunks).collect::<Vec<_>>());
        assert!(lru.streaming().is_empty());
        assert_eq!(lru.streamed(), 0);
        for k in 0..chunks {
            lru.get_or_load(k, || panic!("{k} は読まずに当たるはず"))
                .unwrap();
        }
        // 収まらなければ段を分ける
        let lru = ChunkLru::for_source(whole - 1, 2, frames);
        assert_eq!(lru.stream_capacity(), stream_tier_bytes(whole - 1, 2));
        assert!(lru.stream_capacity() > 0);
        read_through(|| lru.get_or_load(0, || Ok(of(0))).unwrap());
        assert!(lru.kept().is_empty());
        assert_eq!(lru.streaming(), vec![0]);
    }
}
