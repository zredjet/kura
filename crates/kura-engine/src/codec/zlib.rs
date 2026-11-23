//! zlib（RFC 1950）・deflate（RFC 1951）の展開と、CRC-32（ISO 3309・PNG のチャンク）・Adler-32（M16 段 6。png クレートの中の
//! fdeflate と、aseprite の miniz_oxide の代わり）。
//!
//! - 展開（[`Inflate`]）は流して読む：入力は要るときに [`Input`] から引き（PNG の IDAT はチャンクに分かれて届く）、出力は
//!   後ろ向きの参照のための 32 KiB の窓を残しながら少しずつ渡す（[`Inflate::fill`]・[`Inflate::ready`]・[`Inflate::consume`]）。
//!   出力の総量に上限を付けられる（超えれば [`ZError::TooLong`]）。中止は呼び手が渡すたびに見る。
//! - 受け付け（RFC 1951 のとおり）：ブロックの型 3、stored の LEN と NLEN の食い違い、長さの符号 286・287、距離の符号 30・31、
//!   出力の頭より前を指す距離、過剰な符号の長さ、文字と長さの符号の不完全な符号（距離の符号は 1 つだけなら長さ 1 の不完全を許す。
//!   zlib と同じ）、end-of-block の符号のない動的ブロック、繰り返しの符号が数を超える・最初の符号が前の長さの繰り返し、は壊れた
//!   流れ。途中で切れた流れは [`ZError::Truncated`]。呼び手の要る受け付けの違い（PNG の甘さ）は [`Options`] で選ぶ。
//! - zlib の頭（CMF・FLG）は方式 8・窓 32 KiB 以下・FCHECK・FDICT なしを確かめ、Adler-32 は [`Options::check_adler`] のとき
//!   だけ照らす。流れの終わりのあとのデータは読まない（呼び手が決める）。
//! - 整数だけで決まるので、どのターゲットでも同じ出力。`unsafe` は使わない。

use std::fmt;

// ---------------------------------------------------------------- CRC-32

/// CRC-32 の表（反射の多項式 0xEDB88320。16 バイトずつ引く slice-by-16 の 16 枚）。
const CRC_TABLES: [[u32; 256]; 16] = {
    let mut t = [[0u32; 256]; 16];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut k = 1;
        while k < 16 {
            let prev = t[k - 1][i];
            t[k][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            k += 1;
        }
        i += 1;
    }
    t
};

/// CRC-32 を `data` で進める（`crc` は前までの値。始めは 0）。
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let t = &CRC_TABLES;
    let mut c = !crc;
    let (chunks, rest) = data.as_chunks::<16>();
    for b in chunks {
        let w0 = c ^ u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let w1 = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
        let w2 = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        let w3 = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
        c = t[15][(w0 & 0xff) as usize]
            ^ t[14][((w0 >> 8) & 0xff) as usize]
            ^ t[13][((w0 >> 16) & 0xff) as usize]
            ^ t[12][(w0 >> 24) as usize]
            ^ t[11][(w1 & 0xff) as usize]
            ^ t[10][((w1 >> 8) & 0xff) as usize]
            ^ t[9][((w1 >> 16) & 0xff) as usize]
            ^ t[8][(w1 >> 24) as usize]
            ^ t[7][(w2 & 0xff) as usize]
            ^ t[6][((w2 >> 8) & 0xff) as usize]
            ^ t[5][((w2 >> 16) & 0xff) as usize]
            ^ t[4][(w2 >> 24) as usize]
            ^ t[3][(w3 & 0xff) as usize]
            ^ t[2][((w3 >> 8) & 0xff) as usize]
            ^ t[1][((w3 >> 16) & 0xff) as usize]
            ^ t[0][(w3 >> 24) as usize];
    }
    for &b in rest {
        c = (c >> 8) ^ t[0][((c ^ u32::from(b)) & 0xff) as usize];
    }
    !c
}

/// `data` の CRC-32。
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

// ---------------------------------------------------------------- Adler-32

/// Adler-32 の法。
const ADLER_MOD: u32 = 65_521;
/// 法を取らずに足せる最大のバイト数（zlib の NMAX）。
const ADLER_NMAX: usize = 5_552;

/// Adler-32 を `data` で進める（`adler` は前までの値。始めは 1）。
pub fn adler32_update(adler: u32, data: &[u8]) -> u32 {
    let (mut a, mut b) = (adler & 0xffff, adler >> 16);
    for block in data.chunks(ADLER_NMAX) {
        for &x in block {
            a += u32::from(x);
            b += a;
        }
        a %= ADLER_MOD;
        b %= ADLER_MOD;
    }
    (b << 16) | a
}

/// `data` の Adler-32。
pub fn adler32(data: &[u8]) -> u32 {
    adler32_update(1, data)
}

// ---------------------------------------------------------------- 展開

/// 展開の誤り（呼び手が自分のコーデックの誤りに写す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZError {
    /// 流れが途中で切れた。
    Truncated,
    /// 流れが壊れている（理由）。
    Corrupt(&'static str),
    /// 出力が上限を超えた。
    TooLong,
    /// Adler-32 が合わない。
    Adler,
    /// 流れが終わったが、出力が求めた大きさに足りない（[`inflate_exact`]）。
    Short,
    /// 置き場を確保できない（バイト）。
    Alloc(usize),
}

impl fmt::Display for ZError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("圧縮したデータが途中で切れています"),
            Self::Corrupt(why) => write!(f, "圧縮したデータが壊れています（{why}）"),
            Self::TooLong => f.write_str("展開したデータが大きすぎます"),
            Self::Adler => f.write_str("圧縮したデータの Adler-32 が合いません"),
            Self::Short => f.write_str("展開したデータの大きさが合いません"),
            Self::Alloc(n) => write!(f, "{n} 個の領域を確保できません"),
        }
    }
}

