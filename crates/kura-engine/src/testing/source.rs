//! テストの合成の源（M13 段 1）：チャンクを種と位置から決定的に作る音声の源。60 分・3 時間の音声を、全体を持たずに
//! 遅延の値として作る（E41・`mem_smoke`）。作ったチャンクは LRU に置き、作った数を統計の `reads` で数える。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rand_chacha::rand_core::Rng;

use crate::audio::source::{AudioSource, ChunkLru, SourceStats, chunk_span};
use crate::error::EngineError;

/// 合成の源。標本は −0.5〜0.5 の一様な乱数（チャンネル `ch` の位置 i は `rng_at(seed, ch, i)` の流れの u32。チャンクの
/// 区切りによらず位置で決まる）。
pub struct SyntheticSource {
    rate: u32,
    channels: usize,
    frames: u64,
    seed: u64,
    lru: ChunkLru,
    reads: AtomicU64,
    hits: AtomicU64,
}

impl SyntheticSource {
    /// 源を作る。`lru_bytes` は作ったチャンクを置く LRU の上限。
    pub fn new(rate: u32, channels: usize, frames: u64, seed: u64, lru_bytes: u64) -> Self {
        Self {
            rate,
            channels,
            frames,
            seed,
            lru: ChunkLru::new(lru_bytes),
            reads: AtomicU64::new(0),
            hits: AtomicU64::new(0),
        }
    }

    /// 位置 `i` から `n` 個の標本（チャンネル `ch`）。
    pub fn samples(&self, ch: usize, start: u64, n: usize) -> Vec<f32> {
        let mut r = crate::determinism::rng_at(self.seed, ch as u64, u128::from(start));
        (0..n)
            .map(|_| (f64::from(r.next_u32()) / 4_294_967_296.0 - 0.5) as f32)
            .collect()
    }
}

impl AudioSource for SyntheticSource {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn read_chunk(&self, index: u32) -> Result<Vec<Arc<Vec<f32>>>, EngineError> {
        if let Some(hit) = self.lru.get(index) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }
        let (start, end) = chunk_span(self.frames, index).ok_or_else(|| {
            EngineError::Internal(format!("合成の源のチャンク {index} は範囲の外です"))
        })?;
        let n = usize::try_from(end - start)
            .map_err(|_| EngineError::Limit("チャンクが大きすぎます".into()))?;
        let out: Vec<Arc<Vec<f32>>> = (0..self.channels)
            .map(|ch| Arc::new(self.samples(ch, start, n)))
            .collect();
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.lru.insert(index, out.clone());
        Ok(out)
    }

    fn resident_bytes(&self) -> u64 {
        self.lru.capacity()
    }

    fn stats(&self) -> SourceStats {
        SourceStats {
            reads: self.reads.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            bytes_read: 0,
        }
    }
}
