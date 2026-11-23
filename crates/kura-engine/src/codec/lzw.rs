//! GIF の LZW（M16 段 8。ADR-225）。符号は下位のビットから詰め、clear と end の符号を持ち、符号の幅は 2〜12 ビット。
//!
//! 展開（[`Decoder`]）は weezl 0.1.12（前の gif クレートの中の展開）が読めた流れを同じ値で読む：最初の clear は省いてよい
//! （始めから表がある）、clear のあと最初の符号は表にある符号だけ（次の番号以上は誤り）、ほかの符号は次の番号まで（次の番号は
//! 前の文字列＋その最初のバイト）。項目を足したあと次の番号が今の幅を超えたら幅を広げ（12 まで）、表が満ちたら（4,096）
//! clear まで項目を足さずに読み続ける。end のあとは読まない。出力が満ちたら次の符号を読まない（weezl は入力の切れ目によって
//! もう 1 つ読んで誤りにすることがあった。読めたものは同じ値）。
//!
//! 圧縮（[`encode`]）は凍結した式（`gif-kura-enc/1` の一部）：先頭に clear を書き、表に続きがなくなったら今の符号を書いて
//! 新しい項目を足す。読み手から見た次の番号（書いた符号の数から決まる）が今の幅を超えたら幅を広げ、表が満ちた（4,096 項目）
//! らすぐ clear を書いて表を作り直す（利用者の決定。前の書き手 weezl と同じ時点）。最後の符号のあと、読み手が足す項目で
//! 幅が広がるならそれを当てて end を書き、バイトの境目まで 0 で詰める。

/// 符号の幅の上限。
pub(crate) const MAX_BITS: u32 = 12;
/// 表の大きさ（12 ビットの番号の数）。
pub(crate) const MAX_ENTRIES: u32 = 1 << MAX_BITS;

/// 表（前の符号と次のバイト → 符号）の開番地のハッシュの大きさ（項目の 2 倍。満ちても半分まで）。
const SLOTS: usize = 8192;

/// 圧縮の表。作り直しのたびに世代を進め、古い項目は世代で見分ける（毎回 0 で埋めない）。
struct Dict {
    /// 世代 << 20 | 前の符号 << 8 | バイト（0 は空）。
    keys: Vec<u64>,
    codes: Vec<u16>,
    era: u64,
}

impl Dict {
    fn new() -> Self {
        Dict {
            keys: vec![0; SLOTS],
            codes: vec![0; SLOTS],
            era: 1,
        }
    }

    fn reset(&mut self) {
        self.era += 1;
        if self.era >= 1 << 40 {
            self.keys.fill(0);
            self.era = 1;
        }
    }

    #[inline(always)]
    fn slot(key: u32) -> usize {
        (key.wrapping_mul(0x9E37_79B1) >> (32 - 13)) as usize
    }

    /// 前の符号 `prefix` にバイト `b` を続けた符号。
    #[inline(always)]
    fn get(&self, prefix: u16, b: u8) -> Option<u16> {
        let key = (u32::from(prefix) << 8) | u32::from(b);
        let want = (self.era << 20) | u64::from(key);
        let mut i = Self::slot(key);
        loop {
            let k = self.keys[i];
            if k == want {
                return Some(self.codes[i]);
            }
            if k >> 20 != self.era {
                return None;
            }
            i = (i + 1) & (SLOTS - 1);
        }
    }

    #[inline(always)]
    fn insert(&mut self, prefix: u16, b: u8, code: u16) {
        let key = (u32::from(prefix) << 8) | u32::from(b);
        let mut i = Self::slot(key);
        while self.keys[i] >> 20 == self.era {
            i = (i + 1) & (SLOTS - 1);
        }
        self.keys[i] = (self.era << 20) | u64::from(key);
        self.codes[i] = code;
    }
}