/// 展開の入力：要るときに呼ばれ、`dst` の後ろに続きのバイトを足す（足した数を返す。0 は入力の終わり）。誤りはそのまま返す。
pub trait Input {
    /// 続きを足す。
    fn more(&mut self, dst: &mut Vec<u8>) -> Result<usize, InputError>;
}

/// 入力の誤り（呼び手の誤りを包んで運ぶ）。
#[derive(Debug)]
pub struct InputError(pub crate::error::EngineError);

/// 1 つのバイト列を一度に渡す入力。
pub struct SliceInput<'a>(Option<&'a [u8]>);

impl<'a> SliceInput<'a> {
    /// `data` を入力にする。
    pub fn new(data: &'a [u8]) -> Self {
        Self(Some(data))
    }
}

impl Input for SliceInput<'_> {
    fn more(&mut self, dst: &mut Vec<u8>) -> Result<usize, InputError> {
        match self.0.take() {
            Some(d) => {
                dst.extend_from_slice(d);
                Ok(d.len())
            }
            None => Ok(0),
        }
    }
}

/// 展開の設定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// zlib の頭と Adler-32 の尾がある（偽なら生の deflate）。
    pub zlib: bool,
    /// Adler-32 を照らす（zlib のときだけ）。
    pub check_adler: bool,
    /// 出力の総量の上限（バイト）。
    pub limit: u64,
    /// 受け付けの細部（[`Flavor`]）。
    pub flavor: Flavor,
}

/// 受け付けの細部（照らす相手の実装に合わせる。どれも正しい流れの値は同じ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// zlib（inftrees.c）：文字と長さ・距離の表の不完全な符号は、1 つだけの長さ 1 の符号と符号のない距離の表だけ許す。固定の
    /// 符号の 286・287 は誤り。
    Zlib,
    /// miniz_oxide：文字と長さ・距離の表の不完全な符号は、使う符号が 1 つ以下なら長さによらず許す。286・287 は誤り（aseprite）。
    Miniz,
    /// fdeflate 0.3.7（png 0.18.1 の中）：文字と長さの表は完全な符号だけ（end-of-block だけの 1 つの符号も誤り）。距離の表は
    /// 1 つだけの長さ 1 の符号と、符号のない表を許す。固定のブロックの 286・287 は end-of-block として読む（続くビットは次の
    /// ブロックの頭）。
    Fdeflate,
}

/// 展開の誤り（入力の誤りを含む）。
#[derive(Debug)]
pub enum Error {
    /// 流れの誤り。
    Z(ZError),
    /// 入力の誤り。
    Input(crate::error::EngineError),
}

impl From<ZError> for Error {
    fn from(e: ZError) -> Self {
        Self::Z(e)
    }
}

impl From<InputError> for Error {
    fn from(e: InputError) -> Self {
        Self::Input(e.0)
    }
}

/// 後ろ向きの参照の窓（RFC 1951）。
const WINDOW: usize = 32 * 1024;
/// 1 つの一致の最大の長さ。
const MAX_MATCH: usize = 258;
/// 文字と長さの表の最初の段のビット数（fdeflate と同じ 12。2 つの文字の符号が収まる項目には 2 文字を入れる）。
const LIT_BITS: u32 = 12;
/// 距離の表の最初の段のビット数。
const DIST_BITS: u32 = 8;
/// 符号の長さの符号の表のビット数（符号は 7 ビットまでなので二段目はない）。
const CLEN_BITS: u32 = 7;

// 表の項目：ビット 0〜4 は消費するビットの数（二段目へ行く項目は一段目のビット数）、8〜10 は種類、12〜15 は追加のビットの数
// （二段目へ行く項目は二段目のビット数）、16〜31 は値（文字・長さや距離の基・二段目の位置）。
// 文字の項目は 0・1（種類 ＋ 1 が書くバイトの数。速い道は分岐せずに書く）。
const K_LIT: u32 = 0;
/// 文字 2 つ（値は 1 つ目 | 2 つ目 << 8、消費するビットは 2 つの和、追加のビットの欄は 1 つ目の符号の長さ）。文字と長さの表だけ。
const K_LIT2: u32 = 1;
const K_LEN: u32 = 2;
const K_EOB: u32 = 3;
const K_SUB: u32 = 4;
const K_BAD: u32 = 5;

#[inline]
const fn entry(len: u32, kind: u32, extra: u32, value: u32) -> u32 {
    len | (kind << 8) | (extra << 12) | (value << 16)
}

#[inline]
fn e_len(e: u32) -> u32 {
    e & 0x1f
}

#[inline]
fn e_kind(e: u32) -> u32 {
    (e >> 8) & 0x7
}

#[inline]
fn e_extra(e: u32) -> u32 {
    (e >> 12) & 0xf
}

#[inline]
fn e_value(e: u32) -> u32 {
    e >> 16
}

/// 長さの符号 257〜285 の基と追加のビット（RFC 1951 §3.2.5）。
const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// 距離の符号 0〜29 の基と追加のビット。
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// 符号の長さの符号の並び（RFC 1951 §3.2.7）。
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// 表の種類。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Alphabet {
    /// 文字と長さ（0〜287）。
    Lit,
    /// 距離（0〜31）。
    Dist,
    /// 符号の長さ（0〜18）。
    Clen,
}

/// 符号 `sym` の表の項目（二段目を除く値）。
fn symbol_entry(alpha: Alphabet, sym: usize, len: u32) -> u32 {
    match alpha {
        Alphabet::Lit => match sym {
            0..=255 => entry(len, K_LIT, 0, sym as u32),
            256 => entry(len, K_EOB, 0, 0),
            257..=285 => entry(
                len,
                K_LEN,
                u32::from(LEN_EXTRA[sym - 257]),
                u32::from(LEN_BASE[sym - 257]),
            ),
            _ => entry(len, K_BAD, 0, 0),
        },
        Alphabet::Dist => match sym {
            0..=29 => entry(
                len,
                K_LEN,
                u32::from(DIST_EXTRA[sym]),
                u32::from(DIST_BASE[sym]),
            ),
            _ => entry(len, K_BAD, 0, 0),
        },
        Alphabet::Clen => entry(len, K_LIT, 0, sym as u32),
    }
}

