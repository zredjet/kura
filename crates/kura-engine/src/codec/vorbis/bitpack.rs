//! oggpack の書き手（LSb から詰める）。
//!
//! # 移す C
//!
//! - `ogg_vendor/src/bitwise.c`：`mask[]`（27–37）、`oggpack_writeinit`（39–44）、`oggpack_write`（82–122）、`oggpack_reset`
//!   （238–243）、`oggpack_bytes`（502–504）・`oggpack_bits`（506–508）・`oggpack_get_buffer`（518–520）。
//! - `lib/codebook.c:69–111`：aoTuV の速い `vorbis_oggpack_write`（`vorbis_book_encode` だけが使う）。4 バイトをまとめて
//!   書くが、書くビット列は libogg の `oggpack_write` と同じ（後ろのバイトは次の書き込みで上書きされ、最後のバイトの
//!   余りのビットは 0）。移植では 1 つの書き手にまとめてよい。
//!
//! # 規則
//!
//! - バイトは 0 番から順。バイトの中はビット 0 から埋める。n ビットの値は、値の LSb を空いている最も下のビットに置く。
//! - `value &= mask[bits]`（bits は 0〜32）。0 ビットの書き込みは何もしない（`vorbis_book_encode` が長さ 0 の項目で呼ぶことがある）。
//! - パケットの終わりの端数のビットは 0。バイト数は `endbyte + (endbit != 0)`。
//! - libogg の `oggpack_write` は bits > 32 で書き手を壊す（`err`）。エンコーダは 32 以下でしか呼ばないが、Rust では
//!   範囲の外を `EngineError::Internal` にする。
//!
//! # 移さないもの
//!
//! - 読み手（`oggpack_read`・`look`・`adv`）。エンコーダは読まない（codebook の読み戻しは vorbis-tables の `unpack`）。
//! - 大きさの管理（`BUFFER_INCREMENT`・`realloc`）。`Vec` で持つ。
//!
//! # 型の対応
//!
//! `oggpack_buffer`（ogg.h）→ [`OggpackBuffer`]（`endbyte`・`endbit`・`buffer`。`ptr`・`storage` は `Vec` が持つ）。
//!
//! ライセンス：aoTuV b6.03・libvorbis・libogg の C を移したもの（Copyright (c) 2003-2021 Aoyumi、Copyright (c) 2002-2020
//! Xiph.org Foundation）。BSD-3-Clause で、条件と免責は同じディレクトリの `LICENSE`（ADR-149）。

use crate::error::EngineError;

/// `oggpack_buffer` の写し（書くだけ）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OggpackBuffer {
    /// 書いたバイト（最後のバイトは書きかけ）。
    pub buffer: Vec<u8>,
    /// 書きかけのバイトの番号（C の `endbyte`）。
    pub endbyte: usize,
    /// 書きかけのバイトで使ったビット数 0〜7（C の `endbit`）。
    pub endbit: u32,
}

impl OggpackBuffer {
    /// `oggpack_writeinit`。
    pub fn oggpack_writeinit() -> Self {
        Self::default()
    }

    /// `oggpack_reset`（空にする）。
    pub fn oggpack_reset(&mut self) {
        self.buffer.clear();
        self.endbyte = 0;
        self.endbit = 0;
    }

    /// `oggpack_write`（bitwise.c:82–122）：`value` の下位 `bits` ビット（0〜32）を書く。
    pub fn oggpack_write(&mut self, value: u64, bits: u32) -> Result<(), EngineError> {
        if bits > 32 {
            return Err(EngineError::Internal(format!(
                "oggpack_write：{bits} ビットは書けません"
            )));
        }
        let mut v = value & ((1u64 << bits) - 1);
        let mut left = bits;
        while left > 0 {
            if self.buffer.len() <= self.endbyte {
                self.buffer.push(0);
            }
            let take = (8 - self.endbit).min(left);
            let part = (v & ((1u64 << take) - 1)) as u8;
            self.buffer[self.endbyte] |= part << self.endbit;
            v >>= take;
            left -= take;
            self.endbit += take;
            if self.endbit == 8 {
                self.endbit = 0;
                self.endbyte += 1;
            }
        }
        Ok(())
    }

    /// `oggpack_writetrunc`（bitwise.c:59–68）：`bits` ビットまでに切り詰める（それより後ろは 0）。
    #[cfg(test)]
    pub fn oggpack_writetrunc(&mut self, bits: usize) {
        let bytes = bits >> 3;
        let rest = (bits & 7) as u32;
        if bytes > self.endbyte || (bytes == self.endbyte && rest > self.endbit) {
            return;
        }
        self.endbyte = bytes;
        self.endbit = rest;
        self.buffer.truncate(bytes + 1);
        if let Some(last) = self.buffer.get_mut(bytes) {
            *last &= ((1u16 << rest) - 1) as u8;
        }
    }

    /// `oggpack_bytes`：書いたバイト数（端数のビットを含むバイトも数える）。
    pub fn oggpack_bytes(&self) -> usize {
        self.endbyte + usize::from(self.endbit != 0)
    }

    /// `oggpack_bits`：書いたビット数。
    #[cfg(test)]
    pub fn oggpack_bits(&self) -> usize {
        self.endbyte * 8 + self.endbit as usize
    }

    /// `oggpack_get_buffer`：書いたバイト列（`oggpack_bytes` バイト）。
    pub fn oggpack_get_buffer(&self) -> &[u8] {
        &self.buffer[..self.oggpack_bytes().min(self.buffer.len())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vorbis の詰め方（各バイトの最下位ビットから）。仕様 §2.1.4 の例：4 ビットの 12、3 ビットの −1（7）、
    /// 7 ビットの 17、13 ビットの 6969 → 0b11111100、0b01001000、0b11001110、0b00000110。
    #[test]
    fn packs_from_the_lsb_like_the_spec() {
        let mut b = OggpackBuffer::oggpack_writeinit();
        b.oggpack_write(12, 4).unwrap();
        b.oggpack_write(7, 3).unwrap();
        b.oggpack_write(17, 7).unwrap();
        b.oggpack_write(6969, 13).unwrap();
        assert_eq!(
            b.oggpack_get_buffer(),
            &[0b1111_1100, 0b0100_1000, 0b1100_1110, 0b0000_0110]
        );
        assert_eq!(b.oggpack_bits(), 27);
        assert_eq!(b.oggpack_bytes(), 4);
        // 0 ビット・32 ビット・範囲の外
        b.oggpack_write(0xFFFF_FFFF, 0).unwrap();
        assert_eq!(b.oggpack_bits(), 27);
        b.oggpack_write(0xDEAD_BEEF, 32).unwrap();
        assert_eq!(b.oggpack_bits(), 59);
        assert!(b.oggpack_write(0, 33).is_err());
        // 上位の余分なビットは捨てる
        let mut c = OggpackBuffer::oggpack_writeinit();
        c.oggpack_write(0xFF, 3).unwrap();
        assert_eq!(c.oggpack_get_buffer(), &[0b111]);
        // 切り詰め
        b.oggpack_writetrunc(10);
        assert_eq!(b.oggpack_bits(), 10);
        assert_eq!(b.oggpack_get_buffer(), &[0b1111_1100, 0b00]);
        b.oggpack_reset();
        assert_eq!(b.oggpack_bytes(), 0);
    }
}