/// 下位のビットから詰める。
struct BitOut<'a> {
    out: &'a mut Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitOut<'_> {
    #[inline(always)]
    fn put(&mut self, code: u16, width: u32) {
        self.acc |= u32::from(code) << self.n;
        self.n += width;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    fn finish(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }
}

/// 番号の列 `data`（どれも `1 << min_size` より小さいこと）を、最小の符号の長さ `min_size`（2〜8）で LZW にして `out` に
/// 足す（サブブロックには分けない）。番号が範囲の外なら偽を返す（何も足さない）。
pub(crate) fn encode(data: &[u8], min_size: u32, out: &mut Vec<u8>) -> bool {
    if !(2..=8).contains(&min_size) || data.iter().any(|&b| u32::from(b) >= 1 << min_size) {
        return false;
    }
    let clear = 1u16 << min_size;
    let end = clear + 1;
    let mut dict = Dict::new();
    let mut bits = BitOut { out, acc: 0, n: 0 };
    let mut width = min_size + 1;
    // 次に足す番号（読み手が今の符号を読んだあとの次の番号と同じ）
    let mut next = u32::from(end) + 1;
    bits.put(clear, width);
    let Some((&first, rest)) = data.split_first() else {
        bits.put(end, width);
        bits.finish();
        return true;
    };
    let mut prefix = u16::from(first);
    for &b in rest {
        if let Some(c) = dict.get(prefix, b) {
            prefix = c;
            continue;
        }
        bits.put(prefix, width);
        // 読み手はこの符号で項目を足し（clear の直後の符号でなければ）、次の番号が幅を超えたら広げる
        if next >= 1 << width && width < MAX_BITS {
            width += 1;
        }
        if next >= MAX_ENTRIES {
            // 表が満ちた：すぐ clear
            bits.put(clear, width);
            dict.reset();
            width = min_size + 1;
            next = u32::from(end) + 1;
        } else {
            dict.insert(prefix, b, next as u16);
            next += 1;
        }
        prefix = u16::from(b);
    }
    bits.put(prefix, width);
    if next >= 1 << width && width < MAX_BITS {
        width += 1;
    }
    bits.put(end, width);
    bits.finish();
    true
}

/// 展開の誤り（表にない符号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidCode;

/// LZW の展開。入力を少しずつ渡し（[`Decoder::feed`]）、出力の置き場が満ちるか end で止まる。置き場に入りきらなかった
/// 文字列の残りは覚えておき、次の呼び出しの置き場の頭に書く（行ずつの置き場で読める）。
pub(crate) struct Decoder {
    min_size: u32,
    clear: u32,
    end: u32,
    next: u32,
    width: u32,
    /// 項目の前の符号・最後のバイト・最初のバイト・長さ。
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    first: Vec<u8>,
    len: Vec<u16>,
    /// 前の符号（clear の直後は None）。
    prev: Option<u32>,
    acc: u64,
    nbits: u32,
    ended: bool,
    /// 書きかけの文字列（符号と、書いたバイトの数）。
    pending: Option<(u32, usize)>,
}

impl Decoder {
    /// 最小の符号の長さ `min_size`（1〜11）の展開。
    pub(crate) fn new(min_size: u32) -> Self {
        let min_size = min_size.clamp(1, 11);
        let n = MAX_ENTRIES as usize;
        let mut d = Decoder {
            min_size,
            clear: 1 << min_size,
            end: (1 << min_size) + 1,
            next: 0,
            width: 0,
            prefix: vec![0; n],
            suffix: vec![0; n],
            first: vec![0; n],
            len: vec![0; n],
            prev: None,
            acc: 0,
            nbits: 0,
            ended: false,
            pending: None,
        };
        for c in 0..d.clear as usize {
            d.suffix[c] = c as u8;
            d.first[c] = c as u8;
            d.len[c] = 1;
        }
        d.reset();
        d
    }

    fn reset(&mut self) {
        self.next = self.end + 1;
        self.width = self.min_size + 1;
        self.prev = None;
    }

    /// end を読んだか。
    pub(crate) fn ended(&self) -> bool {
        self.ended
    }

    /// 書きかけの文字列があるか。
    pub(crate) fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// 符号 `code` の文字列の `skip` バイト目からを `out[*at..]` に入る分だけ書く。入りきらなければ残りを覚える。
    fn put(&mut self, code: u32, skip: usize, out: &mut [u8], at: &mut usize) {
        let n = usize::from(self.len[code as usize]);
        let k = (n - skip).min(out.len() - *at);
        // 後ろから前の符号をたどり、[skip, skip + k) の位置だけ書く
        let mut c = code as usize;
        let mut i = n;
        while i > skip {
            i -= 1;
            if i < skip + k {
                out[*at + i - skip] = self.suffix[c];
            }
            c = usize::from(self.prefix[c]);
        }
        *at += k;
        self.pending = (skip + k < n).then_some((code, skip + k));
    }

    /// 入力 `inp` を読み、`out[*at..]` に画素を書く。出力が満ちるか end か入力の終わりで止まり、読んだ入力のバイト数を返す
    /// （満ちたら残りの入力は読まない）。表にない符号は誤り。
    pub(crate) fn feed(
        &mut self,
        inp: &[u8],
        out: &mut [u8],
        at: &mut usize,
    ) -> Result<usize, InvalidCode> {
        if let Some((code, done)) = self.pending {
            self.put(code, done, out, at);
        }
        let mut used = 0;
        loop {
            if self.ended || self.pending.is_some() || *at >= out.len() {
                return Ok(used);
            }
            while self.nbits < self.width {
                let Some(&b) = inp.get(used) else {
                    return Ok(used);
                };
                used += 1;
                self.acc |= u64::from(b) << self.nbits;
                self.nbits += 8;
            }
            let code = (self.acc & ((1 << self.width) - 1)) as u32;
            self.acc >>= self.width;
            self.nbits -= self.width;
            if code == self.clear {
                self.reset();
                continue;
            }
            if code == self.end {
                self.ended = true;
                return Ok(used);
            }
            let Some(prev) = self.prev else {
                // clear の直後（か始め）：表にある符号だけ
                if code >= self.next {
                    return Err(InvalidCode);
                }
                self.put(code, 0, out, at);
                self.prev = Some(code);
                continue;
            };
            if code > self.next || (code == self.next && self.next >= MAX_ENTRIES) {
                return Err(InvalidCode);
            }
            let first = if code == self.next {
                // 前の文字列＋その最初のバイト
                self.first[prev as usize]
            } else {
                self.first[code as usize]
            };
            if self.next < MAX_ENTRIES {
                let n = self.next as usize;
                self.prefix[n] = prev as u16;
                self.suffix[n] = first;
                self.first[n] = self.first[prev as usize];
                self.len[n] = self.len[prev as usize].saturating_add(1);
                if self.next + 1 >= 1 << self.width && self.width < MAX_BITS {
                    self.width += 1;
                }
                self.next += 1;
            }
            self.put(code, 0, out, at);
            self.prev = Some(code);
        }
    }
}

/// 番号の列で使う最小の符号の長さ（最大の番号が入るビット数。2 以上）。
pub(crate) fn min_size_for(data: &[u8]) -> u32 {
    let max = data.iter().copied().max().unwrap_or(0);
    (u32::BITS - u32::from(max).leading_zeros()).max(2)
}

#[cfg(test)]
mod tests;