/// `n` ビットの `code` を反転する。
#[inline]
fn reverse(code: u32, n: u32) -> u32 {
    code.reverse_bits() >> (32 - n)
}

/// 符号の長さ `lens` から、最初の段が `bits` ビットの表を `table` に作る（RFC 1951 §3.2.2 の正規の符号。符号は下位のビットから
/// 読むので反転して引く）。過剰な符号は誤り。不完全な符号をどこまで許すかは `flavor`（[`Flavor`]）。使われない側の符号を読めば
/// 誤り。`fixed` は固定のブロックの表（fdeflate は 286・287 を end-of-block にする）。
fn build(
    table: &mut Vec<u32>,
    lens: &[u8],
    bits: u32,
    alpha: Alphabet,
    flavor: Flavor,
    fixed: bool,
) -> Result<(), ZError> {
    let mut count = [0u32; 16];
    for &l in lens {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    // 過剰か不完全か
    let mut left: i64 = 1;
    for c in &count[1..] {
        left = (left << 1) - i64::from(*c);
        if left < 0 {
            return Err(ZError::Corrupt("符号の長さが多すぎます"));
        }
    }
    let used: u32 = count.iter().sum();
    if left > 0 {
        let single = used == 1 && (count[1] == 1 || flavor == Flavor::Miniz);
        let ok = match (alpha, flavor) {
            (Alphabet::Clen, _) => false,
            (Alphabet::Lit, Flavor::Fdeflate) => false,
            (Alphabet::Lit, _) => single,
            (Alphabet::Dist, _) => single || used == 0,
        };
        if !ok {
            return Err(ZError::Corrupt("符号が不完全です"));
        }
    }
    let mut next = [0u32; 16];
    let mut code = 0u32;
    for l in 1..16 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    // 最初の段（使われない項目は誤り。完全な符号はどの項目も符号か二段目で埋まるので、先に誤りで埋めない）と、長い符号の
    // 二段目の大きさ
    let size = 1usize << bits;
    table.truncate(size);
    if left > 0 {
        table.clear();
    }
    table.resize(size, entry(0, K_BAD, 0, 0));
    let mut codes = [0u32; 288];
    for (sym, &l) in lens.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let l = u32::from(l);
        let c = next[l as usize];
        next[l as usize] += 1;
        codes[sym] = reverse(c, l);
    }
    if count[(bits as usize + 1)..].iter().any(|&c| c > 0) {
        // 長い符号があるときだけ、前置きごとの二段目のビット数を数える
        let mut sub_bits = vec![0u32; size];
        for (sym, &l) in lens.iter().enumerate() {
            let l = u32::from(l);
            if l > bits {
                let prefix = (codes[sym] & ((1 << bits) - 1)) as usize;
                sub_bits[prefix] = sub_bits[prefix].max(l - bits);
            }
        }
        for (prefix, &sb) in sub_bits.iter().enumerate() {
            if sb > 0 {
                let at = table.len() as u32;
                table[prefix] = entry(bits, K_SUB, sb, at);
                table.resize(table.len() + (1 << sb), entry(0, K_BAD, 0, 0));
            }
        }
    }
    fill_symbols(table, lens, &codes, bits, alpha, flavor, fixed, size);
    if alpha == Alphabet::Lit {
        pair_literals(table, bits);
    }
    Ok(())
}

/// 表の項目を符号ごとに埋める（[`build`] の続き）。
#[allow(clippy::too_many_arguments)]
fn fill_symbols(
    table: &mut [u32],
    lens: &[u8],
    codes: &[u32; 288],
    bits: u32,
    alpha: Alphabet,
    flavor: Flavor,
    fixed: bool,
    size: usize,
) {
    for (sym, &l) in lens.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let l = u32::from(l);
        let rc = codes[sym];
        let e = if fixed && flavor == Flavor::Fdeflate && alpha == Alphabet::Lit && sym >= 286 {
            entry(l, K_EOB, 0, 0)
        } else {
            symbol_entry(alpha, sym, l)
        };
        if l <= bits {
            let mut i = rc as usize;
            while i < size {
                table[i] = e;
                i += 1 << l;
            }
        } else {
            let prefix = (rc & ((1 << bits) - 1)) as usize;
            let p = table[prefix];
            let (at, sb) = (e_value(p) as usize, e_extra(p));
            let mut i = (rc >> bits) as usize;
            while i < (1usize << sb) {
                table[at + i] = e;
                i += 1 << (l - bits);
            }
        }
    }
}

/// 最初の段の文字の項目のうち、続く符号も文字で、2 つの符号の長さの和が最初の段のビット数に収まるもの（続く符号が最初の段の
/// 分かっているビットだけで決まる）を、文字 2 つの項目にする（読む値は 1 つずつ読むのと同じ）。
fn pair_literals(table: &mut [u32], bits: u32) {
    let size = 1usize << bits;
    // 後ろから回す：2 つ目を引く位置 `i >> l1` は `i` より前（`i` が 0 なら同じ）なので、まだ書き換えていない 1 文字の項目を引く
    for i in (0..size).rev() {
        let e = table[i];
        if e_kind(e) != K_LIT {
            continue;
        }
        let l1 = e_len(e);
        if l1 >= bits {
            continue;
        }
        let e2 = table[i >> l1];
        if e_kind(e2) == K_LIT && l1 + e_len(e2) <= bits {
            table[i] = entry(l1 + e_len(e2), K_LIT2, l1, e_value(e) | (e_value(e2) << 8));
        }
    }
}

/// 展開の段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// zlib の頭の前。
    Header,
    /// ブロックの頭の前。
    Block,
    /// stored のブロックの残り。
    Stored(u32),
    /// ハフマンのブロックの中。
    Codes,
    /// 一致の写しの残り（長さ・距離）。
    Copy(u32, u32),
    /// 最後のブロックのあと（zlib の尾の前）。
    Trailer,
    /// 終わり。
    Done,
}

