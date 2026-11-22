//! MSB から読むビットの読み手（AAC のパケット。読み終えた先は 0。足りなければ壊れたファイル）。

use crate::error::EngineError;

use super::decode_err;

/// MSB から読むビットの読み手。
pub(crate) struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    /// 残りのビット。
    pub fn remaining(&self) -> usize {
        (self.b.len() * 8).saturating_sub(self.pos)
    }

    /// 読んだビットの数（パケットの頭から）。
    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// パケットのビットの数。
    #[inline]
    pub fn total(&self) -> usize {
        self.b.len() * 8
    }

    /// 位置を `pos`（[`Self::total`] まで。呼び手が確かめる）にする。
    #[inline]
    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// 今の位置から 64 ビット（先は 0）。上位に詰める。
    #[inline]
    pub fn window(&self) -> u64 {
        self.window_at(self.pos)
    }

    /// 位置 `pos` から 64 ビット（先は 0）。上位に詰める（下の `pos % 8` ビットは 0 で、確かなのは 57 ビット以上）。
    #[inline]
    pub fn window_at(&self, pos: usize) -> u64 {
        let at = pos / 8;
        let w = match self.b.get(at..at + 8) {
            Some(s) => u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]),
            None => {
                let mut t = [0u8; 8];
                let tail = self.b.get(at..).unwrap_or(&[]);
                t[..tail.len()].copy_from_slice(tail);
                u64::from_be_bytes(t)
            }
        };
        w << (pos % 8)
    }

    /// 次の `n`（1〜32）ビットを覗く（先は 0）。
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        (self.window() >> (64 - n)) as u32
    }

    /// `n` ビット進める。残りが足りなければ誤り。
    #[inline]
    pub fn skip(&mut self, n: usize) -> Result<(), EngineError> {
        if self.remaining() < n {
            return Err(eob());
        }
        self.pos += n;
        Ok(())
    }

    /// `n`（0〜32）ビットを符号なしで。
    #[inline]
    pub fn read(&mut self, n: u32) -> Result<u32, EngineError> {
        if n == 0 {
            return Ok(0);
        }
        if self.remaining() < n as usize {
            return Err(eob());
        }
        let v = self.peek(n);
        self.pos += n as usize;
        Ok(v)
    }

    pub fn bit(&mut self) -> Result<bool, EngineError> {
        Ok(self.read(1)? == 1)
    }

    /// パケットの頭からのバイトの境目まで進める。
    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

pub(crate) fn eob() -> EngineError {
    decode_err("パケットが途中で切れています")
}
