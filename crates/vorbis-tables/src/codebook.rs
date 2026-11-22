//! books.bin の 1 冊（Vorbis I §3.2.1 の codebook の形）を読む・詰める。
//!
//! 詰める規則は libvorbis の `vorbis_staticbook_pack`（codebook.c:158–273）と同じ、読む規則は
//! `vorbis_staticbook_unpack`（codebook.c:277–399）と同じ（エンコーダが使う範囲の検査だけ）。
//! books.bin は C の `vorbis_staticbook_pack` が詰めたもので、ここで読んで詰め直すとビット単位で同じになる
//! （テストで全冊を確かめる）。整数だけで決まる。

use std::fmt;

use crate::types::BookId;

/// `static_codebook`（codebook.h）の写し。`allocedp`（メモリの持ち主の印）は持たない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticCodebook {
    /// C の `long dim`。
    pub dim: i64,
    /// C の `long entries`。
    pub entries: i64,
    /// C の `char *lengthlist`（符号語の長さ 0〜32。0 は使わない項目）。
    pub lengthlist: Vec<u8>,
    /// 0：値なし、1：格子（lookup1）、2：明示。
    pub maptype: i32,
    /// 詰めた 32 ビットの浮動小数（`_float32_unpack` で値にする）。C の表では負の `long` で書かれたものもあるが、
    /// 意味があるのは下位 32 ビットだけ。ここでは 0〜2^32−1 で持つ。
    pub q_min: i64,
    /// `q_min` と同じ形。
    pub q_delta: i64,
    /// 量子化の値のビット数（1〜16）。
    pub q_quant: i32,
    pub q_sequencep: i32,
    /// C の `long *quantlist`（maptype 1 は `lookup1_values` 個、2 は `entries * dim` 個、0 は空）。
    pub quantlist: Vec<i64>,
}

/// codebook を読めなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpackError(pub &'static str);

impl fmt::Display for UnpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "codebook を読めません: {}", self.0)
    }
}

impl std::error::Error for UnpackError {}

/// libvorbis の `ov_ilog`（sharedbook.c:35）：v を表すのに要るビット数（0 なら 0）。
pub const fn ov_ilog(v: u32) -> i32 {
    32 - v.leading_zeros() as i32
}

/// libvorbis の `_book_maptype1_quantvals`（sharedbook.c:174–209）と同じ値：`vals^dim <= entries` となる最大の
/// `vals`。C は `pow` で初めの見当を付けてから整数で確かめ直すので、答えは整数だけで決まる（ここは整数だけ）。
pub fn book_maptype1_quantvals(entries: i64, dim: i64) -> i64 {
    if entries < 1 || dim < 1 {
        return 0;
    }
    let fits = |v: i64| -> bool {
        let mut acc: i64 = 1;
        for _ in 0..dim {
            match acc.checked_mul(v) {
                Some(a) if a <= entries => acc = a,
                _ => return false,
            }
        }
        true
    };
    let mut vals = 1;
    while fits(vals + 1) {
        vals += 1;
    }
    vals
}

/// books.bin の 1 冊のバイト列とビット数。
pub fn packed(id: BookId) -> Option<(&'static [u8], u32)> {
    let b = crate::BOOKS.get(usize::from(id.0))?;
    let start = b.offset as usize;
    let bytes = crate::BOOKS_BIN.get(start..start + b.len as usize)?;
    Some((bytes, b.bits))
}

/// books.bin の 1 冊を読む。
pub fn unpack(id: BookId) -> Result<StaticCodebook, UnpackError> {
    let (bytes, bits) = packed(id).ok_or(UnpackError("番号が範囲の外"))?;
    let mut r = BitReader {
        data: bytes,
        pos: 0,
        limit: bits as usize,
    };
    let c = unpack_from(&mut r)?;
    if r.pos != r.limit {
        return Err(UnpackError("読み終わりがビット数と合わない"));
    }
    Ok(c)
}

/// LSb から読む（oggpack と同じ）。
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    limit: usize,
}

impl BitReader<'_> {
    fn read(&mut self, n: u32) -> Result<u64, UnpackError> {
        let mut v = 0u64;
        for i in 0..n {
            if self.pos >= self.limit {
                return Err(UnpackError("途中で終わった"));
            }
            let byte = self
                .data
                .get(self.pos / 8)
                .ok_or(UnpackError("途中で終わった"))?;
            if (byte >> (self.pos % 8)) & 1 == 1 {
                v |= 1 << i;
            }
            self.pos += 1;
        }
        Ok(v)
    }
}