/// 流して展開する読み手。
pub struct Inflate<I: Input> {
    input: I,
    opts: Options,
    /// 入力の置き場と、次にビットへ積む位置。
    inbuf: Vec<u8>,
    pos: usize,
    /// 入力の終わりに来た。
    eof: bool,
    /// 出力の総量の見込み（置き場をこれより大きく取らない。速さのためだけで値には効かない）。
    hint: Option<u64>,
    /// 入力から受け取ったバイトの総数。
    in_total: u64,
    /// ビットの置き場（下位から読む）と、その中の数。
    bits: u64,
    nbits: u32,
    /// 入力の終わりのあとに積んだ 0 のバイトの数（切れた流れを見つける）。
    pad: u32,
    /// 出力の置き場：[0, r) は渡し済み（窓）、[r, w) はまだ渡していない、[w, ..) は空き。
    out: Vec<u8>,
    r: usize,
    w: usize,
    /// 書いた総量。
    total: u64,
    state: State,
    last: bool,
    adler: u32,
    /// Adler-32 に足し終えた出力の位置（`out` の中）。
    adler_at: usize,
    lit: Vec<u32>,
    dist: Vec<u32>,
    /// 符号の長さの符号の表（7 ビット）。
    clen: Vec<u32>,
    /// `lit`・`dist` が固定のブロックの表のままか（続く固定のブロックで作り直さない。fdeflate と同じ）。
    fixed_tables: bool,
    /// 表を作った回数（試験で、固定の表を作り直さないことを確かめる）。
    #[cfg(test)]
    builds: usize,
}

/// 入力の置き場を詰める大きさ（読み終えた分がこれを超えたら詰める）。
const INPUT_CHUNK: usize = 64 * 1024;

impl<I: Input> Inflate<I> {
    /// `input` を `opts` で展開する読み手（まだ何も読まない）。
    pub fn new(input: I, opts: Options) -> Self {
        Self {
            input,
            opts,
            inbuf: Vec::new(),
            pos: 0,
            eof: false,
            hint: None,
            in_total: 0,
            bits: 0,
            nbits: 0,
            pad: 0,
            out: Vec::new(),
            r: 0,
            w: 0,
            total: 0,
            state: if opts.zlib {
                State::Header
            } else {
                State::Block
            },
            last: false,
            adler: 1,
            adler_at: 0,
            lit: Vec::new(),
            dist: Vec::new(),
            clen: Vec::new(),
            fixed_tables: false,
            #[cfg(test)]
            builds: 0,
        }
    }

    /// 展開に使った入力のビットの数（先読みして積んだだけのものは数えない）。
    pub fn consumed_bits(&self) -> u64 {
        let unread = (self.inbuf.len() - self.pos) as u64;
        let held = u64::from(self.nbits).saturating_sub(u64::from(self.pad) * 8);
        (self.in_total - unread) * 8 - held
    }

    /// 出力の総量の見込みを伝える（置き場をそれより大きく取らず、その終わりまでは求めた分より先に展開する。見込みより先は
    /// 求められたときだけ読む。値には効かない）。
    pub fn set_size_hint(&mut self, total: u64) {
        self.hint = Some(total);
    }

    /// 入力（読み手の状態を見る・進める）。
    pub fn input_mut(&mut self) -> &mut I {
        &mut self.input
    }

    /// 同じ入力で新しい流れを始める（読み置いた入力・出力・状態を捨てる。APNG のフレームごとの流れ）。
    pub fn restart(&mut self) {
        self.inbuf.clear();
        self.pos = 0;
        self.eof = false;
        self.in_total = 0;
        self.bits = 0;
        self.nbits = 0;
        self.pad = 0;
        self.r = 0;
        self.w = 0;
        self.total = 0;
        self.state = if self.opts.zlib {
            State::Header
        } else {
            State::Block
        };
        self.last = false;
        self.adler = 1;
        self.adler_at = 0;
    }

    /// まだ渡していない出力。
    pub fn ready(&self) -> &[u8] {
        &self.out[self.r..self.w]
    }

    /// まだ渡していない出力のうち、頭の `n` バイトを渡したことにする。
    pub fn consume(&mut self, n: usize) {
        self.r = (self.r + n).min(self.w);
    }

    /// 流れの終わり（zlib なら尾まで）まで読んだか。
    pub fn finished(&self) -> bool {
        self.state == State::Done
    }

    /// 書いた総量（バイト）。
    pub fn total_out(&self) -> u64 {
        self.total
    }

    /// 流れの終わりのあとに残った入力（読み手の置き場にあるもの。ビットの置き場の分を含む）のバイト数。終わっていなければ 0。
    pub fn leftover(&self) -> usize {
        if self.state != State::Done {
            return 0;
        }
        (self.inbuf.len() - self.pos)
            + ((self.nbits / 8) as usize).saturating_sub(self.pad as usize)
    }

    /// まだ渡していない出力が `want` バイト以上になるか、流れが終わるまで展開する。まだ渡していない出力のバイト数を返す。
    pub fn fill(&mut self, want: usize) -> Result<usize, Error> {
        // 窓 ＋ 求める分 ＋ 詰める回数を減らす余り（詰めるたびに窓の 32 KiB を写すので、その 8 倍ほど書いてから詰める）。総量の
        // 見込みがあれば、それ（と一致のはみ出しの分）より大きくしない
        let mut need = WINDOW
            .saturating_add(want.saturating_mul(2).max(256 * 1024))
            .saturating_add(2 * MAX_MATCH);
        if let Some(h) = self.hint {
            let cap = usize::try_from(h)
                .unwrap_or(usize::MAX)
                .saturating_add(want)
                .saturating_add(2 * MAX_MATCH + 32);
            need = need.min(cap);
        }
        if self.out.len() < need {
            self.grow_to(need)?;
        }
        while self.w - self.r < want && self.state != State::Done {
            if self.out.len() - self.w < MAX_MATCH {
                self.compact();
                // 詰めても空きがなければ（見込みより先を求めた）置き場を伸ばす（必ず進む）
                if self.out.len() - self.w < MAX_MATCH {
                    let more = self.out.len().max(4 * MAX_MATCH);
                    self.grow_to(self.out.len().saturating_add(more))?;
                }
            }
            // 要る分を出したら先を読まない（PNG：最後の行のあとの流れは読まない）。総量の見込みがあれば、その終わりまでは先に
            // 展開してよい（置き場の空きのかぎり。行ごとに止めない）
            let mut goal = self.r + want;
            if let Some(h) = self.hint {
                let end = (self.w as u64).saturating_add(h.saturating_sub(self.total));
                goal = goal.max(end.min(self.out.len() as u64) as usize);
            }
            self.step(goal)?;
        }
        Ok(self.w - self.r)
    }

