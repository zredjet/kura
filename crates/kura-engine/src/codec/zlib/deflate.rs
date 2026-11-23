//! deflate の圧縮（M16 段 7。RFC 1950・1951。ADR-224）。PNG・APNG の IDAT・fdAT と iCCP を書く（`png_write`）。
//!
//! 段（[`Level`]）はどれも凍結した式で、同じ入力と段なら同じバイト列になる（入力を渡す切れ目にもよらない。解く位置は
//! 先読みが [`LOOKAHEAD`] あるところまで（`High` は 1 ブロック分が揃うまで）で、ブロックの区切りは記号と元のバイトの数で
//! 決める）。式を変えるときは、書く側（`png-kura-enc/N`）の版を上げる。
//! - `Fast`：一致を探さない。前のバイトと同じバイトが [`RUN_MIN`] 個以上続くところだけを距離 1 の一致にし、ほかは文字の並び
//!   （ブロックごとの動的ハフマン。文字と長さの符号は 12 ビットまで（4 つの文字を 1 度に書く））。ハッシュの表を持たない。
//! - `Balanced`：zlib の `deflate_slow` と同じ意味の遅延の一致（3 バイトのハッシュの鎖・good・lazy・nice。[`Params`]）。
//! - `High`：最適に近い解き方（`parse_optimal`）。ブロックごとに各位置の一致の候補を集め、費用の模型で動的計画法で選び、
//!   選んだ記号の頻度で費用を作り直して解き直す。
//! - 位置は 32 ビットの絶対位置で持ち、距離を引き算で出して窓（32 KiB）の中だけを候補にする（表を作り直さずに古い入力を
//!   捨てられる。4 GiB を超えて回っても、候補はバイトを比べて確かめる）。
//! - ブロックは記号が [`BLOCK_SYMS`] 個か元のバイトが [`BLOCK_RAW`] になったら閉じ（`High` は `Params::opt_block` ずつ）、
//!   動的・固定・stored のうち最も短いもの（同じなら動的・固定・stored の順に先のもの）で書く。動的の符号は長さを 15 ビット
//!   （符号長の符号は 7 ビット）に抑えたハフマン符号（Moffat–Katajainen の最小冗長の長さを、miniz と同じ手順で上限に
//!   収める。並べ替えは頻度と記号の番号で決める）。符号の長さの列は、文字と長さの表と距離の表を続けた 1 つの列として
//!   16・17・18 で詰める。使う記号が 2 つ未満の表は、zlib と同じく使わない記号を足して 2 つにする（どの読み手も受け付ける
//!   完全な符号にする）。

use super::{
    CLEN_ORDER, DIST_BASE, DIST_EXTRA, LEN_BASE, LEN_EXTRA, MAX_MATCH, WINDOW, adler32_update,
    reverse,
};

/// 段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// 速さを優先する（サムネイル・エディタのタイル）。
    Fast,
    /// 既定。
    Balanced,
    /// 大きさを優先する。
    High,
}

/// 一致の最短。
const MIN_MATCH: usize = 3;
/// 解く位置の先に要るバイト数（一致を 258 バイトまで伸ばし、8 バイトずつ比べ、遅延の一致で 1 つ先を見る）。
const LOOKAHEAD: usize = MAX_MATCH + 24;
/// 終わりに足す詰め物（論理の終わりの先を 8 バイトずつ読んでよいように。書くバイトには入らない）。
const PAD: usize = 24;
/// ブロックの記号の数の上限。
const BLOCK_SYMS: usize = 16_384;
/// ブロックの元のバイト数の上限（stored で書くとき、元のバイト列を持っている必要がある）。
const BLOCK_RAW: usize = 1 << 20;
/// 一度に受け取る入力（これより長い入力は分けて解く。持つ領域を抑える）。
const CHUNK: usize = 256 * 1024;
/// 古い入力を捨てる単位（これだけ捨てられるようになったら捨てる）。
const SLIDE: usize = 256 * 1024;
/// `High` の解き方の 1 つのブロックの大きさの上限（`Params::opt_block`）。
const OPT_BLOCK_MAX: usize = 1 << 17;
const _: () = assert!(Params::of(Level::High).opt_block <= OPT_BLOCK_MAX);

/// 窓の長さの上限：窓、閉じていないブロックの元（一致がはみ出す分を含む）、まだ捨てない分、解いていない入力（先読みと
/// `High` の 1 ブロック）、一度に足す入力、詰め物。
const BUF_MAX: usize =
    WINDOW + BLOCK_RAW + MAX_MATCH + SLIDE + LOOKAHEAD + OPT_BLOCK_MAX + CHUNK + PAD;
/// ハッシュの表のビット数。
const HASH_BITS: u32 = 15;
const WMASK: usize = WINDOW - 1;

/// 一致の探し方（zlib の `configuration_table` と同じ意味の数）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Params {
    /// 鎖をたどる数の上限。
    chain: u32,
    /// 前の一致がこれ以上なら、鎖を 1/4 にする。
    good: usize,
    /// 前の一致がこれ以上なら、次の位置で探さない。
    lazy: usize,
    /// これ以上の一致が見つかれば探すのをやめる。
    nice: usize,
    /// 一致を探さず、前のバイトと同じバイトの続き（距離 1）だけを一致にする（`Fast`）。
    rle: bool,
    /// 長さ 3 の一致をこの距離より遠ければ使わない。
    too_far: usize,
    /// 文字と長さの符号の長さの上限（`Fast` は 12。4 つの文字を 1 度に書けるように）。
    lit_limit: u32,
    /// 最適に近い解き方の、費用を見直して解き直す回数（0 なら遅延の一致。`High`）。
    optimal: u32,
    /// 最適に近い解き方で 1 つのブロックにする元のバイト数。
    opt_block: usize,
    /// 最適に近い解き方：この長さ以上の一致が見つかれば、その中の位置では探さない（表には入れる）。
    opt_skip: usize,
    /// ブロックの記号の数の上限。
    block_syms: usize,
}