fn unpack_from(r: &mut BitReader<'_>) -> Result<StaticCodebook, UnpackError> {
    if r.read(24)? != 0x56_4342 {
        return Err(UnpackError("同期の印が違う"));
    }
    let dim = r.read(16)? as i64;
    let entries = r.read(24)? as i64;
    if dim < 1 || entries < 1 {
        return Err(UnpackError("次元か項目の数が 0"));
    }
    let n = entries as usize;
    let mut lengthlist = vec![0u8; n];
    if r.read(1)? == 0 {
        // 長さの順でない
        let sparse = r.read(1)? == 1;
        for l in lengthlist.iter_mut() {
            if !sparse || r.read(1)? == 1 {
                *l = (r.read(5)? + 1) as u8;
            }
        }
    } else {
        // 長さの順
        let mut length = r.read(5)? + 1;
        let mut i = 0usize;
        while i < n {
            let num = r.read(ov_ilog((n - i) as u32) as u32)? as usize;
            if length > 32 || num > n - i {
                return Err(UnpackError("長さの順の表が壊れている"));
            }
            for l in &mut lengthlist[i..i + num] {
                *l = length as u8;
            }
            i += num;
            length += 1;
        }
    }
    let maptype = r.read(4)? as i32;
    let (mut q_min, mut q_delta, mut q_quant, mut q_sequencep) = (0, 0, 0, 0);
    let mut quantlist = Vec::new();
    match maptype {
        0 => {}
        1 | 2 => {
            q_min = r.read(32)? as i64;
            q_delta = r.read(32)? as i64;
            q_quant = r.read(4)? as i32 + 1;
            q_sequencep = r.read(1)? as i32;
            let quantvals = if maptype == 1 {
                book_maptype1_quantvals(entries, dim)
            } else {
                entries
                    .checked_mul(dim)
                    .ok_or(UnpackError("値の数があふれる"))?
            };
            for _ in 0..quantvals {
                quantlist.push(r.read(q_quant as u32)? as i64);
            }
        }
        _ => return Err(UnpackError("知らない maptype")),
    }
    Ok(StaticCodebook {
        dim,
        entries,
        lengthlist,
        maptype,
        q_min,
        q_delta,
        q_quant,
        q_sequencep,
        quantlist,
    })
}

/// LSb から詰める（oggpack と同じ）。
#[derive(Default)]
pub struct BitWriter {
    /// 詰めたバイト列（最後のバイトの余りのビットは 0）。
    pub bytes: Vec<u8>,
    /// 詰めたビット数。
    pub bits: usize,
}

impl BitWriter {
    /// `value` の下位 `n` ビット（n ≤ 32）を書く。
    pub fn write(&mut self, value: u64, n: u32) {
        for i in 0..n {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> i) & 1 == 1
                && let Some(last) = self.bytes.last_mut()
            {
                *last |= 1 << (self.bits % 8);
            }
            self.bits += 1;
        }
    }
}

/// libvorbis の `vorbis_staticbook_pack`（codebook.c:158–273）と同じ規則で詰める。
pub fn pack(c: &StaticCodebook, w: &mut BitWriter) -> Result<(), UnpackError> {
    let entries = c.entries as usize;
    let ll = &c.lengthlist;
    if ll.len() != entries || entries == 0 {
        return Err(UnpackError("長さの表が項目の数と合わない"));
    }
    w.write(0x56_4342, 24);
    w.write(c.dim as u64, 16);
    w.write(c.entries as u64, 24);
    let mut i = 1;
    while i < entries {
        if ll[i - 1] == 0 || ll[i] < ll[i - 1] {
            break;
        }
        i += 1;
    }
    if i == entries {
        // 長さの順
        let mut count = 0usize;
        w.write(1, 1);
        // C は `lengthlist[0]-1` を 5 ビットで書く（0 なら -1 の下位 5 ビット）
        w.write(u64::from(ll[0]).wrapping_sub(1), 5);
        for i in 1..entries {
            let (this, last) = (ll[i], ll[i - 1]);
            if this > last {
                for _ in last..this {
                    w.write((i - count) as u64, ov_ilog((entries - count) as u32) as u32);
                    count = i;
                }
            }
        }
        w.write(
            (entries - count) as u64,
            ov_ilog((entries - count) as u32) as u32,
        );
    } else {
        w.write(0, 1);
        if ll.iter().all(|&l| l != 0) {
            w.write(0, 1);
            for &l in ll {
                w.write(u64::from(l) - 1, 5);
            }
        } else {
            w.write(1, 1);
            for &l in ll {
                if l == 0 {
                    w.write(0, 1);
                } else {
                    w.write(1, 1);
                    w.write(u64::from(l) - 1, 5);
                }
            }
        }
    }
    w.write(c.maptype as u64, 4);
    match c.maptype {
        0 => {}
        1 | 2 => {
            w.write(c.q_min as u64, 32);
            w.write(c.q_delta as u64, 32);
            w.write((c.q_quant - 1) as u64, 4);
            w.write(c.q_sequencep as u64, 1);
            for &q in &c.quantlist {
                w.write(q.unsigned_abs(), c.q_quant as u32);
            }
        }
        _ => return Err(UnpackError("知らない maptype")),
    }
    Ok(())
}