    /// 出力の置き場を `len` バイトにする（確保できなければ誤り）。
    fn grow_to(&mut self, len: usize) -> Result<(), ZError> {
        let extra = len.saturating_sub(self.out.len());
        self.out
            .try_reserve_exact(extra)
            .map_err(|_| ZError::Alloc(len))?;
        self.out.resize(len, 0);
        Ok(())
    }

    /// 渡し済みの出力のうち、窓より前のものを捨てて詰める。
    fn compact(&mut self) {
        self.flush_adler();
        let drop = self.r.min(self.w.saturating_sub(WINDOW));
        if drop == 0 {
            return;
        }
        self.out.copy_within(drop..self.w, 0);
        self.r -= drop;
        self.w -= drop;
        self.adler_at -= drop;
    }

    /// 書いた出力を Adler-32 に足す。
    fn flush_adler(&mut self) {
        if self.opts.zlib && self.opts.check_adler && self.adler_at < self.w {
            self.adler = adler32_update(self.adler, &self.out[self.adler_at..self.w]);
        }
        self.adler_at = self.w;
    }

    /// ビットの置き場に 56 ビット以上積む（入力の終わりのあとは 0 を積み、その数を数える）。
    #[inline]
    fn refill(&mut self) -> Result<(), Error> {
        if self.pos + 8 <= self.inbuf.len() {
            let b = &self.inbuf[self.pos..self.pos + 8];
            let word = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            self.bits |= word << self.nbits;
            let take = (63 - self.nbits) >> 3;
            self.pos += take as usize;
            self.nbits |= 56;
            return Ok(());
        }
        self.refill_slow()
    }

    fn refill_slow(&mut self) -> Result<(), Error> {
        // 残りが少なければ先に続きを取りに行く（チャンクに分かれた入力の境目でも、速く積めるように）
        while !self.eof && self.inbuf.len() - self.pos < 16 {
            if self.pos > 0 {
                self.inbuf.drain(..self.pos);
                self.pos = 0;
            }
            let got = self.input.more(&mut self.inbuf)?;
            self.in_total += got as u64;
            if got == 0 {
                self.eof = true;
            }
        }
        if self.pos + 8 <= self.inbuf.len() {
            return self.refill();
        }
        while self.nbits <= 56 {
            if self.pos < self.inbuf.len() {
                self.bits |= u64::from(self.inbuf[self.pos]) << self.nbits;
                self.pos += 1;
                self.nbits += 8;
            } else if !self.eof {
                if self.pos > INPUT_CHUNK || self.pos == self.inbuf.len() {
                    self.inbuf.drain(..self.pos);
                    self.pos = 0;
                }
                let got = self.input.more(&mut self.inbuf)?;
                self.in_total += got as u64;
                if got == 0 {
                    self.eof = true;
                }
                if self.pos + 8 <= self.inbuf.len() {
                    return self.refill();
                }
            } else {
                self.pad += 1;
                self.nbits += 8;
            }
        }
        Ok(())
    }

    /// `n` ビットを読み捨てる（積んである前提）。入力の終わりのあとの 0 まで読んだら切れた流れ。
    #[inline]
    fn drop_bits(&mut self, n: u32) -> Result<(), ZError> {
        self.bits >>= n;
        self.nbits -= n;
        if self.pad > 0 && self.nbits < self.pad * 8 {
            return Err(ZError::Truncated);
        }
        Ok(())
    }

    /// `n`（0〜16）ビットを読む。
    #[inline]
    fn take(&mut self, n: u32) -> Result<u32, Error> {
        if self.nbits < n {
            self.refill()?;
        }
        let v = (self.bits & ((1u64 << n) - 1)) as u32;
        self.drop_bits(n)?;
        Ok(v)
    }

    /// 表 `table`（最初の段 `bits` ビット）で符号を 1 つ読み、その項目を返す（積んである前提）。
    #[inline]
    fn decode(&mut self, which: Alphabet) -> Result<u32, Error> {
        if self.nbits < 32 {
            self.refill()?;
        }
        let (table, bits) = match which {
            Alphabet::Dist => (&self.dist, DIST_BITS),
            Alphabet::Clen => (&self.clen, CLEN_BITS),
            Alphabet::Lit => (&self.lit, LIT_BITS),
        };
        let mut e = table[(self.bits & ((1u64 << bits) - 1)) as usize];
        if e_kind(e) == K_SUB {
            let sub = (self.bits >> bits) as u32 & ((1u32 << e_extra(e)) - 1);
            e = table[(e_value(e) + sub) as usize];
        }
        if e_kind(e) == K_BAD {
            return Err(ZError::Corrupt("使われない符号です").into());
        }
        if e_kind(e) == K_LIT2 {
            // ふつうの道は 1 文字ずつ（1 つ目だけを読む）
            self.drop_bits(e_extra(e))?;
            return Ok(entry(e_extra(e), K_LIT, 0, e_value(e) & 0xff));
        }
        self.drop_bits(e_len(e))?;
        Ok(e)
    }