impl Params {
    pub(crate) const fn of(level: Level) -> Self {
        match level {
            Level::Fast => Params {
                chain: 0,
                good: 0,
                lazy: 0,
                nice: 0,
                rle: true,
                too_far: 0,
                lit_limit: 12,
                optimal: 0,
                opt_block: 0,
                opt_skip: 0,
                block_syms: BLOCK_SYMS,
            },
            Level::Balanced => Params {
                chain: 64,
                good: 4,
                lazy: 16,
                nice: 64,
                rle: false,
                too_far: 4096,
                lit_limit: 15,
                optimal: 0,
                opt_block: 0,
                opt_skip: 0,
                block_syms: BLOCK_SYMS,
            },
            Level::High => Params {
                chain: 24,
                good: 0,
                lazy: 0,
                nice: 258,
                rle: false,
                too_far: 0,
                lit_limit: 15,
                optimal: 3,
                opt_block: 1 << 17,
                opt_skip: 32,
                block_syms: BLOCK_SYMS,
            },
        }
    }
}

/// 長さ − 3 → 長さの符号の番号（0〜28）。
const LEN_CODE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut c = 0;
    while c < 28 {
        let base = LEN_BASE[c] as usize;
        let n = 1usize << LEN_EXTRA[c];
        let mut k = 0;
        while k < n {
            if base + k - 3 < 256 {
                t[base + k - 3] = c as u8;
            }
            k += 1;
        }
        c += 1;
    }
    t[255] = 28;
    t
};

/// 距離 − 1 → 距離の符号の番号（0〜29）。
#[inline]
fn dist_code(x: u32) -> usize {
    if x < 4 {
        x as usize
    } else {
        let n = 31 - x.leading_zeros();
        (2 * n + ((x >> (n - 1)) & 1)) as usize
    }
}

/// 固定のブロックの符号の長さ（RFC 1951 §3.2.6）。
const FIXED_LIT_LENS: [u8; 288] = {
    let mut t = [0u8; 288];
    let mut i = 0;
    while i < 288 {
        t[i] = if i < 144 {
            8
        } else if i < 256 {
            9
        } else if i < 280 {
            7
        } else {
            8
        };
        i += 1;
    }
    t
};

/// ビットを下位から詰める。
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// `v` の下位 `n` ビット（32 まで）を足す。
    #[inline(always)]
    fn put(&mut self, v: u32, n: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += n;
        if self.n >= 32 {
            self.out.extend_from_slice(&(self.acc as u32).to_le_bytes());
            self.acc >>= 32;
            self.n -= 32;
        }
    }

    /// `v` の下位 `n` ビットを足す（書き出さない。足したあと 64 ビットに収まること）。
    #[inline(always)]
    fn put_raw(&mut self, v: u64, n: u32) {
        self.acc |= v << self.n;
        self.n += n;
    }

    /// 詰まったバイトを書き出す（残りは 7 ビットまで）。
    #[inline(always)]
    fn flush_bytes(&mut self) {
        let k = self.n / 8;
        let len = self.out.len();
        self.out.extend_from_slice(&self.acc.to_le_bytes());
        self.out.truncate(len + k as usize);
        self.acc = self.acc.checked_shr(8 * k).unwrap_or(0);
        self.n -= 8 * k;
    }

    /// バイトの境目まで 0 で詰め、詰まったバイトを書き出す。
    fn align(&mut self) {
        self.n += (8 - self.n % 8) % 8;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
}

/// 頻度 `freq` から、上限 `limit` ビットの符号の長さを `lens` に作る（使わない記号は 0）。使う記号が 2 つ未満なら、番号の
/// 小さい使わない記号を頻度 1 として足す（zlib と同じく、どの表も 2 つ以上の記号の完全な符号にする）。
pub(crate) fn huff_lengths(freq: &[u32], limit: u32, lens: &mut [u8]) {
    lens.fill(0);
    let mut syms: Vec<(u32, u16)> = Vec::with_capacity(freq.len());
    for (i, &f) in freq.iter().enumerate() {
        if f > 0 {
            syms.push((f, i as u16));
        }
    }
    let mut i = 0;
    while syms.len() < 2 && i < freq.len() {
        if freq[i] == 0 {
            syms.push((1, i as u16));
        }
        i += 1;
    }
    if syms.len() < 2 {
        // 記号が 1 つしかない表（呼び手は使わない）
        if let Some(&(_, s)) = syms.first() {
            lens[s as usize] = 1;
        }
        return;
    }
    // 頻度の小さい順（同じなら番号の小さい順）
    syms.sort_by_key(|&(f, s)| (f, s));
    let n = syms.len();
    let mut a: Vec<u32> = syms.iter().map(|&(f, _)| f).collect();
    minimum_redundancy(&mut a);
    // 長さごとの数（上限を超えたものは上限へ寄せ、Kraft の和を 1 に戻す）
    let limit = limit as usize;
    let mut num = [0u32; 33];
    for &l in &a {
        num[(l as usize).min(32)] += 1;
    }
    if num[limit + 1..].iter().any(|&c| c > 0) {
        for i in limit + 1..=32 {
            num[limit] += num[i];
            num[i] = 0;
        }
        let mut total: u64 = 0;
        for (i, &c) in num.iter().enumerate().take(limit + 1).skip(1) {
            total += u64::from(c) << (limit - i);
        }
        while total != 1u64 << limit {
            num[limit] -= 1;
            for i in (1..limit).rev() {
                if num[i] != 0 {
                    num[i] -= 1;
                    num[i + 1] += 2;
                    break;
                }
            }
            total -= 1;
        }
    }
    // 頻度の大きいものから短い長さを割り当てる
    let mut k = n;
    for (l, &c) in num.iter().enumerate().take(limit + 1).skip(1) {
        for _ in 0..c {
            k -= 1;
            lens[syms[k].1 as usize] = l as u8;
        }
    }
}

/// 頻度の小さい順に並んだ `a`（2 つ以上）を、その順の記号の符号の長さに置き換える（Moffat–Katajainen の場所を取らない
/// 最小冗長の符号。miniz の `tdefl_calculate_minimum_redundancy` と同じ手順）。
fn minimum_redundancy(a: &mut [u32]) {
    let n = a.len();
    a[0] += a[1];
    let mut root = 0usize;
    let mut leaf = 2usize;
    for next in 1..n - 1 {
        if leaf >= n || a[root] < a[leaf] {
            a[next] = a[root];
            a[root] = next as u32;
            root += 1;
        } else {
            a[next] = a[leaf];
            leaf += 1;
        }
        if leaf >= n || (root < next && a[root] < a[leaf]) {
            a[next] += a[root];
            a[root] = next as u32;
            root += 1;
        } else {
            a[next] += a[leaf];
            leaf += 1;
        }
    }
    a[n - 2] = 0;
    for next in (0..n.saturating_sub(2)).rev() {
        a[next] = a[a[next] as usize] + 1;
    }
    let mut avbl: i64 = 1;
    let mut used: i64 = 0;
    let mut dpth: u32 = 0;
    let mut root = n as i64 - 2;
    let mut next = n as i64 - 1;
    while avbl > 0 {
        while root >= 0 && a[root as usize] == dpth {
            used += 1;
            root -= 1;
        }
        while avbl > used {
            a[next as usize] = dpth;
            next -= 1;
            avbl -= 1;
        }
        avbl = 2 * used;
        dpth += 1;
        used = 0;
    }
}