    /// 段を 1 つ進める（ブロックの頭・stored の写し・ハフマンの符号を空きのあるかぎり・尾）。
    fn step(&mut self, goal: usize) -> Result<(), Error> {
        match self.state {
            State::Header => {
                let cmf = self.take(8)?;
                let flg = self.take(8)?;
                if cmf & 0x0f != 8 {
                    return Err(ZError::Corrupt("zlib の方式が 8 ではありません").into());
                }
                if cmf >> 4 > 7 {
                    return Err(ZError::Corrupt("zlib の窓が大きすぎます").into());
                }
                if ((cmf << 8) | flg) % 31 != 0 {
                    return Err(ZError::Corrupt("zlib の頭の検査が合いません").into());
                }
                if flg & 0x20 != 0 {
                    return Err(ZError::Corrupt("zlib の辞書は使えません").into());
                }
                self.state = State::Block;
            }
            State::Block => self.block_header()?,
            State::Stored(left) => self.stored(left, goal)?,
            State::Codes => self.codes(goal)?,
            State::Copy(len, dist) => {
                self.state = State::Codes;
                self.copy(len, dist)?;
            }
            State::Trailer => {
                // バイトの境目まで読み捨て、Adler-32（ビッグエンディアン）
                let n = self.nbits % 8;
                self.take(n)?;
                let mut v = 0u32;
                for _ in 0..4 {
                    v = (v << 8) | self.take(8)?;
                }
                self.flush_adler();
                if self.opts.check_adler && v != self.adler {
                    return Err(ZError::Adler.into());
                }
                self.state = State::Done;
            }
            State::Done => {}
        }
        Ok(())
    }

    fn block_header(&mut self) -> Result<(), Error> {
        if self.last {
            self.state = if self.opts.zlib {
                State::Trailer
            } else {
                State::Done
            };
            return Ok(());
        }
        let h = self.take(3)?;
        self.last = h & 1 != 0;
        match h >> 1 {
            0 => {
                let n = self.nbits % 8;
                self.take(n)?;
                let len = self.take(16)?;
                let nlen = self.take(16)?;
                if len != !nlen & 0xffff {
                    return Err(ZError::Corrupt("stored の長さが食い違います").into());
                }
                self.state = State::Stored(len);
            }
            1 => {
                // 直前も固定のブロックなら表はそのまま（同じ表。空の固定のブロックを並べた流れで作り直さない）
                if !self.fixed_tables {
                    let mut lens = [0u8; 320];
                    lens[..144].fill(8);
                    lens[144..256].fill(9);
                    lens[256..280].fill(7);
                    lens[280..288].fill(8);
                    lens[288..320].fill(5);
                    let f = self.opts.flavor;
                    build(
                        &mut self.lit,
                        &lens[..288],
                        LIT_BITS,
                        Alphabet::Lit,
                        f,
                        true,
                    )?;
                    build(
                        &mut self.dist,
                        &lens[288..],
                        DIST_BITS,
                        Alphabet::Dist,
                        f,
                        true,
                    )?;
                    self.fixed_tables = true;
                    #[cfg(test)]
                    {
                        self.builds += 1;
                    }
                }
                self.state = State::Codes;
            }
            2 => {
                self.fixed_tables = false;
                self.dynamic()?;
                #[cfg(test)]
                {
                    self.builds += 1;
                }
                self.state = State::Codes;
            }
            _ => return Err(ZError::Corrupt("ブロックの型 3 は使えません").into()),
        }
        Ok(())
    }

    /// 動的ハフマンのブロックの表を読む（RFC 1951 §3.2.7）。
    fn dynamic(&mut self) -> Result<(), Error> {
        let hlit = self.take(5)? as usize + 257;
        let hdist = self.take(5)? as usize + 1;
        let hclen = self.take(4)? as usize + 4;
        if hlit > 286 || hdist > 30 {
            return Err(ZError::Corrupt("符号の数が多すぎます").into());
        }
        let mut clen = [0u8; 19];
        for &i in &CLEN_ORDER[..hclen] {
            clen[i] = self.take(3)? as u8;
        }
        build(
            &mut self.clen,
            &clen,
            CLEN_BITS,
            Alphabet::Clen,
            self.opts.flavor,
            false,
        )?;
        let mut lens = [0u8; 286 + 30];
        let total = hlit + hdist;
        let mut i = 0;
        while i < total {
            let e = self.decode(Alphabet::Clen)?;
            let sym = e_value(e);
            match sym {
                0..=15 => {
                    lens[i] = sym as u8;
                    i += 1;
                }
                16 => {
                    if i == 0 {
                        return Err(ZError::Corrupt("最初の符号の長さが繰り返しです").into());
                    }
                    let prev = lens[i - 1];
                    let n = 3 + self.take(2)? as usize;
                    if i + n > total {
                        return Err(ZError::Corrupt("符号の長さが多すぎます").into());
                    }
                    lens[i..i + n].fill(prev);
                    i += n;
                }
                _ => {
                    let n = if sym == 17 {
                        3 + self.take(3)? as usize
                    } else {
                        11 + self.take(7)? as usize
                    };
                    if i + n > total {
                        return Err(ZError::Corrupt("符号の長さが多すぎます").into());
                    }
                    lens[i..i + n].fill(0);
                    i += n;
                }
            }
        }
        if lens[256] == 0 {
            return Err(ZError::Corrupt("end-of-block の符号がありません").into());
        }
        let f = self.opts.flavor;
        build(
            &mut self.lit,
            &lens[..hlit],
            LIT_BITS,
            Alphabet::Lit,
            f,
            false,
        )?;
        build(
            &mut self.dist,
            &lens[hlit..total],
            DIST_BITS,
            Alphabet::Dist,
            f,
            false,
        )?;
        Ok(())
    }

    /// stored のブロックを空きのあるかぎり写す。
    fn stored(&mut self, mut left: u32, goal: usize) -> Result<(), Error> {
        while left > 0 {
            if self.w >= goal {
                self.state = State::Stored(left);
                return Ok(());
            }
            let room = (self.out.len() - self.w)
                .min(left as usize)
                .min(goal - self.w);
            if room == 0 {
                self.state = State::Stored(left);
                return Ok(());
            }
            // 先にビットの置き場のバイトを使う
            if self.nbits >= 8 {
                let b = self.take(8)? as u8;
                self.put_byte(b)?;
                left -= 1;
                continue;
            }
            // ビットの置き場は空（先読みしたビットは入力の置き場から直に写すので捨てる）
            self.bits = 0;
            if self.pos == self.inbuf.len() {
                if self.eof {
                    return Err(ZError::Truncated.into());
                }
                let got = self.input.more(&mut self.inbuf)?;
                self.in_total += got as u64;
                if got == 0 {
                    self.eof = true;
                    continue;
                }
            }
            let n = room.min(self.inbuf.len() - self.pos);
            self.total += n as u64;
            if self.total > self.opts.limit {
                return Err(ZError::TooLong.into());
            }
            self.out[self.w..self.w + n].copy_from_slice(&self.inbuf[self.pos..self.pos + n]);
            self.w += n;
            self.pos += n;
            left -= n as u32;
            if self.pos > INPUT_CHUNK {
                self.inbuf.drain(..self.pos);
                self.pos = 0;
            }
        }
        self.state = State::Block;
        Ok(())
    }

    #[inline]
    fn put_byte(&mut self, b: u8) -> Result<(), ZError> {
        self.total += 1;
        if self.total > self.opts.limit {
            return Err(ZError::TooLong);
        }
        self.out[self.w] = b;
        self.w += 1;
        Ok(())
    }