/// 符号の長さから正規の符号（下位から書くので反転したもの）。
pub(crate) fn huff_codes(lens: &[u8], codes: &mut [u16]) {
    let mut count = [0u16; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for b in 1..16 {
        code = (code + count[b - 1]) << 1;
        next[b] = code;
    }
    for (s, &l) in lens.iter().enumerate() {
        if l > 0 {
            codes[s] = reverse(u32::from(next[l as usize]), u32::from(l)) as u16;
            next[l as usize] += 1;
        }
    }
}

/// 符号の長さの列の、繰り返しを詰めた項目（記号 0〜18 と追加のビットの値）。
fn rle_lengths(seq: &[u8], items: &mut Vec<(u8, u8)>) {
    items.clear();
    let mut i = 0;
    while i < seq.len() {
        let l = seq[i];
        let mut run = 1;
        while i + run < seq.len() && seq[i + run] == l {
            run += 1;
        }
        i += run;
        if l == 0 {
            while run >= 11 {
                let r = run.min(138);
                items.push((18, (r - 11) as u8));
                run -= r;
            }
            if run >= 3 {
                items.push((17, (run - 3) as u8));
                run = 0;
            }
        } else {
            items.push((l, 0));
            run -= 1;
            while run >= 3 {
                let r = run.min(6);
                items.push((16, (r - 3) as u8));
                run -= r;
            }
        }
        for _ in 0..run {
            items.push((l, 0));
        }
    }
}

/// 符号の長さの符号の追加のビット数。
const CLEN_EXTRA: [u32; 19] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// 動的のブロックの表（書くときに使う）。
struct Dynamic {
    llens: [u8; 288],
    lcodes: [u16; 288],
    dlens: [u8; 32],
    dcodes: [u16; 32],
    hlit: usize,
    hdist: usize,
    hclen: usize,
    clens: [u8; 19],
    ccodes: [u16; 19],
    items: Vec<(u8, u8)>,
}

/// deflate（zlib の包み）の圧縮器。[`Deflate::write`] で入力を渡し、[`Deflate::finish`] で閉じる。書いたバイトは
/// [`Deflate::output`] から取り出せる（取り出す量と時は結果に効かない）。
pub struct Deflate {
    params: Params,
    level: Level,
    /// 窓（捨てていない入力）。論理の終わりは `end`（`finish` のあとは詰め物を足す）。
    buf: Vec<u8>,
    end: usize,
    /// `buf[0]` の絶対位置。
    base: u64,
    /// 流れの最初の絶対位置。
    origin: u64,
    /// 次に解く位置（`buf` の中）。
    pos: usize,
    /// 今のブロックの元の始まり（`buf` の中）。
    block_start: usize,
    /// `Fast`：まだ記号にしていない文字の並びの始まり（`buf` の中。`pos` まで）。
    span: usize,
    head: Vec<u32>,
    prev: Vec<u32>,
    /// 遅延の一致：前の位置の一致（長さ・距離）と、前の位置の文字がまだ書かれていないか。
    prev_match: (usize, usize),
    avail: bool,
    syms: Vec<u32>,
    lit: [u32; 288],
    dist: [u32; 32],
    /// 文字の並びの頻度（4 つに分けた表。ブロックを閉じるときに `lit` へ足す）。
    hist: Box<[[u32; 256]; 4]>,
    /// `High` の解き方の作業領域。
    opt: Opt,
    bits: Bits,
    adler: u32,
    finished: bool,
}

/// 記号：上の 1 ビットが立っていれば一致（距離 − 1 を 8 ビット目から、長さ − 3 を下位 8 ビット）、次のビットが立って
/// いれば文字の並び（下位 30 ビットが長さ。中身は窓のブロックの元から読む）、どちらもなければ文字（下位 8 ビット）。
const MATCH_FLAG: u32 = 0x8000_0000;
const SPAN_FLAG: u32 = 0x4000_0000;
/// 文字の並びの記号 1 つの長さの上限。
const SPAN_MAX: usize = 1 << 20;
/// `Fast` で一致にする続きの最短。
const RUN_MIN: usize = 4;
/// `Fast` の走査で 1 度に見る位置の数（16 バイトの比べのうち、続きの終わりまで見えるレーン）。
const RUN_STEP: usize = 16 - RUN_MIN + 1;
const RUN_LANES: u128 = (1u128 << (8 * RUN_STEP)) - 1;

/// `buf[p..p + 8]`（リトルエンディアン）。
#[inline(always)]
fn load64(buf: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(buf[p..p + 8].try_into().unwrap_or([0; 8]))
}

/// 窓 `w`（位置 q − 1 から）の q + k + i（i = 0〜7）のバイトが前のバイトと同じなら、バイト i の上のビットを立てる。
#[inline(always)]
fn eq_bytes(w: &[u8; 18], k: usize) -> u64 {
    let mut a = [0u8; 8];
    let mut b = [0u8; 8];
    a.copy_from_slice(&w[k + 1..k + 9]);
    b.copy_from_slice(&w[k..k + 8]);
    let x = u64::from_le_bytes(a) ^ u64::from_le_bytes(b);
    const LO: u64 = 0x7F7F_7F7F_7F7F_7F7F;
    !(((x & LO) + LO) | x) & !LO
}

/// `buf[c..]` が `b` の続きである長さ（`max` まで。8 バイトずつ比べる）。
#[inline(always)]
fn run_len(buf: &[u8], c: usize, b: u8, max: usize) -> usize {
    let pat = u64::from(b) * 0x0101_0101_0101_0101;
    let mut k = 0;
    while k < max {
        let d = load64(buf, c + k) ^ pat;
        if d != 0 {
            return (k + (d.trailing_zeros() / 8) as usize).min(max);
        }
        k += 8;
    }
    max
}

/// バイトの頻度を 4 つの表に分けて数える（ブロックを閉じるときに足す。値は 1 つの表で数えたのと同じ）。
fn count_bytes(t: &mut [[u32; 256]; 4], data: &[u8]) {
    let (chunks, rest) = data.as_chunks::<4>();
    for c in chunks {
        t[0][c[0] as usize] += 1;
        t[1][c[1] as usize] += 1;
        t[2][c[2] as usize] += 1;
        t[3][c[3] as usize] += 1;
    }
    for &b in rest {
        t[0][b as usize] += 1;
    }
}

impl Deflate {
    /// 段 `level` の圧縮器（zlib の頭を書いた状態）。
    pub fn new(level: Level) -> Self {
        Self::with_base(level, 0)
    }

    /// 入力がおよそ `total` バイトとわかっているときの圧縮器（領域を先に取るだけで、値には効かない）。
    pub fn with_hint(level: Level, total: u64) -> Self {
        let mut d = Self::with_base(level, 0);
        let cap = usize::try_from(total).unwrap_or(usize::MAX);
        d.buf.reserve(cap.saturating_add(PAD).min(BUF_MAX));
        d.bits
            .out
            .reserve((cap / 4).saturating_add(64).min(BUF_MAX));
        d
    }

    /// 絶対位置を `base` から数える圧縮器（試験で 32 ビットの位置が回るところを確かめる。値には効かない）。
    pub(crate) fn with_base(level: Level, base: u64) -> Self {
        let params = Params::of(level);
        #[cfg(test)]
        let params = tune(params, level);
        let mut bits = Bits {
            out: Vec::new(),
            acc: 0,
            n: 0,
        };
        let flg = match level {
            Level::Fast => 0x01,
            Level::Balanced => 0x9C,
            Level::High => 0xDA,
        };
        bits.out.extend_from_slice(&[0x78, flg]);
        Deflate {
            params,
            level,
            buf: Vec::new(),
            end: 0,
            base,
            origin: base,
            pos: 0,
            block_start: 0,
            span: 0,
            head: if params.rle {
                Vec::new()
            } else {
                vec![base as u32; 1 << HASH_BITS]
            },
            prev: if params.rle {
                Vec::new()
            } else {
                vec![base as u32; WINDOW]
            },
            prev_match: (0, 0),
            avail: false,
            syms: Vec::with_capacity(BLOCK_SYMS.max(params.opt_block) + 2),
            lit: [0; 288],
            dist: [0; 32],
            hist: Box::new([[0; 256]; 4]),
            opt: Opt::default(),
            bits,
            adler: 1,
            finished: false,
        }
    }

    /// 段。
    pub fn level(&self) -> Level {
        self.level
    }

    /// 入力を足す（先読みが足りる位置まで解く）。
    pub fn write(&mut self, data: &[u8]) {
        if self.finished {
            return;
        }
        self.adler = adler32_update(self.adler, data);
        for piece in data.chunks(CHUNK) {
            self.buf.extend_from_slice(piece);
            self.end = self.buf.len();
            self.run(false);
        }
    }

    /// 入力を閉じる（残りを解き、最後のブロックと Adler-32 を書く）。
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.buf.resize(self.end + PAD, 0);
        self.run(true);
        if self.avail {
            // 遅延の一致の、まだ書いていない前の位置の文字
            let b = self.buf[self.pos - 1];
            self.push_lit(b);
            self.avail = false;
        }
        self.flush_block(true);
        self.bits.align();
        let a = self.adler.to_be_bytes();
        self.bits.out.extend_from_slice(&a);
        self.buf = Vec::new();
        self.head = Vec::new();
        self.prev = Vec::new();
    }

    /// 書いたバイト（取り出してよい）。
    pub fn output(&mut self) -> &mut Vec<u8> {
        &mut self.bits.out
    }

    /// 段 `level` の圧縮器が入力 `total` バイトで持つ領域の上限（バイト。`with_hint` で作ったとき。書いたバイト
    /// （[`Deflate::output`]）は呼び手が取り出す分なので含めない）：窓（[`BUF_MAX`] か入力の全部）、ハッシュの表、記号、
    /// 頻度の表と、`High` の解き方の作業領域（1 ブロック。位置ごとの候補は鎖の長さまで）。
    pub(crate) fn held_bytes(level: Level, total: u64) -> u64 {
        let p = Params::of(level);
        let buf = (BUF_MAX as u64).min(total.saturating_add(PAD as u64));
        let tables = if p.rle {
            0
        } else {
            ((1u64 << HASH_BITS) + WINDOW as u64) * 4
        };
        let syms = (BLOCK_SYMS.max(p.opt_block) as u64 + 2) * 4;
        let opt = if p.optimal > 0 {
            (p.opt_block as u64).min(total) * (4 * 3 + 4 * u64::from(p.chain)) + 8
        } else {
            0
        };
        buf + tables + syms + 4 * 256 * 4 + opt
    }

    #[inline(always)]
    fn push_lit(&mut self, b: u8) {
        self.syms.push(u32::from(b));
        self.lit[b as usize] += 1;
    }

    #[inline(always)]
    fn push_match(&mut self, len: usize, dist: usize) {
        self.syms
            .push(MATCH_FLAG | ((dist as u32 - 1) << 8) | (len as u32 - 3));
        self.lit[257 + LEN_CODE[len - 3] as usize] += 1;
        self.dist[dist_code(dist as u32 - 1)] += 1;
    }

    /// `buf[a..b]` を文字の並びとして足す。
    fn push_span(&mut self, a: usize, b: usize) {
        let mut a = a;
        while a < b {
            let n = (b - a).min(SPAN_MAX);
            self.syms.push(SPAN_FLAG | n as u32);
            count_bytes(&mut self.hist, &self.buf[a..a + n]);
            a += n;
        }
    }

    /// 解けるところまで解く。`fin` なら論理の終わりまで。
    fn run(&mut self, fin: bool) {
        let limit = if fin {
            self.end
        } else {
            self.end.saturating_sub(LOOKAHEAD)
        };
        if self.params.optimal > 0 {
            // 最適に近い解き方：OPT_BLOCK バイトずつ（揃うまで待つ。終わりは残り）を 1 つのブロックにする。流れの最後の
            // ブロックは `finish` が閉じる
            let block = self.params.opt_block;
            while self.pos < self.end && (fin || self.pos + block <= limit) {
                let n = block.min(self.end - self.pos);
                self.parse_optimal(n);
                if !(fin && self.pos == self.end) {
                    self.flush_block(false);
                }
            }
            if !fin {
                self.slide();
            }
            return;
        }
        while self.pos < limit {
            if self.params.rle {
                self.parse_rle(limit);
            } else {
                self.parse_lazy(limit);
            }
            if self.block_full() {
                self.flush_block(false);
            }
        }
        if !fin {
            self.slide();
        }
    }

    /// ブロックを閉じるか（記号の数か元のバイト数）。
    fn block_full(&self) -> bool {
        self.syms.len() >= self.params.block_syms || self.pos - self.block_start >= BLOCK_RAW
    }

    /// 窓より古く、今のブロックより前の入力を捨てる。
    fn slide(&mut self) {
        let keep = self.pos.saturating_sub(WINDOW).min(self.block_start);
        // 遅延の一致で前の位置の文字がまだなら、それも残す（`block_start` より後なので上で残る）
        if keep >= SLIDE {
            self.buf.drain(..keep);
            self.base += keep as u64;
            self.pos -= keep;
            self.block_start -= keep;
            self.span = self.span.saturating_sub(keep);
            self.end -= keep;
        }
    }

    /// 位置 `p` の 3 バイトのハッシュ。
    #[inline(always)]
    fn hash(&self, p: usize) -> usize {
        let v = u32::from_le_bytes([self.buf[p], self.buf[p + 1], self.buf[p + 2], 0]);
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    /// 位置 `p` を表に入れ、前の先頭（候補の絶対位置）を返す。
    #[inline(always)]
    fn insert(&mut self, p: usize) -> u32 {
        let h = self.hash(p);
        let abs = (self.base + p as u64) as u32;
        let old = self.head[h];
        self.prev[abs as usize & WMASK] = old;
        self.head[h] = abs;
        old
    }

    /// `a` と `b` から一致する長さ（`max` まで）。
    #[inline(always)]
    fn match_len(&self, a: usize, b: usize, max: usize) -> usize {
        let buf = &self.buf;
        let mut k = 0;
        while k < max {
            let x = u64::from_le_bytes(buf[a + k..a + k + 8].try_into().unwrap_or([0; 8]));
            let y = u64::from_le_bytes(buf[b + k..b + k + 8].try_into().unwrap_or([0; 8]));
            let d = x ^ y;
            if d != 0 {
                return (k + (d.trailing_zeros() / 8) as usize).min(max);
            }
            k += 8;
        }
        max
    }

    /// 位置 `p` の最長の一致（`best` より長いものだけ。鎖を `chain` までたどる）。`cand` は鎖の先頭。
    #[inline(always)]
    fn longest(&self, p: usize, cand: u32, chain: u32, best: usize) -> (usize, usize) {
        let max = MAX_MATCH.min(self.end - p);
        let mut best = best;
        let mut best_dist = 0;
        if best >= max {
            return (0, 0);
        }
        let cur = (self.base + p as u64) as u32;
        let mut cand = cand;
        let mut prevd = 0usize;
        let buf = &self.buf;
        for _ in 0..chain {
            let d = cur.wrapping_sub(cand) as usize;
            if d == 0 || d > WINDOW || d <= prevd {
                break;
            }
            prevd = d;
            let c = p - d;
            if buf[c + best] == buf[p + best] && buf[c] == buf[p] && buf[c + 1] == buf[p + 1] {
                let l = self.match_len(c, p, max);
                if l > best {
                    best = l;
                    best_dist = d;
                    if l >= self.params.nice || l >= max {
                        break;
                    }
                }
            }
            cand = self.prev[cand as usize & WMASK];
        }
        if best_dist == 0 {
            (0, 0)
        } else {
            (best, best_dist)
        }
    }

    /// 前のバイトと同じバイトの続きだけを一致（距離 1）にして解く（`Fast`）。続きが [`RUN_MIN`] バイト以上なら一致、
    /// ほかは文字の並び（`SPAN_FLAG`）。続きは位置から 258 バイトまでしか見ない（先読みの中で決まる）。
    fn parse_rle(&mut self, limit: usize) {
        let stop = limit.min(self.block_start + BLOCK_RAW);
        let mut q = self.pos;
        // 文字の並びは呼ぶ切れ目をまたいで続ける（記号の数が入力を渡す切れ目によらないように）
        let mut span = self.span;
        if self.base + q as u64 == self.origin && q < stop {
            // 流れの最初のバイトには前がない
            q += 1;
        }
        while q < stop && self.syms.len() < self.params.block_syms {
            // 位置 q+i から RUN_MIN バイトが前のバイトと同じなら、レーン i が立つ（i = 0〜RUN_STEP−1）。窓は先読みと詰め物の
            // 中にある（q + 16 は論理の終わり + 詰め物より前）
            let Some(w) = self.buf.get(q - 1..q + 17) else {
                break;
            };
            let w: &[u8; 18] = match w.try_into() {
                Ok(w) => w,
                Err(_) => break,
            };
            let e = u128::from(eq_bytes(w, 0)) | (u128::from(eq_bytes(w, 8)) << 64);
            let mut g = e;
            for k in 1..RUN_MIN {
                g &= e >> (8 * k);
            }
            let g = g & RUN_LANES;
            if g == 0 {
                q = (q + RUN_STEP).min(stop);
                continue;
            }
            let c = q + (g.trailing_zeros() / 8) as usize;
            if c >= stop {
                q = stop;
                break;
            }
            let max = MAX_MATCH.min(self.end - c);
            let b = self.buf[c - 1];
            let l = run_len(&self.buf, c, b, max);
            if l >= RUN_MIN {
                if c > span {
                    self.push_span(span, c);
                }
                self.push_match(l, 1);
                // 一致は `stop` を越えてよい
                q = c + l;
                span = q;
            } else {
                q = c + 1;
            }
        }
        self.span = span;
        self.pos = q;
    }

    /// 最適に近い解き方（`High`）：位置 `pos` から `n` バイトのブロックで、各位置の一致の候補（長さごとに最も近い距離）を
    /// 鎖で集め、費用の模型（ビット）で終わりから最小の費用の選びを決める（動的計画法）。費用は、最初は文字の頻度から作った
    /// 符号の長さと固定の長さ・距離の符号の長さで、選んだ記号の頻度から作り直して `optimal` 回解く。一致はブロックの中で
    /// 終わる。費用が同じなら、文字・短い一致の順に先のもの。
    fn parse_optimal(&mut self, n: usize) {
        let pr = self.params;
        let p0 = self.pos;
        let e = p0 + n;
        let mut opt = std::mem::take(&mut self.opt);
        opt.start.clear();
        opt.cands.clear();
        // 位置ごとの候補は鎖の長さまで（領域の見積もりと同じ上限を先に取る。倍々に伸ばさない。空にしたあとなので
        // `reserve_exact` は容量をこの長さ以上にする）
        opt.cands.reserve_exact(n * pr.chain as usize);
        opt.start.reserve_exact(n + 1);
        // 1. 候補を集める（長い一致の中の位置は表に入れるだけで探さない）
        let mut skip_to = p0;
        for i in p0..e {
            opt.start.push(opt.cands.len() as u32);
            if self.end - i < MIN_MATCH {
                continue;
            }
            let cand = self.insert(i);
            if i < skip_to {
                continue;
            }
            let max = MAX_MATCH.min(e - i);
            if max < MIN_MATCH {
                continue;
            }
            let cur = (self.base + i as u64) as u32;
            let mut c_abs = cand;
            let mut prevd = 0usize;
            let mut best = MIN_MATCH - 1;
            for _ in 0..pr.chain {
                let d = cur.wrapping_sub(c_abs) as usize;
                if d == 0 || d > WINDOW || d <= prevd {
                    break;
                }
                prevd = d;
                let c = i - d;
                if self.buf[c + best] == self.buf[i + best]
                    && self.buf[c] == self.buf[i]
                    && self.buf[c + 1] == self.buf[i + 1]
                {
                    let l = self.match_len(c, i, max);
                    if l > best {
                        best = l;
                        opt.cands.push(((l as u32) << 16) | (d as u32 - 1));
                        if l >= pr.nice || l >= max {
                            break;
                        }
                    }
                }
                c_abs = self.prev[c_abs as usize & WMASK];
            }
            if best >= pr.opt_skip {
                skip_to = i + best;
            }
        }
        opt.start.push(opt.cands.len() as u32);
        // 2. 費用（最初の模型）
        let mut lfreq = [0u32; 286];
        for &b in &self.buf[p0..e] {
            lfreq[b as usize] += 1;
        }
        lfreq[256] = 1;
        let mut lcost = [0u32; 286];
        costs_of(&lfreq, 15, &mut lcost);
        for (s, c) in lcost.iter_mut().enumerate().skip(257) {
            *c = u32::from(FIXED_LIT_LENS[s]);
        }
        let mut dcost = [5u32; 30];
        opt.cost.clear();
        opt.cost.reserve_exact(n + 1);
        opt.cost.resize(n + 1, 0);
        opt.choice.clear();
        opt.choice.reserve_exact(n);
        opt.choice.resize(n, 0);
        for pass in 0..pr.optimal {
            let mut len_cost = [0u32; MAX_MATCH + 1];
            for (l, c) in len_cost.iter_mut().enumerate().skip(MIN_MATCH) {
                let code = LEN_CODE[l - 3] as usize;
                *c = lcost[257 + code] + u32::from(LEN_EXTRA[code]);
            }
            let mut dist_cost = [0u32; 30];
            for (code, c) in dist_cost.iter_mut().enumerate() {
                *c = dcost[code] + u32::from(DIST_EXTRA[code]);
            }
            // 3. 終わりから
            opt.cost[n] = 0;
            for j in (0..n).rev() {
                let mut best = lcost[self.buf[p0 + j] as usize] + opt.cost[j + 1];
                let mut choice = 0u32;
                let mut lo = MIN_MATCH;
                for &m in &opt.cands[opt.start[j] as usize..opt.start[j + 1] as usize] {
                    let len = (m >> 16) as usize;
                    let dc = dist_cost[dist_code(m & 0xFFFF)];
                    for (l, &lc) in len_cost.iter().enumerate().take(len + 1).skip(lo) {
                        let c = lc + dc + opt.cost[j + l];
                        if c < best {
                            best = c;
                            choice = ((l as u32) << 16) | (m & 0xFFFF);
                        }
                    }
                    lo = len + 1;
                }
                opt.cost[j] = best;
                opt.choice[j] = choice;
            }
            // 4. 前から選びをたどる（最後の回は記号にする）
            let last = pass + 1 == pr.optimal;
            let mut lf = [0u32; 286];
            let mut df = [0u32; 30];
            let mut j = 0;
            while j < n {
                let ch = opt.choice[j];
                if ch == 0 {
                    let b = self.buf[p0 + j];
                    if last {
                        self.push_lit(b);
                    } else {
                        lf[b as usize] += 1;
                    }
                    j += 1;
                } else {
                    let l = (ch >> 16) as usize;
                    let d = (ch & 0xFFFF) as usize + 1;
                    if last {
                        self.push_match(l, d);
                    } else {
                        lf[257 + LEN_CODE[l - 3] as usize] += 1;
                        df[dist_code(d as u32 - 1)] += 1;
                    }
                    j += l;
                }
            }
            if !last {
                lf[256] = 1;
                costs_of(&lf, 15, &mut lcost);
                costs_of(&df, 15, &mut dcost);
            }
        }
        self.pos = e;
        self.opt = opt;
    }

    /// 遅延の一致で解く（zlib の `deflate_slow` と同じ意味の手順）。
    fn parse_lazy(&mut self, limit: usize) {
        let pr = self.params;
        while self.pos < limit && !self.block_full() {
            let p = self.pos;
            let (plen, pdist) = self.prev_match;
            let mut len = 0;
            let mut dist = 0;
            if self.end - p >= MIN_MATCH {
                let cand = self.insert(p);
                if !self.avail || plen < pr.lazy {
                    let chain = if self.avail && plen >= pr.good {
                        pr.chain >> 2
                    } else {
                        pr.chain
                    };
                    let need = if self.avail {
                        plen.max(MIN_MATCH - 1)
                    } else {
                        MIN_MATCH - 1
                    };
                    (len, dist) = self.longest(p, cand, chain.max(1), need);
                    if len == MIN_MATCH && dist > pr.too_far {
                        len = 0;
                    }
                }
            }
            if self.avail && plen >= MIN_MATCH && len <= plen {
                // 前の位置の一致を使う（p は入れた。一致の中の残りを入れる）
                self.push_match(plen, pdist);
                let stop = (p - 1 + plen).min(self.end.saturating_sub(MIN_MATCH - 1));
                for q in p + 1..stop {
                    self.insert(q);
                }
                self.pos = p - 1 + plen;
                self.avail = false;
                self.prev_match = (0, 0);
            } else if self.avail {
                let b = self.buf[p - 1];
                self.push_lit(b);
                self.prev_match = (len, dist);
                self.pos += 1;
            } else {
                self.avail = true;
                self.prev_match = (len, dist);
                self.pos += 1;
            }
        }
    }

    /// 今のブロックを閉じて書く。
    fn flush_block(&mut self, last: bool) {
        // 遅延の一致で前の位置の文字が未決なら、その位置はまだブロックに入っていない
        let raw_end = if self.avail { self.pos - 1 } else { self.pos };
        if self.params.rle && self.span < raw_end {
            let span = self.span;
            self.push_span(span, raw_end);
        }
        self.span = raw_end;
        let raw_len = raw_end - self.block_start;
        if self.syms.is_empty() && !last {
            return;
        }
        for t in self.hist.iter_mut() {
            for (l, c) in self.lit.iter_mut().zip(t.iter_mut()) {
                *l += *c;
                *c = 0;
            }
        }
        self.lit[256] += 1;
        let dy = self.dynamic();
        let mut extra: u64 = 0;
        for (&f, &x) in self.lit[257..286].iter().zip(&LEN_EXTRA) {
            extra += u64::from(f) * u64::from(x);
        }
        for (&f, &x) in self.dist[..30].iter().zip(&DIST_EXTRA) {
            extra += u64::from(f) * u64::from(x);
        }
        let mut dyn_bits: u64 = 3 + 14 + 3 * dy.hclen as u64;
        for &(s, _) in &dy.items {
            dyn_bits += u64::from(dy.clens[s as usize]) + u64::from(CLEN_EXTRA[s as usize]);
        }
        let mut fixed_bits: u64 = 3;
        for ((&f, &l), &fl) in self.lit[..286].iter().zip(&dy.llens).zip(&FIXED_LIT_LENS) {
            dyn_bits += u64::from(f) * u64::from(l);
            fixed_bits += u64::from(f) * u64::from(fl);
        }
        for (&f, &l) in self.dist[..30].iter().zip(&dy.dlens) {
            dyn_bits += u64::from(f) * u64::from(l);
            fixed_bits += u64::from(f) * 5;
        }
        dyn_bits += extra;
        fixed_bits += extra;
        let stored_bits = self.stored_bits(raw_len);
        let final_bit = u32::from(last);
        if stored_bits < dyn_bits && stored_bits < fixed_bits {
            self.write_stored(raw_len, last);
        } else if fixed_bits < dyn_bits {
            self.bits.put(final_bit | (1 << 1), 3);
            let mut lcodes = [0u16; 288];
            huff_codes(&FIXED_LIT_LENS, &mut lcodes);
            let dlens = [5u8; 32];
            let mut dcodes = [0u16; 32];
            huff_codes(&dlens, &mut dcodes);
            self.write_symbols(&FIXED_LIT_LENS, &lcodes, &dlens, &dcodes);
        } else {
            self.bits.put(final_bit | (2 << 1), 3);
            self.bits.put((dy.hlit - 257) as u32, 5);
            self.bits.put((dy.hdist - 1) as u32, 5);
            self.bits.put((dy.hclen - 4) as u32, 4);
            for &i in CLEN_ORDER.iter().take(dy.hclen) {
                self.bits.put(u32::from(dy.clens[i]), 3);
            }
            for &(s, x) in &dy.items {
                let s = s as usize;
                self.bits
                    .put(u32::from(dy.ccodes[s]), u32::from(dy.clens[s]));
                if CLEN_EXTRA[s] > 0 {
                    self.bits.put(u32::from(x), CLEN_EXTRA[s]);
                }
            }
            self.write_symbols(&dy.llens, &dy.lcodes, &dy.dlens, &dy.dcodes);
        }
        self.syms.clear();
        self.lit = [0; 288];
        self.dist = [0; 32];
        self.block_start = raw_end;
    }

    /// 今の頻度から動的のブロックの表を作る。
    fn dynamic(&self) -> Dynamic {
        let mut dy = Dynamic {
            llens: [0; 288],
            lcodes: [0; 288],
            dlens: [0; 32],
            dcodes: [0; 32],
            hlit: 257,
            hdist: 1,
            hclen: 4,
            clens: [0; 19],
            ccodes: [0; 19],
            items: Vec::new(),
        };
        huff_lengths(
            &self.lit[..286],
            self.params.lit_limit,
            &mut dy.llens[..286],
        );
        huff_lengths(&self.dist[..30], 15, &mut dy.dlens[..30]);
        huff_codes(&dy.llens, &mut dy.lcodes);
        huff_codes(&dy.dlens, &mut dy.dcodes);
        dy.hlit = 257.max(
            dy.llens[..286]
                .iter()
                .rposition(|&l| l > 0)
                .map_or(0, |i| i + 1),
        );
        dy.hdist = 1.max(
            dy.dlens[..30]
                .iter()
                .rposition(|&l| l > 0)
                .map_or(0, |i| i + 1),
        );
        let mut seq = Vec::with_capacity(dy.hlit + dy.hdist);
        seq.extend_from_slice(&dy.llens[..dy.hlit]);
        seq.extend_from_slice(&dy.dlens[..dy.hdist]);
        rle_lengths(&seq, &mut dy.items);
        let mut cfreq = [0u32; 19];
        for &(s, _) in &dy.items {
            cfreq[s as usize] += 1;
        }
        huff_lengths(&cfreq, 7, &mut dy.clens);
        huff_codes(&dy.clens, &mut dy.ccodes);
        dy.hclen = 4.max(
            CLEN_ORDER
                .iter()
                .rposition(|&i| dy.clens[i] > 0)
                .map_or(0, |i| i + 1),
        );
        dy
    }

    /// `len` バイトを stored のブロック（65,535 バイトずつ）で書くビット数（今のビットの位置から）。
    fn stored_bits(&self, len: usize) -> u64 {
        let blocks = len.div_ceil(65_535).max(1) as u64;
        // 最初のブロック：頭の 3 ビットのあとバイトの境目まで詰める
        let first_pad = u64::from((8 - (self.bits.n + 3) % 8) % 8);
        // 2 つ目からは境目から始まるので、頭の 3 ビットと 5 ビットの詰め物
        (3 + first_pad) + (blocks - 1) * 8 + blocks * 32 + 8 * len as u64
    }

    fn write_stored(&mut self, len: usize, last: bool) {
        let start = self.block_start;
        let mut done = 0;
        loop {
            let n = (len - done).min(65_535);
            let fin = last && done + n == len;
            self.bits.put(u32::from(fin), 3);
            self.bits.align();
            let n16 = n as u16;
            self.bits.out.extend_from_slice(&n16.to_le_bytes());
            self.bits.out.extend_from_slice(&(!n16).to_le_bytes());
            self.bits
                .out
                .extend_from_slice(&self.buf[start + done..start + done + n]);
            done += n;
            if done == len {
                break;
            }
        }
    }

    fn write_symbols(&mut self, llens: &[u8], lcodes: &[u16], dlens: &[u8], dcodes: &[u16]) {
        let bits = &mut self.bits;
        // 文字の符号と長さをまとめた表
        let mut lit = [0u32; 256];
        for (i, e) in lit.iter_mut().enumerate() {
            *e = u32::from(lcodes[i]) | (u32::from(llens[i]) << 16);
        }
        let quads = llens[..256].iter().all(|&l| l <= 12);
        let mut raw = self.block_start;
        for &s in &self.syms {
            if s & MATCH_FLAG != 0 {
                let l = (s & 0xFF) as usize;
                let lc = LEN_CODE[l] as usize;
                let x = u32::from(LEN_EXTRA[lc]);
                bits.put(
                    u32::from(lcodes[257 + lc])
                        | ((l as u32 + 3 - u32::from(LEN_BASE[lc])) << llens[257 + lc]),
                    u32::from(llens[257 + lc]) + x,
                );
                let d = (s >> 8) & 0x7FFF;
                let dc = dist_code(d);
                let dx = u32::from(DIST_EXTRA[dc]);
                let dv = d + 1 - u32::from(DIST_BASE[dc]);
                bits.put(
                    u32::from(dcodes[dc]) | (dv << dlens[dc]),
                    u32::from(dlens[dc]) + dx,
                );
                raw += l + 3;
            } else if s & SPAN_FLAG != 0 {
                let n = (s & !SPAN_FLAG) as usize;
                let span = &self.buf[raw..raw + n];
                if quads {
                    // 4 つの文字（12 ビットまで）を 48 ビットにまとめて足す
                    bits.flush_bytes();
                    let (quad, rest) = span.as_chunks::<4>();
                    for p in quad {
                        let a = lit[p[0] as usize];
                        let b = lit[p[1] as usize];
                        let c = lit[p[2] as usize];
                        let d = lit[p[3] as usize];
                        let (la, lb, lc) = (a >> 16, b >> 16, c >> 16);
                        let v = u64::from(a & 0xFFFF)
                            | (u64::from(b & 0xFFFF) << la)
                            | (u64::from(c & 0xFFFF) << (la + lb))
                            | (u64::from(d & 0xFFFF) << (la + lb + lc));
                        bits.put_raw(v, la + lb + lc + (d >> 16));
                        bits.flush_bytes();
                    }
                    for &c in rest {
                        let a = lit[c as usize];
                        bits.put(a & 0xFFFF, a >> 16);
                    }
                } else {
                    let (pairs, rest) = span.as_chunks::<2>();
                    for p in pairs {
                        let a = lit[p[0] as usize];
                        let b = lit[p[1] as usize];
                        let la = a >> 16;
                        bits.put((a & 0xFFFF) | ((b & 0xFFFF) << la), la + (b >> 16));
                    }
                    for &c in rest {
                        let a = lit[c as usize];
                        bits.put(a & 0xFFFF, a >> 16);
                    }
                }
                raw += n;
            } else {
                let b = s as usize;
                bits.put(u32::from(lcodes[b]), u32::from(llens[b]));
                raw += 1;
            }
        }
        let e = 256;
        bits.put(u32::from(lcodes[e]), u32::from(llens[e]));
    }
}

/// 調整の試し（試験だけ。`KURA_DEFLATE_TUNE_<段>="chain=…,nice=…,…"`）。
#[cfg(test)]
#[allow(clippy::disallowed_methods)]
fn tune(mut p: Params, level: Level) -> Params {
    let Ok(t) = std::env::var(format!("KURA_DEFLATE_TUNE_{level:?}").to_uppercase()) else {
        return p;
    };
    for kv in t.split(',') {
        if let Some((k, v)) = kv.split_once('=')
            && let Ok(v) = v.parse::<usize>()
        {
            match k {
                "chain" => p.chain = v as u32,
                "nice" => p.nice = v,
                "good" => p.good = v,
                "lazy" => p.lazy = v,
                "far" => p.too_far = v,
                "passes" => p.optimal = v as u32,
                "block" => p.opt_block = v,
                "skip" => p.opt_skip = v,
                "syms" => p.block_syms = v,
                _ => {}
            }
        }
    }
    p
}

/// 最適に近い解き方の作業領域（ブロックごとに使い回す）。
#[derive(Default)]
struct Opt {
    /// 位置ごとの候補の始まり（`start[j]..start[j + 1]`）。
    start: Vec<u32>,
    /// 候補：（長さ << 16）|（距離 − 1）。長さの短い順で、距離はその長さに届く最も近いもの。
    cands: Vec<u32>,
    /// 位置からブロックの終わりまでの最小の費用（ビット）。
    cost: Vec<u32>,
    /// 位置の選び：0 なら文字、ほかは候補と同じ形の一致。
    choice: Vec<u32>,
}

/// 頻度から、記号の費用（ビット）を作る。使わない記号は上限の長さ。
fn costs_of(freq: &[u32], limit: u32, out: &mut [u32]) {
    let mut lens = vec![0u8; freq.len()];
    huff_lengths(freq, limit, &mut lens);
    for (o, &l) in out.iter_mut().zip(&lens) {
        *o = if l == 0 { limit } else { u32::from(l) };
    }
}

/// `data` を段 `level` の zlib にする。
pub fn deflate_zlib(data: &[u8], level: Level) -> Vec<u8> {
    let mut d = Deflate::new(level);
    d.write(data);
    d.finish();
    std::mem::take(d.output())
}

#[cfg(test)]
mod tests;