    /// ハフマンの符号を、出力の空き（1 つの一致の最大の長さ）があるかぎり読む。入力と出力が十分あるあいだは速い道（[`Self::fast`]）。
    fn codes(&mut self, goal: usize) -> Result<(), Error> {
        while self.out.len() - self.w >= MAX_MATCH && self.w < goal {
            // 入力が 8 バイト以上あれば速い道（遅い道で 1 つ読むたびに戻れるか見る）
            if self.pad == 0 && self.pos + 8 <= self.inbuf.len() {
                if self.fast(goal)? {
                    self.state = State::Block;
                    return Ok(());
                }
                if !(self.out.len() - self.w >= MAX_MATCH && self.w < goal) {
                    return Ok(());
                }
            }
            let e = self.decode(Alphabet::Lit)?;
            match e_kind(e) {
                K_LIT => self.put_byte(e_value(e) as u8)?,
                K_LEN => {
                    let len = e_value(e) + self.take(e_extra(e))?;
                    let d = self.decode(Alphabet::Dist)?;
                    let dist = e_value(d) + self.take(e_extra(d))?;
                    self.copy(len, dist)?;
                }
                _ => {
                    // end-of-block
                    self.state = State::Block;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// 速い道：入力に 8 バイト以上（積み足しが 1 回の読みで済む）、出力に一致の最大より広い空き、上限までの余りがあるあいだ、ビットの
    /// 置き場と位置を手元に持って読む。1 回の積み足し（56 ビット以上）で文字の項目（1 つ 15 ビットまで）を 3 つまで続けて読み、
    /// 長さと距離（合わせて 48 ビットまで）の前で足りなければ積み足す。end-of-block まで読めば真。条件が崩れたらふつうの道に戻す
    /// （偽）。値と受け付けはふつうの道と同じ。
    fn fast(&mut self, goal: usize) -> Result<bool, Error> {
        let Self {
            inbuf,
            pos,
            bits,
            nbits,
            out,
            w,
            total,
            lit,
            dist,
            opts,
            ..
        } = self;
        let inp = &inbuf[..];
        let (lit, dist) = (&lit[..], &dist[..]);
        let mut b = *bits;
        let mut n = *nbits;
        let mut p = *pos;
        let mut o = *w;
        let start = o;
        // 上限までに書けるバイト（出力の位置で）
        let left = opts.limit.saturating_sub(*total);
        let lim = start.saturating_add(usize::try_from(left).unwrap_or(usize::MAX));
        let room = out.len().saturating_sub(MAX_MATCH).min(goal);
        // 文字を続けて書いてよい終わり（3 つの 2 文字の項目の分を残す）
        let lit_end = room.min(lim).saturating_sub(6);
        let lit_mask = (1u64 << LIT_BITS) - 1;
        let dist_mask = (1u64 << DIST_BITS) - 1;
        macro_rules! refill {
            () => {{
                let c = &inp[p..p + 8];
                b |= u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]) << n;
                p += ((63 - n) >> 3) as usize;
                n |= 56;
            }};
        }
        let mut eob = false;
        let r: Result<(), ZError> = 'outer: loop {
            if o >= room || p + 8 > inp.len() {
                break Ok(());
            }
            refill!();
            let mut e = lit[(b & lit_mask) as usize];
            // 文字の項目を 3 つまで（積み足した 56 ビットで足りる）
            if o < lit_end {
                let mut k = 0;
                while k < 3 {
                    let kind = e_kind(e);
                    if kind > K_LIT2 {
                        break;
                    }
                    // 1 文字でも 2 バイト書き（2 つ目は次に上書きされる）、文字の数だけ進める
                    let v = e_value(e);
                    out[o] = v as u8;
                    out[o + 1] = (v >> 8) as u8;
                    o += kind as usize + 1;
                    let l = e_len(e);
                    b >>= l;
                    n -= l;
                    e = lit[(b & lit_mask) as usize];
                    k += 1;
                }
                if k > 0 {
                    continue 'outer;
                }
            }
            // 文字以外（か終わりに近い）：長さと距離の分のビットを積む
            if n < 48 {
                if p + 8 > inp.len() {
                    break Ok(());
                }
                refill!();
                e = lit[(b & lit_mask) as usize];
            }
            if e_kind(e) == K_LIT2 {
                if o + 2 <= room && o + 2 <= lim {
                    let l = e_len(e);
                    b >>= l;
                    n -= l;
                    let v = e_value(e);
                    out[o] = v as u8;
                    out[o + 1] = (v >> 8) as u8;
                    o += 2;
                    continue;
                }
                // 要る分（上限）が 1 文字しかなければ 1 つ目だけ
                e = entry(e_extra(e), K_LIT, 0, e_value(e) & 0xff);
            }
            if e_kind(e) == K_SUB {
                let sub = (b >> LIT_BITS) as u32 & ((1u32 << e_extra(e)) - 1);
                e = lit[(e_value(e) + sub) as usize];
            }
            let kind = e_kind(e);
            let l = e_len(e);
            if kind == K_LIT {
                if o >= lim {
                    break Err(ZError::TooLong);
                }
                b >>= l;
                n -= l;
                out[o] = e_value(e) as u8;
                o += 1;
                continue;
            }
            if kind == K_EOB {
                b >>= l;
                n -= l;
                eob = true;
                break Ok(());
            }
            if kind != K_LEN {
                break Err(ZError::Corrupt("使われない符号です"));
            }
            b >>= l;
            n -= l;
            let x = e_extra(e);
            let len = (e_value(e) + (b as u32 & ((1u32 << x) - 1))) as usize;
            b >>= x;
            n -= x;
            let mut d = dist[(b & dist_mask) as usize];
            if e_kind(d) == K_SUB {
                let sub = (b >> DIST_BITS) as u32 & ((1u32 << e_extra(d)) - 1);
                d = dist[(e_value(d) + sub) as usize];
            }
            if e_kind(d) != K_LEN {
                break Err(ZError::Corrupt("使われない符号です"));
            }
            let dl = e_len(d);
            b >>= dl;
            n -= dl;
            let dx = e_extra(d);
            let dd = (e_value(d) + (b as u32 & ((1u32 << dx) - 1))) as usize;
            b >>= dx;
            n -= dx;
            let written = *total + (o - start) as u64;
            if dd as u64 > written {
                break Err(ZError::Corrupt("距離が出力の頭より前を指しています"));
            }
            if o + len > lim {
                break Err(ZError::TooLong);
            }
            let src = o - dd;
            // 写し：距離が 16（8）以上なら 16（8）バイトずつ（どのかたまりも書き先より前から読むので重ならない。末尾は空きに
            // 15 バイトまではみ出して書くが、`o` は `len` だけ進めるので値は同じ）
            if dd >= 16 && o + len + 16 <= out.len() {
                let mut i = 0;
                while i < len {
                    out.copy_within(src + i..src + i + 16, o + i);
                    i += 16;
                }
            } else if dd >= 8 && o + len + 8 <= out.len() {
                let mut i = 0;
                while i < len {
                    out.copy_within(src + i..src + i + 8, o + i);
                    i += 8;
                }
            } else if dd >= len {
                out.copy_within(src..src + len, o);
            } else if dd == 1 {
                let v = out[src];
                out[o..o + len].fill(v);
            } else {
                out.copy_within(src..src + dd, o);
                let mut q = dd;
                while q < len {
                    let k = q.min(len - q);
                    out.copy_within(o..o + k, o + q);
                    q += k;
                }
            }
            o += len;
        };
        *bits = b;
        *nbits = n;
        *pos = p;
        *total += (o - start) as u64;
        *w = o;
        r?;
        Ok(eob)
    }

    /// 距離 `dist` の `len` バイトを写す（空きが足りなければ残りを覚える）。
    fn copy(&mut self, len: u32, dist: u32) -> Result<(), Error> {
        if u64::from(dist) > self.total {
            return Err(ZError::Corrupt("距離が出力の頭より前を指しています").into());
        }
        let room = (self.out.len() - self.w) as u32;
        let n = len.min(room);
        if n < len {
            self.state = State::Copy(len - n, dist);
        }
        let (n, dist) = (n as usize, dist as usize);
        self.total += n as u64;
        if self.total > self.opts.limit {
            return Err(ZError::TooLong.into());
        }
        let (w, src) = (self.w, self.w - dist);
        if dist >= n {
            self.out.copy_within(src..src + n, w);
        } else if dist == 1 {
            let b = self.out[src];
            self.out[w..w + n].fill(b);
        } else {
            // 周期 dist の繰り返し：まず 1 周期を写し、写した分を倍々に写す
            self.out.copy_within(src..src + dist, w);
            let mut p = dist;
            while p < n {
                let k = p.min(n - p);
                self.out.copy_within(w..w + k, w + p);
                p += k;
            }
        }
        self.w += n;
        Ok(())
    }
}

/// zlib の `src` を、ちょうど `expect` バイトに展開する（Adler-32 を照らす。多くても少なくても壊れた流れ）。流れの終わりの
/// あとのデータは読まない。不完全な符号の受け付けは miniz_oxide と同じ（aseprite のセル。前の miniz_oxide の
/// `decompress_slice_iter_to_slice` と同じ受け付け）。
pub fn inflate_exact(src: &[u8], expect: usize) -> Result<Vec<u8>, Error> {
    let mut z = Inflate::new(
        SliceInput::new(src),
        Options {
            zlib: true,
            check_adler: true,
            limit: expect as u64,
            flavor: Flavor::Miniz,
        },
    );
    z.set_size_hint(expect as u64);
    let mut out = Vec::new();
    out.try_reserve_exact(expect)
        .map_err(|_| ZError::Alloc(expect))?;
    while !z.finished() {
        let n = z.fill(64 * 1024)?;
        out.extend_from_slice(&z.ready()[..n]);
        z.consume(n);
        if n == 0 && !z.finished() {
            return Err(ZError::Truncated.into());
        }
    }
    if out.len() != expect {
        return Err(ZError::Short.into());
    }
    Ok(out)
}

/// `data` を、圧縮しない zlib（stored のブロックだけ・Adler-32 つき）にする（試験のファイルの作り手 `testfiles` が使う。値を
/// 運ぶだけで、大きさは縮めない。段 7 で圧縮を足す）。
pub fn store_zlib(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65_535 * 5 + 11);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut blocks = data.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while let Some(b) = blocks.next() {
        let last = blocks.peek().is_none();
        out.push(u8::from(last));
        let len = b.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(b);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests;
