//! FLAC の読み取り（自作。RFC 9639。M16 段 1・ADR-216）。`flac.rs` の `flac-symphonia-dec/1` の中身。
//!
//! 標本の値は仕様で 1 つに決まるので、前の symphonia 0.6.1 の FLAC と同じ値になる（識別子は変えない。ADR-216 (2)）。
//! 受け付ける範囲と値に効く細部も symphonia に合わせる（照合は下のテストの `same_values_as_symphonia_*` ほか）：
//! - 頭が `fLaC` でなければ `Unsupported`（ID3v2 の付いた FLAC などを次の段へ回す）。LPC の係数のずらしが負なら `Unsupported`。
//! - STREAMINFO：34 バイト、ブロックの最小・最大は 16 以上で最小 ≤ 最大、フレームの最小・最大は両方 0 でなければ最小 ≤ 最大、
//!   レート 1〜655,350、ビット数 4〜32。2 つあれば壊れたファイル。総フレーム数 0 は「不明」、MD5 が 0 なら照らさない。
//! - VORBIS_COMMENT（いくつあっても出た順に）：コメントを UTF-8 として読み（読めない所は U+FFFD）、最初の `=` で名前と値に
//!   分け、名前は 0x20〜0x7E の文字だけを残す。`=` のないもの、名前が CHAPTER＋数字 3 つで始まるもの（章）、
//!   METADATA_BLOCK_PICTURE・COVERART（画像。大文字と小文字を区別しない）はタグにしない。箱の中で切れていれば壊れたファイル。
//!   ほかの箱（PICTURE・SEEKTABLE・APPLICATION・CUESHEET・PADDING・知らない種類）は読み飛ばし、持たない（symphonia は
//!   中身が壊れていれば断ったが、値に効かないので受け付ける）。
//! - 最後の箱のあと、最初のフレームの頭（同期・頭の CRC-8・STREAMINFO との照合が通るもの）まで読み飛ばす。
//! - フレームの頭の照合（symphonia の strict check）：レート・ビット数は書いてあれば STREAMINFO と同じ、ブロックは STREAMINFO の
//!   最大以下、チャンネルの数は STREAMINFO と同じ、番号は STREAMINFO のブロックの最小 = 最大なら固定（フレームの番号）・違えば
//!   可変（標本の番号）で、2 つ前のフレームの番号より大きいか 0（[`fits`]）。同期の 2 バイト目の予約のビットは見ない。
//! - CRC-16 はふつう詰め物の直後の 2 バイト。そこで合わなければ、symphonia と同じく、次のフレームの頭（かファイルの終わり）の
//!   直前の 2 バイトで照らす（フレームの中の余分なバイトを受け付ける。最初の区切りだけ）。合わなければ壊れたファイル。
//! - フレームは前のフレームの終わりのすぐあとに続く。
//!   - 総フレーム数が分かるとき：そこまで読んだら止め、そのあとがフレームの頭なら「ヘッダより長い」、そうでなければ ID3v1
//!     などの余分なデータとして読み飛ばす（symphonia は最後のフレームを捨てて「ヘッダより短い」で断っていた）。届く前に
//!     フレームの頭でないものが来たら壊れたファイル。
//!   - 不明なとき：フレームの頭が続く限り読む。読めないフレーム（途中で切れた最後のフレームなど）のあとにフレームの頭が
//!     なければ、そのフレームを捨てて終わりにする（symphonia と同じ）。フレームの頭でないものが来たら、そのあとにフレームの頭が
//!     なければ余分なデータとして読み飛ばす。どちらも、あとにフレームの頭があれば壊れたファイル（symphonia は壊れた
//!     フレームを黙って捨てて読み続けた。総フレーム数も MD5 もない FLAC で値が違うのはこの形だけ）。長さを数えるのにフレームを
//!     読む（symphonia は数えるだけだった）ので、読めないフレームがあれば形を読む段で断る。
//! - 演算は symphonia と同じ幅（標本・残差・予測の足し算は i32 の折り返し、LPC の予測の和は i64）。32 ビットのステレオの差の
//!   チャンネル（33 ビット）だけは i64 で正確に読む（symphonia は差が 0 の定数・次数 0 の固定の予測子などの形だけを正しく
//!   読めた。その形の値は同じ。中間と差（M/S）で i32 があふれる形は、symphonia の値が誤っていた）。標本は 32 ビットに
//!   左詰めし、`pcm::int_to_f32(v, 32)` にする。
//! - MD5 は左詰めする前の標本を、フレームのビット数の入れ物の幅（1〜4 バイト）のリトルエンディアンで、時刻の順・チャンネルの
//!   順に並べたもの。
//!
//! 1 フレームは 16 MiB まで（symphonia と同じ。長い 1 進の列で時間をかけさせない）。中止は読み口を補うたび、フレームごと、
//! 頭を探して走査する所では 65,536 バイトごとに見る。

use crate::codec::byte_in::Input;
use crate::codec::md5::Md5;
use crate::codec::stream::DecodeOut;
use crate::codec::symph::Media;
use crate::codec::try_vec;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};

/// 読み取りの約束の版（実装を symphonia から替えても値は同じなので、識別子はそのまま。ADR-216 (2)）。
pub(crate) const DEC_ID: &str = "flac-symphonia-dec/1";
/// 1 フレームのバイト数の上限（symphonia の `FLAC_MAX_FRAME_SIZE`）。
const MAX_FRAME_BYTES: usize = 16 << 20;
/// フレームの頭のバイト数の上限（同期 2・記述 2・番号 7・ブロック 2・レート 2・CRC-8 1）。
const MAX_HEADER: usize = 16;

fn dec_err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: message.into(),
    }
}

fn truncated() -> EngineError {
    dec_err("ファイルが途中で切れています")
}

// ---------------------------------------------------------------------------------------------------------------
// CRC

const fn crc8_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const fn crc16_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

/// CRC-16 を 8 バイトずつ進める表：`[k][x]` は、バイト `x` のあとに 0 のバイトを `k` 個続けたものの CRC（`[0]` が
/// [`CRC16`]）。CRC は線形なので、8 バイトの CRC は、今の値の上位・下位のバイトを最初の 2 バイトに足して、各バイトの
/// 寄与を足し合わせたもの（[`crc16`]）。
const fn crc16_slices() -> [[u16; 256]; 8] {
    let base = crc16_table();
    let mut t = [[0u16; 256]; 8];
    t[0] = base;
    let mut k = 1;
    while k < 8 {
        let mut x = 0;
        while x < 256 {
            let c = t[k - 1][x];
            t[k][x] = (c << 8) ^ base[(c >> 8) as usize];
            x += 1;
        }
        k += 1;
    }
    t
}

/// CRC-8（多項式 0x07、初期値 0。フレームの頭）。
static CRC8: [u8; 256] = crc8_table();
/// CRC-16（多項式 0x8005、初期値 0。フレーム全体）。
static CRC16: [u16; 256] = crc16_table();
/// [`crc16_slices`]。
static CRC16_SLICES: [[u16; 256]; 8] = crc16_slices();

fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, b| CRC8[usize::from(c ^ b)])
}

#[inline]
fn crc16_byte(c: u16, b: u8) -> u16 {
    (c << 8) ^ CRC16[usize::from((c >> 8) as u8 ^ b)]
}

/// バイト列の CRC-16（[`crc16_byte`] を順に当てたものと同じ値。8 バイトずつ表を引く）。
fn crc16(data: &[u8]) -> u16 {
    let t = &CRC16_SLICES;
    let (blocks, rest) = data.as_chunks::<8>();
    let mut c = 0u16;
    for b in blocks {
        let [hi, lo] = c.to_be_bytes();
        c = t[7][usize::from(b[0] ^ hi)]
            ^ t[6][usize::from(b[1] ^ lo)]
            ^ t[5][usize::from(b[2])]
            ^ t[4][usize::from(b[3])]
            ^ t[3][usize::from(b[4])]
            ^ t[2][usize::from(b[5])]
            ^ t[1][usize::from(b[6])]
            ^ t[0][usize::from(b[7])];
    }
    rest.iter().fold(c, |c, b| crc16_byte(c, *b))
}

// ---------------------------------------------------------------------------------------------------------------
// STREAMINFO とタグ

/// STREAMINFO。
#[derive(Debug, Clone)]
pub(crate) struct StreamInfo {
    pub block_min: u16,
    pub block_max: u16,
    pub rate: u32,
    pub channels: usize,
    pub bps: u32,
    /// 総フレーム数（0 は不明）。
    pub total: u64,
    /// 標本の MD5（0 ばかりなら None）。
    pub md5: Option<[u8; 16]>,
}

fn parse_streaminfo(b: &[u8; 34]) -> Result<StreamInfo, EngineError> {
    let block_min = u16::from_be_bytes([b[0], b[1]]);
    let block_max = u16::from_be_bytes([b[2], b[3]]);
    if block_min < 16 || block_max < 16 {
        return Err(dec_err("STREAMINFO のブロックが 16 フレームより小さい"));
    }
    if block_max < block_min {
        return Err(dec_err("STREAMINFO のブロックの最大が最小より小さい"));
    }
    let frame_min = u32::from_be_bytes([0, b[4], b[5], b[6]]);
    let frame_max = u32::from_be_bytes([0, b[7], b[8], b[9]]);
    if frame_min > 0 && frame_max > 0 && frame_max < frame_min {
        return Err(dec_err("STREAMINFO のフレームの最大が最小より小さい"));
    }
    let x = u64::from_be_bytes([b[10], b[11], b[12], b[13], b[14], b[15], b[16], b[17]]);
    let rate = (x >> 44) as u32;
    if !(1..=655_350).contains(&rate) {
        return Err(dec_err("STREAMINFO のサンプルレートが範囲の外です"));
    }
    let channels = ((x >> 41) & 0x7) as usize + 1;
    let bps = ((x >> 36) & 0x1f) as u32 + 1;
    if !(4..=32).contains(&bps) {
        return Err(dec_err("STREAMINFO のビット数が範囲の外です"));
    }
    let total = x & 0xf_ffff_ffff;
    let mut md5 = [0u8; 16];
    md5.copy_from_slice(&b[18..34]);
    Ok(StreamInfo {
        block_min,
        block_max,
        rate,
        channels,
        bps,
        total,
        md5: (md5 != [0u8; 16]).then_some(md5),
    })
}

/// 名前が CHAPTER＋数字 3 つで始まるか（章の情報。0x20〜0x7E だけに絞った名前に当てる）。
fn is_chapter_key(key: &str) -> bool {
    let b = key.as_bytes();
    b.len() >= 10
        && b[..7].eq_ignore_ascii_case(b"CHAPTER")
        && b[7..10].iter().all(u8::is_ascii_digit)
}

/// コメント 1 つをタグにする（タグにしないものは None）。
///
/// 値は 1 度だけ確保する（読めない UTF-8 を置き換えた文字列は、そのまま名前の分を切り落として値にする。持つ量の見積もり
/// `held::flac` の倍率に収めるため）。
pub(crate) fn comment_tag(data: &[u8]) -> Option<(String, String)> {
    let comment = String::from_utf8_lossy(data);
    let eq = comment.find('=')?;
    let key: String = comment[..eq]
        .chars()
        .filter(|c| matches!(c, ' '..='~'))
        .collect();
    if is_chapter_key(&key)
        || key.eq_ignore_ascii_case("metadata_block_picture")
        || key.eq_ignore_ascii_case("coverart")
    {
        return None;
    }
    let value = match comment {
        std::borrow::Cow::Borrowed(c) => c[eq + 1..].to_string(),
        std::borrow::Cow::Owned(mut c) => {
            c.drain(..=eq);
            c
        }
    };
    Some((key, value))
}

/// VORBIS_COMMENT の中身（リトルエンディアンの長さ付き）からタグを足す。箱の中で切れていれば壊れたファイル。
fn parse_comments(block: &[u8], tags: &mut Vec<(String, String)>) -> Result<(), EngineError> {
    let broken = || dec_err("VORBIS_COMMENT が箱の中で切れています");
    let mut at = 0usize;
    let u32le = |at: &mut usize| -> Result<u32, EngineError> {
        let b = block.get(*at..*at + 4).ok_or_else(broken)?;
        *at += 4;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let vendor = u32le(&mut at)? as usize;
    at = at
        .checked_add(vendor)
        .filter(|e| *e <= block.len())
        .ok_or_else(broken)?;
    let n = u32le(&mut at)?;
    for _ in 0..n {
        let len = u32le(&mut at)? as usize;
        let end = at
            .checked_add(len)
            .filter(|e| *e <= block.len())
            .ok_or_else(broken)?;
        if let Some(t) = comment_tag(&block[at..end]) {
            tags.try_reserve(1)
                .map_err(|_| EngineError::Limit("タグが多すぎます".into()))?;
            tags.push(t);
        }
        at = end;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// フレームの頭

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Assign {
    Independent(usize),
    LeftSide,
    RightSide,
    MidSide,
}

impl Assign {
    fn channels(self) -> usize {
        match self {
            Assign::Independent(n) => n,
            _ => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    /// 可変のブロック（標本の番号）か。
    variable: bool,
    /// フレームの番号か標本の番号。
    seq: u64,
    block: u32,
    rate: Option<u32>,
    bps: Option<u32>,
    assign: Assign,
    /// 頭のバイト数（CRC-8 を含む）。
    len: usize,
}

/// フレームの頭を読む（同期・記述・番号・ブロック・レート・ビット数・チャンネル・CRC-8。どれかが合わなければ None）。
fn parse_header(b: &[u8]) -> Option<FrameHeader> {
    let get = |i: usize| b.get(i).copied();
    if get(0)? != 0xff || get(1)? & 0xfc != 0xf8 {
        return None;
    }
    let variable = get(1)? & 1 == 1;
    let (d0, d1) = (get(2)?, get(3)?);
    if d1 & 1 == 1 {
        return None;
    }
    let mut i = 4;
    let lead = get(i)?;
    i += 1;
    let (mut seq, extra) = match lead {
        0x00..=0x7f => (u64::from(lead), 0),
        0xc0..=0xdf => (u64::from(lead & 0x1f), 1),
        0xe0..=0xef => (u64::from(lead & 0x0f), 2),
        0xf0..=0xf7 => (u64::from(lead & 0x07), 3),
        0xf8..=0xfb => (u64::from(lead & 0x03), 4),
        0xfc..=0xfd => (u64::from(lead & 0x01), 5),
        0xfe => (0, 6),
        _ => return None,
    };
    for _ in 0..extra {
        seq = (seq << 6) | u64::from(get(i)? & 0x3f);
        i += 1;
    }
    let max_seq = if variable { 0xf_ffff_ffff } else { 0x7fff_ffff };
    if seq > max_seq {
        return None;
    }
    let block = match d0 >> 4 {
        0 => return None,
        1 => 192,
        e @ 2..=5 => 576 << (e - 2),
        6 => {
            let v = u32::from(get(i)?) + 1;
            i += 1;
            v
        }
        7 => {
            let v = u16::from_be_bytes([get(i)?, get(i + 1)?]);
            i += 2;
            if v == 0xffff {
                return None;
            }
            u32::from(v) + 1
        }
        e => 256 << (e - 8),
    };
    let rate = match d0 & 0x0f {
        0 => None,
        1 => Some(88_200),
        2 => Some(176_400),
        3 => Some(192_000),
        4 => Some(8_000),
        5 => Some(16_000),
        6 => Some(22_050),
        7 => Some(24_000),
        8 => Some(32_000),
        9 => Some(44_100),
        0xa => Some(48_000),
        0xb => Some(96_000),
        0xc => {
            let v = u32::from(get(i)?) * 1_000;
            i += 1;
            Some(v)
        }
        0xd | 0xe => {
            let v = u32::from(u16::from_be_bytes([get(i)?, get(i + 1)?]));
            i += 2;
            Some(if d0 & 0x0f == 0xe { v * 10 } else { v })
        }
        _ => return None,
    };
    if rate.is_some_and(|r| !(1..=655_350).contains(&r)) {
        return None;
    }
    let bps = match (d1 >> 1) & 0x7 {
        0 => None,
        1 => Some(8),
        2 => Some(12),
        4 => Some(16),
        5 => Some(20),
        6 => Some(24),
        7 => Some(32),
        _ => return None,
    };
    let assign = match d1 >> 4 {
        n @ 0..=7 => Assign::Independent(usize::from(n) + 1),
        8 => Assign::LeftSide,
        9 => Assign::RightSide,
        0xa => Assign::MidSide,
        _ => return None,
    };
    if crc8(b.get(..i)?) != get(i)? {
        return None;
    }
    Some(FrameHeader {
        variable,
        seq,
        block,
        rate,
        bps,
        assign,
        len: i + 1,
    })
}

/// フレームの頭が STREAMINFO と前のフレームに合うか（symphonia の strict check）。番号は `last_seq`（なければ 0）より
/// 大きいか 0。`last_seq` は 2 つ前のフレームの番号（symphonia はフレームの終わりを次の頭で決め、そのときの「前」はまだ
/// 組んでいない今のフレームの 1 つ前なので）。
fn fits(info: &StreamInfo, h: &FrameHeader, last_seq: Option<u64>) -> bool {
    if h.rate.is_some_and(|r| r != info.rate) || h.bps.is_some_and(|b| b != info.bps) {
        return false;
    }
    if h.block > u32::from(info.block_max) {
        return false;
    }
    let fixed = info.block_min == info.block_max;
    let last = last_seq.unwrap_or(0);
    if h.variable == fixed || !(h.seq > last || h.seq == 0) {
        return false;
    }
    h.assign.channels() == info.channels
}

// ---------------------------------------------------------------------------------------------------------------
// ビットの読み手（フレームの中身。8 バイトまで先読みし、終わりに先読みした分を読み口へ返す）

struct Bits<'i, 'a> {
    input: &'i mut Input<'a>,
    /// 上位の `n` ビットだけが有効（下位は 0）。
    acc: u64,
    n: u32,
}

impl<'i, 'a> Bits<'i, 'a> {
    fn new(input: &'i mut Input<'a>) -> Self {
        Self {
            input,
            acc: 0,
            n: 0,
        }
    }

    /// 57 ビット以上になるまで（読み口が尽きればそこまで）丸ごとのバイトを足す。
    #[inline]
    fn fill(&mut self) -> Result<(), EngineError> {
        if self.input.since_mark() > MAX_FRAME_BYTES {
            return Err(dec_err("フレームが大きすぎます"));
        }
        let pos = self.input.pos;
        if self.n <= 56 && self.input.end - pos >= 8 {
            // 8 バイトをまとめて読み、入る丸ごとのバイトだけを残す
            let mut w = [0u8; 8];
            w.copy_from_slice(&self.input.data()[pos..pos + 8]);
            let take = (64 - self.n) / 8;
            let n = self.n + 8 * take;
            self.acc = (self.acc | (u64::from_be_bytes(w) >> self.n))
                & if n >= 64 { u64::MAX } else { !(u64::MAX >> n) };
            self.n = n;
            self.input.pos += take as usize;
            return Ok(());
        }
        while self.n <= 56 {
            if self.input.pos >= self.input.end && !self.input.refill()? {
                break;
            }
            let b = self.input.data()[self.input.pos];
            self.acc |= u64::from(b) << (56 - self.n);
            self.n += 8;
            self.input.pos += 1;
        }
        Ok(())
    }

    /// `bits`（0〜32）ビットを読む。
    #[inline]
    fn read(&mut self, bits: u32) -> Result<u32, EngineError> {
        if bits == 0 {
            return Ok(0);
        }
        if self.n < bits {
            self.fill()?;
            if self.n < bits {
                return Err(truncated());
            }
        }
        let v = self.acc >> (64 - bits);
        self.acc <<= bits;
        self.n -= bits;
        Ok(v as u32)
    }

    /// 1 進の数（1 の前の 0 の数）。
    #[inline]
    fn unary(&mut self) -> Result<u32, EngineError> {
        let mut count = 0u32;
        loop {
            let zeros = self.acc.leading_zeros();
            if zeros < self.n {
                // 64 ビットずらすと溢れるので 2 回に分ける
                self.acc = (self.acc << zeros) << 1;
                self.n -= zeros + 1;
                return Ok(count.saturating_add(zeros));
            }
            count = count.saturating_add(self.n);
            self.acc = 0;
            self.n = 0;
            self.fill()?;
            if self.n == 0 {
                return Err(truncated());
            }
        }
    }

    /// ライスの符号語 1 つ（1 進の数 `q` と続く `k` ビット `r` を、symphonia と同じく u32 で `(q << k) | r` に組む。あふれた
    /// ビットは捨てる）。符号語がすべて先読みの中にあれば先読みから直に取る（[`Bits::unary`] と [`Bits::read`] を順に呼ぶ
    /// のと同じ値・同じ位置。読み足さない）。そうでなければその 2 つを呼ぶ。
    #[inline]
    fn rice(&mut self, k: u32) -> Result<u32, EngineError> {
        let zeros = self.acc.leading_zeros();
        let used = zeros + 1 + k;
        if used <= self.n {
            // 64 ビットずらすと溢れるので 2 回に分ける（zeros ≤ 63、1 + k ≤ 32）
            let head = self.acc << zeros;
            let r = (head << 1).checked_shr(64 - k).unwrap_or(0) as u32;
            self.acc = head << (1 + k);
            self.n -= used;
            return Ok((zeros << k) | r);
        }
        let q = self.unary()?;
        let r = self.read(k)?;
        Ok((q << k) | r)
    }

    /// バイトの境目までの詰め物を捨て、先読みした丸ごとのバイトを読み口へ返す。
    fn finish(self) {
        self.input.pos -= (self.n / 8) as usize;
    }
}

/// 幅 `w`（0〜32）の符号付きの値に広げる（symphonia の `sign_extend_leq32_to_i32` と同じ）。
#[inline]
fn sext(v: u32, w: u32) -> i32 {
    (v.wrapping_shl(32 - w) as i32).wrapping_shr(32 - w)
}

// ---------------------------------------------------------------------------------------------------------------
// サブフレーム

fn read_subframe(
    bits: &mut Bits<'_, '_>,
    frame_bps: u32,
    buf: &mut [i32],
) -> Result<(), EngineError> {
    if bits.read(1)? != 0 {
        return Err(dec_err("サブフレームの詰め物が 0 ではありません"));
    }
    let kind = bits.read(6)?;
    let wasted = if bits.read(1)? == 1 {
        bits.unary()?.saturating_add(1)
    } else {
        0
    };
    if wasted > frame_bps {
        return Err(dec_err("下位の 0 のビットがビット数より多い"));
    }
    let bps = frame_bps - wasted;
    if bps > 32 {
        return Err(EngineError::Internal(
            "33 ビットの標本は read_subframe_wide で読む".into(),
        ));
    }
    match kind {
        0x00 => {
            let v = sext(bits.read(bps)?, bps);
            buf.fill(v);
        }
        0x01 => verbatim(bits, bps, buf)?,
        0x08..=0x0f => {
            let order = (kind & 0x07) as usize;
            if order > 4 {
                return Err(dec_err("固定の予測子の次数が 4 を超えています"));
            }
            if order > buf.len() {
                return Err(dec_err("予測子の次数がブロックより大きい"));
            }
            verbatim(bits, bps, &mut buf[..order])?;
            residual(bits, order, buf)?;
            fixed_predict(order, buf);
        }
        0x20..=0x3f => {
            let order = (kind & 0x1f) as usize + 1;
            if order > buf.len() {
                return Err(dec_err("予測子の次数がブロックより大きい"));
            }
            verbatim(bits, bps, &mut buf[..order])?;
            let precision = bits.read(4)? + 1;
            if precision > 15 {
                return Err(dec_err("LPC の係数の精度が予約の値です"));
            }
            let shift = sext(bits.read(5)?, 5);
            if shift < 0 {
                return Err(EngineError::Unsupported(format!(
                    "{DEC_ID}: LPC の係数のずらしが負のものは読めません"
                )));
            }
            let mut coeffs = [0i64; 32];
            for c in coeffs.iter_mut().take(order) {
                *c = i64::from(sext(bits.read(precision)?, precision));
            }
            residual(bits, order, buf)?;
            lpc_predict(&coeffs[..order], shift as u32, buf);
        }
        _ => return Err(dec_err("サブフレームの種類が予約の値です")),
    }
    if wasted > 0 {
        for s in buf.iter_mut() {
            *s = s.wrapping_shl(wasted);
        }
    }
    Ok(())
}

fn verbatim(bits: &mut Bits<'_, '_>, bps: u32, buf: &mut [i32]) -> Result<(), EngineError> {
    for s in buf.iter_mut() {
        *s = sext(bits.read(bps)?, bps);
    }
    Ok(())
}

/// 分割ライスの残差を `buf[order..]` に読む。
fn residual(bits: &mut Bits<'_, '_>, order: usize, buf: &mut [i32]) -> Result<(), EngineError> {
    let pbits = match bits.read(2)? {
        0 => 4,
        1 => 5,
        _ => return Err(dec_err("残差の方式が予約の値です")),
    };
    let porder = bits.read(4)?;
    let len = buf.len();
    let psize = len >> porder;
    let parts = 1usize << porder;
    if order > psize {
        return Err(dec_err("残差の分割が予測子の次数より小さい"));
    }
    if parts * psize != len {
        return Err(dec_err("残差の分割がブロックと合いません"));
    }
    let escape = (1u32 << pbits) - 1;
    let mut start = order;
    for p in 0..parts {
        let end = (p + 1) * psize;
        let k = bits.read(pbits)?;
        if k < escape {
            for s in &mut buf[start..end] {
                let w = bits.rice(k)?;
                *s = ((w >> 1) as i32) ^ -((w & 1) as i32);
            }
        } else {
            let rb = bits.read(5)?;
            for s in &mut buf[start..end] {
                *s = sext(bits.read(rb)?, rb);
            }
        }
        start = end;
    }
    Ok(())
}

/// 固定の予測（次数 1〜4）。C の式 `x[i] = r[i] + 2x[i−1] − x[i−2]` などは、`r` を次数の回だけ順に足し上げたもの（`r` が
/// 次数の階差）と同じなので、階差を持って回す（足し算は i32 の折り返し：2³² を法とする和なので、i64 で予測してから i32 に
/// 戻す symphonia の式と同じ値。試験 `fixed_predict_matches_the_direct_form`）。どの階差も前の標本の 1 回の足し算で
/// 決まるので、標本ごとに掛け算を待たない。
fn fixed_predict(order: usize, buf: &mut [i32]) {
    match order {
        1 => {
            let Some((first, rest)) = buf.split_first_mut() else {
                return;
            };
            let mut a = *first;
            for v in rest {
                *v = v.wrapping_add(a);
                a = *v;
            }
        }
        2 if buf.len() > 2 => {
            let mut x = buf[1];
            let mut d1 = buf[1].wrapping_sub(buf[0]);
            for v in &mut buf[2..] {
                d1 = d1.wrapping_add(*v);
                x = x.wrapping_add(d1);
                *v = x;
            }
        }
        3 if buf.len() > 3 => {
            let mut x = buf[2];
            let mut d1 = buf[2].wrapping_sub(buf[1]);
            let mut d2 = d1.wrapping_sub(buf[1].wrapping_sub(buf[0]));
            for v in &mut buf[3..] {
                d2 = d2.wrapping_add(*v);
                d1 = d1.wrapping_add(d2);
                x = x.wrapping_add(d1);
                *v = x;
            }
        }
        4 if buf.len() > 4 => {
            let mut x = buf[3];
            let mut d1 = buf[3].wrapping_sub(buf[2]);
            let e1 = buf[2].wrapping_sub(buf[1]);
            let f1 = buf[1].wrapping_sub(buf[0]);
            let mut d2 = d1.wrapping_sub(e1);
            let mut d3 = d2.wrapping_sub(e1.wrapping_sub(f1));
            for v in &mut buf[4..] {
                d3 = d3.wrapping_add(*v);
                d2 = d2.wrapping_add(d3);
                d1 = d1.wrapping_add(d2);
                x = x.wrapping_add(d1);
                *v = x;
            }
        }
        _ => {}
    }
}

/// LPC の予測（`coeffs[j]` は `x[i − 1 − j]` に掛ける。和は i64 で、`shift` だけ右へずらして足す）。
fn lpc_predict(coeffs: &[i64], shift: u32, buf: &mut [i32]) {
    let order = coeffs.len();
    for i in order..buf.len() {
        let (past, rest) = buf.split_at_mut(i);
        let mut p = 0i64;
        for (c, x) in coeffs.iter().zip(past[i - order..].iter().rev()) {
            p += c * i64::from(*x);
        }
        rest[0] = rest[0].wrapping_add((p >> shift) as i32);
    }
}

/// 33 ビットまでの値を読んで符号付きに広げる。
fn read_wide(bits: &mut Bits<'_, '_>, w: u32) -> Result<i64, EngineError> {
    let v = if w > 32 {
        let hi = u64::from(bits.read(w - 32)?);
        (hi << 32) | u64::from(bits.read(32)?)
    } else {
        u64::from(bits.read(w)?)
    };
    Ok(if w == 0 {
        0
    } else {
        ((v << (64 - w)) as i64) >> (64 - w)
    })
}

/// 32 ビットのステレオの差のチャンネル（33 ビット。RFC 9639 §9.2.4）を i64 で読む。`scratch` は残差を読む作業列。
///
/// 予測は i64 で正確に求める（値が i32 の幅に収まるなら、`read_subframe` の i32 の折り返しと同じ値。symphonia は 33 ビットを
/// 正しく読めず、差が 0 の定数・次数 0 の固定の予測子などの形だけを正しく読めた）。
fn read_subframe_wide(
    bits: &mut Bits<'_, '_>,
    frame_bps: u32,
    buf: &mut [i64],
    scratch: &mut [i32],
) -> Result<(), EngineError> {
    if bits.read(1)? != 0 {
        return Err(dec_err("サブフレームの詰め物が 0 ではありません"));
    }
    let kind = bits.read(6)?;
    let wasted = if bits.read(1)? == 1 {
        bits.unary()?.saturating_add(1)
    } else {
        0
    };
    if wasted > frame_bps {
        return Err(dec_err("下位の 0 のビットがビット数より多い"));
    }
    let bps = frame_bps - wasted;
    let (order, coeffs, shift): (usize, Vec<i64>, u32) = match kind {
        0x00 => {
            let v = read_wide(bits, bps)?;
            buf.fill(v);
            (0, Vec::new(), 0)
        }
        0x01 => {
            for s in buf.iter_mut() {
                *s = read_wide(bits, bps)?;
            }
            (0, Vec::new(), 0)
        }
        0x08..=0x0f => {
            let order = (kind & 0x07) as usize;
            if order > 4 {
                return Err(dec_err("固定の予測子の次数が 4 を超えています"));
            }
            let coeffs = [
                vec![],
                vec![1],
                vec![2, -1],
                vec![3, -3, 1],
                vec![4, -6, 4, -1],
            ];
            (order, coeffs[order].clone(), 0)
        }
        0x20..=0x3f => ((kind & 0x1f) as usize + 1, Vec::new(), 0),
        _ => return Err(dec_err("サブフレームの種類が予約の値です")),
    };
    if kind >= 0x08 {
        if order > buf.len() {
            return Err(dec_err("予測子の次数がブロックより大きい"));
        }
        for s in &mut buf[..order] {
            *s = read_wide(bits, bps)?;
        }
        let (coeffs, shift) = if kind >= 0x20 {
            let precision = bits.read(4)? + 1;
            if precision > 15 {
                return Err(dec_err("LPC の係数の精度が予約の値です"));
            }
            let shift = sext(bits.read(5)?, 5);
            if shift < 0 {
                return Err(EngineError::Unsupported(format!(
                    "{DEC_ID}: LPC の係数のずらしが負のものは読めません"
                )));
            }
            let mut c = Vec::with_capacity(order);
            for _ in 0..order {
                c.push(i64::from(sext(bits.read(precision)?, precision)));
            }
            (c, shift as u32)
        } else {
            (coeffs, shift)
        };
        let scratch = &mut scratch[..buf.len()];
        residual(bits, order, scratch)?;
        for i in order..buf.len() {
            let mut p = 0i64;
            for (j, c) in coeffs.iter().enumerate() {
                p = p.wrapping_add(c.wrapping_mul(buf[i - 1 - j]));
            }
            buf[i] = i64::from(scratch[i]).wrapping_add(p >> shift);
        }
    }
    if wasted > 0 {
        for s in buf.iter_mut() {
            *s = s.wrapping_shl(wasted);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// 開く・フレームを読む

/// 次に来るもの。
enum Next {
    Frame(FrameHeader),
    /// フレームの頭でないもの。
    Other,
    End,
}

/// 開いた FLAC（最初のフレームの頭の位置にいる）。
pub(crate) struct Reader<'a> {
    pub info: StreamInfo,
    /// Vorbis コメントのタグ（名前, 値）。出た順に。
    pub tags: Vec<(String, String)>,
    input: Input<'a>,
    /// 2 つ前と 1 つ前のフレームの番号（[`fits`] は 2 つ前と比べる）。
    seqs: [Option<u64>; 2],
    /// 標本の作業列（チャンネルごと）。
    planes: Vec<Vec<i32>>,
    /// 32 ビットのステレオの差のチャンネル（33 ビット）の作業列。
    wide: Vec<i64>,
}

/// 開いて、メタデータの箱を読み、最初のフレームの頭まで進む。チャンネル・レートを上限で確かめる。
pub(crate) fn open<'a>(
    media: &'a mut Media<'_>,
    limits: &Limits,
    mon: &'a dyn Monitor,
) -> Result<Reader<'a>, EngineError> {
    let mut input = Input::new(media, mon, DEC_ID)?;
    let mut marker = [0u8; 4];
    input.read_exact(&mut marker)?;
    if &marker != b"fLaC" {
        return Err(EngineError::Unsupported(format!(
            "{DEC_ID}: FLAC の目印（fLaC）がありません"
        )));
    }
    let mut info: Option<StreamInfo> = None;
    let mut tags = Vec::new();
    loop {
        let head = input.read_u8()?;
        let len = input.be24()?;
        match head & 0x7f {
            0 => {
                if info.is_some() {
                    return Err(dec_err("STREAMINFO が 2 つあります"));
                }
                if len != 34 {
                    return Err(dec_err("STREAMINFO の大きさが 34 バイトではありません"));
                }
                let mut b = [0u8; 34];
                input.read_exact(&mut b)?;
                info = Some(parse_streaminfo(&b)?);
            }
            4 => {
                let mut block = try_vec(len as usize, 0u8)?;
                input.read_exact(&mut block)?;
                parse_comments(&block, &mut tags)?;
            }
            _ => input.skip(u64::from(len))?,
        }
        if head & 0x80 != 0 {
            break;
        }
    }
    let info = info.ok_or_else(|| dec_err("STREAMINFO がありません"))?;
    if info.channels as u64 > limits.max_channels {
        return Err(EngineError::Limit("チャンネルが多すぎます".into()));
    }
    if info.rate > limits.max_sample_rate {
        return Err(EngineError::Limit("サンプルレートが大きすぎます".into()));
    }
    // 最初のフレームの頭まで読み飛ばす（symphonia の resync）
    let mut scanned = 0u32;
    loop {
        scanned = scanned.wrapping_add(1);
        if scanned.is_multiple_of(1 << 16) && mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let b = input.peek(MAX_HEADER)?;
        if b.len() < 2 {
            return Err(dec_err("フレームがありません"));
        }
        if parse_header(b).is_some_and(|h| fits(&info, &h, None)) {
            break;
        }
        input.advance(1);
    }
    Ok(Reader {
        info,
        tags,
        input,
        seqs: [None, None],
        planes: Vec::new(),
        wide: Vec::new(),
    })
}

impl Reader<'_> {
    /// 長さ（総フレーム数。不明なら、フレームをたどって数える）。
    pub fn frames(&mut self, limits: &Limits) -> Result<u64, EngineError> {
        if self.info.total > 0 {
            Ok(self.info.total)
        } else {
            self.count_frames(limits)
        }
    }

    /// 次に来るもの（読み進めない）。
    fn next(&mut self) -> Result<Next, EngineError> {
        let b = self.input.peek(MAX_HEADER)?;
        if b.is_empty() {
            return Ok(Next::End);
        }
        Ok(match parse_header(b) {
            Some(h) if fits(&self.info, &h, self.seqs[0]) => Next::Frame(h),
            _ => Next::Other,
        })
    }

    /// フレームの頭でないものが来たときに、そのあとにフレームの頭があるか（読み進める）。
    fn frame_follows(&mut self) -> Result<bool, EngineError> {
        let mut scanned = 0u32;
        loop {
            scanned = scanned.wrapping_add(1);
            if scanned.is_multiple_of(1 << 16) && self.input.mon.canceled() {
                return Err(EngineError::Canceled);
            }
            self.input.advance(1);
            match self.next()? {
                Next::Frame(_) => return Ok(true),
                Next::Other => {}
                Next::End => return Ok(false),
            }
        }
    }

    /// 次のフレームの頭（なければ None）。総フレーム数 `total`（0 は不明）と、ここまでの数 `done` で、続きの扱いを決める
    /// （モジュールの説明）。
    fn next_frame(&mut self, done: u64) -> Result<Option<FrameHeader>, EngineError> {
        let total = self.info.total;
        let next = self.next()?;
        if total > 0 && done >= total {
            return match next {
                Next::Frame(_) => Err(dec_err("長さがヘッダより長い")),
                _ => Ok(None),
            };
        }
        match next {
            Next::Frame(h) => Ok(Some(h)),
            Next::End if total > 0 => Err(truncated()),
            Next::End => Ok(None),
            Next::Other if total > 0 => Err(dec_err("フレームのあとに余分なデータがあります")),
            Next::Other => {
                if self.frame_follows()? {
                    Err(dec_err("フレームのあいだに余分なデータがあります"))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// フレームを 1 つ読んで、標本（左詰めする前）を `self.planes` に置く。MD5 があれば足す。
    fn read_frame(&mut self, h: &FrameHeader, md5: Option<&mut Md5>) -> Result<usize, EngineError> {
        if self.input.mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let n = h.block as usize;
        let bps = h.bps.unwrap_or(self.info.bps);
        let ch = self.info.channels;
        if self.planes.len() != ch {
            self.planes = (0..ch).map(|_| Vec::new()).collect();
        }
        for p in &mut self.planes {
            if p.len() < n {
                p.try_reserve_exact(n - p.len())
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            }
            p.resize(n, 0);
        }
        // 32 ビットのステレオの差のチャンネルは 33 ビットなので i64 で読む
        let wide = bps == 32 && !matches!(h.assign, Assign::Independent(_));
        if wide && self.wide.len() < n {
            self.wide
                .try_reserve_exact(n - self.wide.len())
                .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
            self.wide.resize(n, 0);
        }
        // フレームの頭から CRC-16 の前までを残して読む（CRC-16 は終わりにまとめて求める）
        self.input.mark();
        self.input.advance(h.len);
        let mut bits = Bits::new(&mut self.input);
        for c in 0..ch {
            let side = match h.assign {
                Assign::LeftSide | Assign::MidSide => c == 1,
                Assign::RightSide => c == 0,
                Assign::Independent(_) => false,
            };
            if wide && side {
                read_subframe_wide(&mut bits, 33, &mut self.wide[..n], &mut self.planes[c])?;
            } else {
                read_subframe(&mut bits, bps + u32::from(side), &mut self.planes[c][..n])?;
            }
        }
        bits.finish();
        self.frame_end()?;
        if wide && let [a, b, ..] = self.planes.as_mut_slice() {
            let (a, b, s) = (&mut a[..n], &mut b[..n], &self.wide[..n]);
            // i64 で正確に戻し、i32 にする（左右・差と右は i32 の折り返しと同じ値）
            match h.assign {
                Assign::LeftSide => {
                    for ((l, r), s) in a.iter().zip(b.iter_mut()).zip(s) {
                        *r = (i64::from(*l).wrapping_sub(*s)) as i32;
                    }
                }
                Assign::RightSide => {
                    for ((l, r), s) in a.iter_mut().zip(b.iter()).zip(s) {
                        *l = s.wrapping_add(i64::from(*r)) as i32;
                    }
                }
                Assign::MidSide => {
                    for ((m, r), s) in a.iter_mut().zip(b.iter_mut()).zip(s) {
                        let mid = (i64::from(*m) << 1) | (s & 1);
                        *m = (mid.wrapping_add(*s) >> 1) as i32;
                        *r = (mid.wrapping_sub(*s) >> 1) as i32;
                    }
                }
                Assign::Independent(_) => {}
            }
        } else if let [a, b, ..] = self.planes.as_mut_slice() {
            let (a, b) = (&mut a[..n], &mut b[..n]);
            match h.assign {
                Assign::LeftSide => {
                    for (l, s) in a.iter().zip(b.iter_mut()) {
                        *s = l.wrapping_sub(*s);
                    }
                }
                Assign::RightSide => {
                    for (s, r) in a.iter_mut().zip(b.iter()) {
                        *s = s.wrapping_add(*r);
                    }
                }
                Assign::MidSide => {
                    for (m, s) in a.iter_mut().zip(b.iter_mut()) {
                        let mid = (*m << 1) | (*s & 1);
                        let side = *s;
                        *m = mid.wrapping_add(side) >> 1;
                        *s = mid.wrapping_sub(side) >> 1;
                    }
                }
                Assign::Independent(_) => {}
            }
        }
        if let Some(md5) = md5 {
            md5_frame(md5, &self.planes, n, bps);
        }
        self.seqs = [self.seqs[1], Some(h.seq)];
        Ok(n)
    }

    /// サブフレームを読み終えた所（詰め物のあと）から、CRC-16 を照らしてフレームの終わりへ進む。CRC-16 はふつう詰め物の
    /// 直後の 2 バイトで、そこで合わなければ、symphonia と同じく「次のフレームの頭（同期・頭の CRC-8・照合が通るもの）か
    /// ファイルの終わりの直前の 2 バイト」を CRC-16 として、合う所を探す（フレームの中の余分なバイトを受け付ける）。
    fn frame_end(&mut self) -> Result<(), EngineError> {
        let body = self.input.since_mark();
        if body > MAX_FRAME_BYTES {
            return Err(dec_err("フレームが大きすぎます"));
        }
        if !self.input.ensure_rel(body + 2)? {
            return Err(truncated());
        }
        let crc_of = |input: &Input<'_>, end: usize| crc16(input.slice_rel(0, end));
        let be16 = |input: &Input<'_>, at: usize| {
            let b = input.slice_rel(at, at + 2);
            u16::from_be_bytes([b[0], b[1]])
        };
        let crc = crc_of(&self.input, body);
        if be16(&self.input, body) == crc {
            self.input.release_at(body + 2);
            return Ok(());
        }
        // 次の頭か終わりで区切る（symphonia の断片の区切り。比べる番号は、今のフレームの 1 つ前）。最初の区切りだけを見る
        // （先の区切りまで探すと、偶然 CRC-16 の合う所までのフレームを黙って飛ばしうる）
        let mut end = body + 3;
        let mut scanned = 0u32;
        loop {
            scanned = scanned.wrapping_add(1);
            if scanned.is_multiple_of(1 << 16) && self.input.mon.canceled() {
                return Err(EngineError::Canceled);
            }
            if end > MAX_FRAME_BYTES {
                return Err(dec_err("フレームの CRC-16 が合いません"));
            }
            self.input.ensure_rel(end + MAX_HEADER)?;
            let avail = self.input.avail_rel();
            if end > avail {
                return Err(dec_err("フレームの CRC-16 が合いません"));
            }
            let head = self.input.slice_rel(end, avail.min(end + MAX_HEADER));
            if end == avail
                || parse_header(head).is_some_and(|h| fits(&self.info, &h, self.seqs[1]))
            {
                if be16(&self.input, end - 2) == crc_of(&self.input, end - 2) {
                    self.input.release_at(end);
                    return Ok(());
                }
                return Err(dec_err("フレームの CRC-16 が合いません"));
            }
            end += 1;
        }
    }

    /// 次のフレームを読む（なければ None。読んだらフレームの長さ）。総フレーム数が不明なとき、読めなかったフレームのあとに
    /// フレームの頭がなければ、そのフレーム（途中で切れた最後のフレームなど）を捨てて終わりにする（symphonia と同じ。MD5 が
    /// あれば、照らしたところで合わずに壊れたファイルになる）。あとにフレームの頭があれば壊れたファイル。
    fn next_decoded(
        &mut self,
        done: u64,
        md5: Option<&mut Md5>,
    ) -> Result<Option<(FrameHeader, usize)>, EngineError> {
        let Some(h) = self.next_frame(done)? else {
            return Ok(None);
        };
        match self.read_frame(&h, md5) {
            Ok(n) => Ok(Some((h, n))),
            Err(e) => {
                if self.info.total == 0
                    && !matches!(e, EngineError::Canceled | EngineError::Limit(_))
                    && !self.frame_after_mark()?
                {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// 読めなかったフレームの頭（残した位置）より後ろに、次のフレームの頭（照合する番号は、そのフレームの 1 つ前）があるか。
    fn frame_after_mark(&mut self) -> Result<bool, EngineError> {
        if self.input.keep.is_none() {
            return Ok(true);
        }
        let mut rel = 1usize;
        loop {
            if rel.is_multiple_of(1 << 16) && self.input.mon.canceled() {
                return Err(EngineError::Canceled);
            }
            self.input.ensure_rel(rel + MAX_HEADER)?;
            let avail = self.input.avail_rel();
            if rel + 2 > avail {
                return Ok(false);
            }
            let head = self.input.slice_rel(rel, avail.min(rel + MAX_HEADER));
            if parse_header(head).is_some_and(|h| fits(&self.info, &h, self.seqs[1])) {
                return Ok(true);
            }
            rel += 1;
        }
    }

    /// フレームをたどって長さを数える（総フレーム数が不明なとき。数える量を上限で確かめる）。
    fn count_frames(&mut self, limits: &Limits) -> Result<u64, EngineError> {
        let mut total = 0u64;
        while let Some((_, n)) = self.next_decoded(total, None)? {
            total = total.saturating_add(n as u64);
            limits
                .check(&Shape::Audio {
                    rate: self.info.rate,
                    channels: u16::try_from(self.info.channels).unwrap_or(u16::MAX),
                    frames: Dim::Exact(total),
                })
                .map_err(|m| EngineError::Limit(format!("デコードする量が大きすぎます（{m}）")))?;
        }
        Ok(total)
    }

    /// すべてのフレームを読んで、標本を `out` に渡す（長さは `frames` ちょうど。MD5 があれば最後に照らす）。
    pub fn decode_all(&mut self, frames: u64, out: &mut DecodeOut<'_>) -> Result<(), EngineError> {
        let mut md5 = self.info.md5.map(|_| Md5::new());
        let mut done = 0u64;
        let mut f32s: Vec<Vec<f32>> = (0..self.info.channels).map(|_| Vec::new()).collect();
        while let Some((h, n)) = self.next_decoded(done, md5.as_mut())? {
            done = done.saturating_add(n as u64);
            if done > frames {
                return Err(dec_err("長さがヘッダより長い"));
            }
            let bps = h.bps.unwrap_or(self.info.bps);
            let shift = 32u32.saturating_sub(bps);
            for (dst, src) in f32s.iter_mut().zip(&self.planes) {
                dst.clear();
                dst.try_reserve(n)
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                dst.extend(src[..n].iter().map(|s| full_scale(*s << shift)));
            }
            let views: Vec<&[f32]> = f32s.iter().map(Vec::as_slice).collect();
            out.push_frames(&views)?;
        }
        if let (Some(want), Some(m)) = (self.info.md5, md5)
            && m.finalize() != want
        {
            return Err(dec_err("MD5 が合いません（壊れたファイル）"));
        }
        if done != frames {
            return Err(dec_err("長さがヘッダと合いません"));
        }
        Ok(())
    }
}

/// 左詰めした標本を −1〜1 の f32 にする（`pcm::int_to_f32(v, 32)` と同じ値：あちらは f64 で 2³¹ で割ってから f32 に丸め、
/// こちらは f32 に丸めてから 2⁻³¹ を掛ける。i32 は f64 で正確、2 の累乗で割る・掛けるのは正規の範囲で正確なので、どちらも
/// `v · 2⁻³¹` を 1 回丸めた値。試験 `full_scale_is_int_to_f32`）。4 つずつの演算にまとめられる。
#[inline]
fn full_scale(v: i32) -> f32 {
    v as f32 * (1.0 / 2_147_483_648.0)
}

/// フレームの標本（左詰めする前）を MD5 に足す（入れ物の幅のリトルエンディアン。時刻の順・チャンネルの順）。
fn md5_frame(md5: &mut Md5, planes: &[Vec<i32>], n: usize, bps: u32) {
    let width = match bps {
        0 => return,
        1..=8 => 1,
        9..=16 => 2,
        17..=24 => 3,
        _ => 4,
    };
    let mut buf = [0u8; 4096];
    let per = width * planes.len();
    let step = (buf.len() / per).max(1);
    let mut t0 = 0;
    while t0 < n {
        let t1 = (t0 + step).min(n);
        let len = (t1 - t0) * per;
        pack_frames(planes, t0, t1, width, &mut buf[..len]);
        md5.update(&buf[..len]);
        t0 = t1;
    }
}

/// 標本 `t0..t1` を、時刻の順・チャンネルの順に `width` バイトのリトルエンディアンで `out` に並べる。多い形（16 ビットの
/// モノラル・ステレオ、24 ビットのステレオ）は時刻の順に 1 つずつ書き、ほかはチャンネルごとに飛び飛びに書く（同じバイト列。
/// 試験 `packed_frames_are_the_same_bytes`）。
fn pack_frames(planes: &[Vec<i32>], t0: usize, t1: usize, width: usize, out: &mut [u8]) {
    match (width, planes) {
        (2, [l]) => {
            for (d, a) in out.as_chunks_mut::<2>().0.iter_mut().zip(&l[t0..t1]) {
                *d = (*a as i16).to_le_bytes();
            }
        }
        (2, [l, r]) => {
            for ((d, a), b) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(&l[t0..t1])
                .zip(&r[t0..t1])
            {
                let (a, b) = ((*a as i16).to_le_bytes(), (*b as i16).to_le_bytes());
                *d = [a[0], a[1], b[0], b[1]];
            }
        }
        (3, [l, r]) => {
            for ((d, a), b) in out
                .as_chunks_mut::<6>()
                .0
                .iter_mut()
                .zip(&l[t0..t1])
                .zip(&r[t0..t1])
            {
                let (a, b) = (a.to_le_bytes(), b.to_le_bytes());
                *d = [a[0], a[1], a[2], b[0], b[1], b[2]];
            }
        }
        _ => pack_frames_strided(planes, t0, t1, width, out),
    }
}

/// [`pack_frames`] の一般の形（チャンネルごとに、`width × チャンネル` バイトおきに書く）。
fn pack_frames_strided(planes: &[Vec<i32>], t0: usize, t1: usize, width: usize, out: &mut [u8]) {
    let per = width * planes.len();
    for (c, p) in planes.iter().enumerate() {
        let Some(dst) = out.get_mut(c * width..) else {
            return;
        };
        let dst = dst.chunks_mut(per);
        match width {
            1 => dst.zip(&p[t0..t1]).for_each(|(d, s)| d[0] = *s as u8),
            2 => dst
                .zip(&p[t0..t1])
                .for_each(|(d, s)| d[..2].copy_from_slice(&(*s as i16).to_le_bytes())),
            3 => dst
                .zip(&p[t0..t1])
                .for_each(|(d, s)| d[..3].copy_from_slice(&s.to_le_bytes()[..3])),
            _ => dst
                .zip(&p[t0..t1])
                .for_each(|(d, s)| d[..4].copy_from_slice(&s.to_le_bytes())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;

    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::Rng;
    use serde_json::json;
    use symphonia_core::codecs::CodecParameters;
    use symphonia_core::codecs::audio::AudioDecoder as _;
    use symphonia_core::formats::{FormatOptions, FormatReader as _, TrackType};
    use symphonia_core::io::MediaSourceStream;

    use super::*;
    use crate::codec::stream::{self, DecodeInfo};
    use crate::codec::{CodecSet, Decoder as _, pcm, symph};
    use crate::doc::DecodeOptions;
    use crate::monitor::NoMonitor;
    use crate::value::{Audio, AudioMeta, AudioSource, LoopRange, Value};

    // -------------------------------------------------------------------------------------------------------
    // 照合相手：前の `flac-symphonia-dec/1`（symphonia 0.6.1 の FLAC の読み口とデコーダ）をそのまま組んだもの

    fn old_open(data: &[u8], limits: &Limits) -> Result<symph::Opened<'static>, EngineError> {
        let mss = MediaSourceStream::new(Box::new(Cursor::new(data.to_vec())), Default::default());
        let reader = symphonia_bundle_flac::FlacReader::try_new(mss, FormatOptions::default())
            .map_err(|e| symph::sym_err(DEC_ID, e, &NoMonitor))?;
        let track = reader
            .default_track(TrackType::Audio)
            .cloned()
            .ok_or_else(|| EngineError::Unsupported("トラックがありません".into()))?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(CodecParameters::audio)
            .cloned()
            .ok_or_else(|| EngineError::Unsupported("トラックがありません".into()))?;
        let rate = params
            .sample_rate
            .filter(|r| *r > 0)
            .ok_or_else(|| dec_err("サンプルレートがありません"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|n| *n > 0)
            .ok_or_else(|| EngineError::Unsupported("チャンネルの数がありません".into()))?;
        if channels as u64 > limits.max_channels || rate > limits.max_sample_rate {
            return Err(EngineError::Limit("上限".into()));
        }
        Ok(symph::Opened {
            reader: Box::new(reader),
            track,
            params,
            rate,
            channels,
        })
    }

    /// 前の flac.rs の `decode_media` と同じ組み方。
    fn old_decode(data: &[u8]) -> Result<Value, EngineError> {
        let limits = Limits::default();
        let mon = NoMonitor;
        let mut o = old_open(data, &limits)?;
        let frames = match o.track.num_frames.filter(|n| *n > 0) {
            Some(n) => n,
            None => o.count_frames(DEC_ID, &mon, &limits)?,
        };
        let (rate, channels, bits) = (o.rate, o.channels, o.params.bits_per_sample);
        let mut o = old_open(data, &limits)?;
        let raw = o.raw_tags();
        let opts = symph::decoder_options().verify(true);
        let mut dec = symphonia_bundle_flac::FlacDecoder::try_new(&o.params, &opts)
            .map_err(|e| symph::sym_err(DEC_ID, e, &mon))?;
        stream::collect(&mon, |out| {
            out.begin(DecodeInfo {
                rate,
                channels,
                frames: Dim::Exact(frames),
                expect: frames,
            })?;
            o.decode_all(DEC_ID, &mut dec, frames, out, &mon)?;
            if dec.finalize().verify_ok == Some(false) {
                return Err(dec_err("MD5"));
            }
            if out.frames() != frames {
                return Err(dec_err("長さ"));
            }
            let mut meta = symph::meta_from_tags(&raw, frames, true, &limits);
            meta.source = Some(AudioSource {
                bits: pcm::container_bits(bits.unwrap_or(16).clamp(1, 32) as u16),
                float: false,
                rate,
            });
            out.end(meta)
        })
    }

    fn new_decode(data: &[u8]) -> Result<Value, EngineError> {
        CodecSet::pinned()
            .decode(
                &mut Cursor::new(data.to_vec()),
                "flac",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .map(|(v, _)| v)
    }

    /// 流して読む道（読み口から 64 KiB ずつ）で読んだ値のダイジェスト。
    fn new_decode_reader(data: &[u8]) -> Result<Value, EngineError> {
        let limits = Limits::default();
        let mut cur = Cursor::new(data.to_vec());
        let mut media = Media::Reader(&mut cur);
        let mut r = open(&mut media, &limits, &NoMonitor)?;
        let frames = r.frames(&limits)?;
        let mut media = Media::Reader(&mut cur);
        let mut r = open(&mut media, &limits, &NoMonitor)?;
        let (rate, channels) = (r.info.rate, r.info.channels);
        stream::collect(&NoMonitor, |out| {
            out.begin(DecodeInfo {
                rate,
                channels,
                frames: Dim::Exact(frames),
                expect: frames,
            })?;
            r.decode_all(frames, out)?;
            out.end(AudioMeta::default())
        })
    }

    fn frames_of(v: &Value) -> usize {
        match v {
            Value::Audio(a) => a.frames(),
            _ => 0,
        }
    }

    /// STREAMINFO の総フレーム数と MD5 があるか（symphonia がフレームを黙って捨てられるのは、どちらもないときだけ）。
    fn guarded(data: &[u8]) -> bool {
        let Some(si) = data.get(8..42) else {
            return false;
        };
        let total = u64::from_be_bytes([
            si[10], si[11], si[12], si[13], si[14], si[15], si[16], si[17],
        ]) & 0xf_ffff_ffff;
        total > 0 || si[18..34].iter().any(|b| *b != 0)
    }

    /// 照らした結果の数。
    #[derive(Default, Debug)]
    struct Tally {
        both: usize,
        neither: usize,
        new_only: usize,
        /// 総フレーム数も MD5 もない FLAC で、symphonia が壊れたフレームを黙って捨てたもの（自作は断るか、余分な
        /// データとして読み飛ばして全部読む）。
        dropped: usize,
    }

    /// 同じ入力を前の読み手と自作の読み手で読み、symphonia が読めたものは同じ値であることを確かめる。
    fn compare(data: &[u8], tally: &mut Tally, what: &str) {
        let old = catch_unwind(AssertUnwindSafe(|| old_decode(data)))
            .unwrap_or_else(|_| Err(dec_err("panic")));
        let new = catch_unwind(AssertUnwindSafe(|| new_decode(data)))
            .unwrap_or_else(|_| panic!("自作の読み手が panic した：{what}"));
        match (old, new) {
            (Ok(a), Ok(b)) => {
                let (da, db) = (a.digest().unwrap(), b.digest().unwrap());
                if da == db {
                    tally.both += 1;
                } else if !guarded(data) && frames_of(&a) < frames_of(&b) {
                    tally.dropped += 1;
                } else {
                    panic!(
                        "値が違う：{what}（フレーム {} / {}、{} バイト）",
                        frames_of(&a),
                        frames_of(&b),
                        data.len()
                    );
                }
                // 流して読む道も同じ標本
                let s = new_decode_reader(data).unwrap();
                let plain = |v: &Value| match v {
                    Value::Audio(a) => (0..a.channels())
                        .map(|c| {
                            let mut v = vec![0f32; a.frames()];
                            a.read(c, 0, &mut v).unwrap();
                            v
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                assert_eq!(plain(&s).len(), plain(&b).len(), "{what}");
                assert!(
                    plain(&s).iter().zip(plain(&b).iter()).all(|(x, y)| x
                        .iter()
                        .map(|v| v.to_bits())
                        .eq(y.iter().map(|v| v.to_bits()))),
                    "流して読んだ値が違う：{what}"
                );
            }
            (Ok(_), Err(e)) => {
                if guarded(data) {
                    panic!("symphonia が読めたものを断った：{what}：{e}");
                }
                tally.dropped += 1;
            }
            (Err(_), Ok(_)) => tally.new_only += 1,
            (Err(_), Err(_)) => tally.neither += 1,
        }
    }

    // -------------------------------------------------------------------------------------------------------
    // 入力

    fn fixtures() -> Vec<(String, Vec<u8>)> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
        let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "flac"))
            .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
            .collect();
        out.sort();
        assert!(out.len() >= 10, "FLAC のフィクスチャが見つかりません");
        out
    }

    /// 自作の書き出しの出力（8・16・24 ビット、1〜8 チャンネル、タグとループ点つき）。
    fn encoded() -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for (bits, channels, frames) in [
            (8, 1, 3_000),
            (16, 2, 9_000),
            (24, 2, 5_000),
            (16, 6, 4_100),
            (24, 8, 700),
        ] {
            let scale = f64::from(1u32 << (bits - 1));
            let planes = (0..channels)
                .map(|c| {
                    Arc::new(
                        (0..frames)
                            .map(|i| {
                                let w =
                                    crate::math::sin(i as f64 * (0.01 + c as f64 * 0.003)) * 0.6;
                                ((w * scale).round() / scale) as f32
                            })
                            .collect::<Vec<f32>>(),
                    )
                })
                .collect();
            let mut meta = AudioMeta::default();
            meta.tags.insert("title".into(), "題".into());
            meta.tags.insert("vorbis:TRACKNUMBER".into(), "3/12".into());
            meta.loop_range = Some(LoopRange {
                start: 10,
                end: 600,
            });
            let v = Value::Audio(Arc::new(Audio::new(44_100, planes, meta).unwrap()));
            let bytes = CodecSet::pinned()
                .encode_bytes(&v, "flac", &json!({ "bits": bits }), &NoMonitor)
                .unwrap()
                .0;
            out.push((format!("kura-enc {bits} ビット {channels} ch"), bytes));
        }
        out
    }

    /// 上位のビットから書く（試験の FLAC を作る）。
    struct W {
        buf: Vec<u8>,
        acc: u8,
        n: u32,
    }

    impl W {
        fn new() -> Self {
            Self {
                buf: Vec::new(),
                acc: 0,
                n: 0,
            }
        }
        fn bit(&mut self, b: bool) {
            self.acc = (self.acc << 1) | u8::from(b);
            self.n += 1;
            if self.n == 8 {
                self.buf.push(self.acc);
                self.acc = 0;
                self.n = 0;
            }
        }
        fn put(&mut self, v: u64, bits: u32) {
            for i in (0..bits).rev() {
                self.bit((v >> i) & 1 == 1);
            }
        }
        fn signed(&mut self, v: i64, bits: u32) {
            self.put(v as u64, bits);
        }
        fn unary(&mut self, q: u64) {
            for _ in 0..q {
                self.bit(false);
            }
            self.bit(true);
        }
        fn align(&mut self) {
            while self.n != 0 {
                self.bit(false);
            }
        }
    }

    fn below(r: &mut ChaCha8Rng, n: u64) -> u64 {
        r.next_u64() % n.max(1)
    }

    fn between(r: &mut ChaCha8Rng, lo: i64, hi: i64) -> i64 {
        lo + below(r, (hi - lo + 1) as u64) as i64
    }

    fn utf8_num(v: u64, w: &mut W) {
        if v < 0x80 {
            w.put(v, 8);
            return;
        }
        let len: u32 = match v {
            0..0x800 => 2,
            0x800..0x1_0000 => 3,
            0x1_0000..0x20_0000 => 4,
            0x20_0000..0x400_0000 => 5,
            0x400_0000..0x8000_0000 => 6,
            _ => 7,
        };
        let lead: u64 = if len == 7 {
            0xfe
        } else {
            let mask = (0xff00u64 >> len) & 0xff;
            mask | (v >> (6 * (len - 1)))
        };
        w.put(lead, 8);
        for i in (0..len - 1).rev() {
            w.put(0x80 | ((v >> (6 * i)) & 0x3f), 8);
        }
    }

    /// 分割ライスの残差（`order` から）。
    fn put_residual(r: &mut ChaCha8Rng, w: &mut W, order: usize, n: usize) {
        let method = below(r, 2) as u32;
        let pbits = 4 + method;
        let escape = (1u64 << pbits) - 1;
        let orders: Vec<u32> = (0..=8u32)
            .filter(|po| n.is_multiple_of(1 << po) && (n >> po) >= order)
            .collect();
        let po = orders[below(r, orders.len() as u64) as usize];
        w.put(u64::from(method), 2);
        w.put(u64::from(po), 4);
        let psize = n >> po;
        for p in 0..(1usize << po) {
            let count = psize - if p == 0 { order } else { 0 };
            if below(r, 8) == 0 {
                let rb = below(r, 9) as u32;
                w.put(escape, pbits);
                w.put(u64::from(rb), 5);
                for _ in 0..count {
                    let v = if rb == 0 {
                        0
                    } else {
                        between(r, -(1 << (rb - 1)), (1 << (rb - 1)) - 1)
                    };
                    w.signed(v, rb);
                }
            } else {
                let k = below(r, 13) as u32;
                w.put(u64::from(k), pbits);
                for _ in 0..count {
                    let mag = 1i64 << (k + below(r, 3) as u32);
                    let v = between(r, -mag, mag);
                    let u = ((v << 1) ^ (v >> 63)) as u64;
                    w.unary(u >> k);
                    w.put(u & ((1u64 << k) - 1), k);
                }
            }
        }
    }

    fn put_subframe(r: &mut ChaCha8Rng, w: &mut W, sub_bps: u32, n: usize) {
        let wasted = if sub_bps > 32 || below(r, 6) == 0 {
            1 + below(r, u64::from(sub_bps.min(4))) as u32
        } else {
            0
        };
        let eff = sub_bps - wasted;
        let (lo, hi) = if eff == 0 {
            (0, 0)
        } else {
            (-(1i64 << (eff - 1)), (1i64 << (eff - 1)) - 1)
        };
        let small = (lo.max(-1000), hi.min(1000));
        let kind = below(r, 10);
        w.bit(false);
        let kind_code: u64 = match kind {
            0 => 0,
            1 => 1,
            2..=5 => 8 + below(r, 5).min(n as u64),
            _ => 0x20 + below(r, 32.min(n as u64)),
        };
        w.put(kind_code, 6);
        if wasted > 0 {
            w.bit(true);
            w.unary(u64::from(wasted - 1));
        } else {
            w.bit(false);
        }
        match kind_code {
            0 => w.signed(between(r, lo, hi), eff),
            1 => {
                for _ in 0..n {
                    w.signed(between(r, lo, hi), eff);
                }
            }
            8..=12 => {
                let order = (kind_code - 8) as usize;
                for _ in 0..order {
                    w.signed(between(r, small.0, small.1), eff);
                }
                put_residual(r, w, order, n);
            }
            _ => {
                let order = (kind_code - 0x1f) as usize;
                for _ in 0..order {
                    w.signed(between(r, small.0, small.1), eff);
                }
                let precision = 1 + below(r, 15) as u32;
                let shift = below(r, 16) as u32;
                let bound =
                    ((1i64 << (precision - 1)) - 1).min((1i64 << shift) / (order as i64 + 1));
                w.put(u64::from(precision - 1), 4);
                w.put(u64::from(shift), 5);
                for _ in 0..order {
                    w.signed(between(r, -bound, bound), precision);
                }
                put_residual(r, w, order, n);
            }
        }
    }

    const TABLE_RATES: [(u64, u32); 11] = [
        (1, 88_200),
        (2, 176_400),
        (3, 192_000),
        (4, 8_000),
        (5, 16_000),
        (6, 22_050),
        (7, 24_000),
        (8, 32_000),
        (9, 44_100),
        (0xa, 48_000),
        (0xb, 96_000),
    ];
    const TABLE_BPS: [(u64, u32); 6] = [(1, 8), (2, 12), (4, 16), (5, 20), (6, 24), (7, 32)];

    /// 仕様のいろいろな枝を通る、正しい FLAC（MD5 なし）。
    fn random_stream(r: &mut ChaCha8Rng) -> Vec<u8> {
        let channels = [1usize, 2, 2, 2, 3, 6, 8][below(r, 7) as usize];
        let (bps, bps_code) = if below(r, 3) == 0 {
            (between(r, 4, 32) as u32, None)
        } else {
            let (c, b) = TABLE_BPS[below(r, 6) as usize];
            (b, Some(c))
        };
        let rate = match below(r, 4) {
            0 => TABLE_RATES[below(r, 11) as usize].1,
            1 => 1_000 * between(r, 1, 255) as u32,
            2 => between(r, 1, 65_535) as u32,
            _ => between(r, 1, 655_350) as u32,
        };
        let variable = below(r, 4) == 0;
        let block_max = if below(r, 6) == 0 {
            [576u32, 1_152, 4_096, 4_608][below(r, 4) as usize]
        } else {
            between(r, 17, 300) as u32
        };
        let block_min = if variable {
            between(r, 16, i64::from(block_max) - 1) as u32
        } else {
            block_max
        };
        let nframes = 1 + below(r, 4) as usize;
        let blocks: Vec<u32> = (0..nframes)
            .map(|i| {
                if variable || i + 1 == nframes {
                    between(r, 1, i64::from(block_max)) as u32
                } else {
                    block_max
                }
            })
            .collect();
        let total: u64 = blocks.iter().map(|b| u64::from(*b)).sum();
        let write_total = below(r, 3) != 0;
        let mut out = b"fLaC".to_vec();
        // 箱（STREAMINFO のほかは任意。最後の箱に印）
        let mut blocks_meta: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut si = W::new();
        si.put(u64::from(block_min), 16);
        si.put(u64::from(block_max), 16);
        si.put(0, 24);
        si.put(0, 24);
        si.put(u64::from(rate), 20);
        si.put(channels as u64 - 1, 3);
        si.put(u64::from(bps - 1), 5);
        si.put(if write_total { total } else { 0 }, 36);
        si.put(0, 64);
        si.put(0, 64);
        blocks_meta.push((0, si.buf));
        if below(r, 2) == 0 {
            let pool: [&[u8]; 14] = [
                b"TITLE=abc",
                b"title=\xe3\x81\x82",
                b"NOEQUALS",
                b"=empty key",
                b"CHAPTER001=00:00:00.000",
                b"chapter002NAME=x",
                b"CHAPTER01=two digits",
                b"METADATA_BLOCK_PICTURE=AAAA",
                b"coverart=zz",
                b"K\xffEY=\xff\xfe",
                b"LOOPSTART=10",
                b"LOOPLENGTH=5",
                b"ARTIST=a=b",
                b"\xe3\x81\x82KEY=v",
            ];
            let mut vc = Vec::new();
            vc.extend_from_slice(&4u32.to_le_bytes());
            vc.extend_from_slice(b"kura");
            let n = below(r, 8) as u32;
            vc.extend_from_slice(&n.to_le_bytes());
            for _ in 0..n {
                let c = pool[below(r, 14) as usize];
                vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
                vc.extend_from_slice(c);
            }
            blocks_meta.push((4, vc));
        }
        if below(r, 3) == 0 {
            blocks_meta.push((1, vec![0u8; below(r, 40) as usize]));
        }
        if below(r, 4) == 0 {
            let mut app = b"kura".to_vec();
            app.extend((0..below(r, 20)).map(|_| r.next_u32() as u8));
            blocks_meta.push((2, app));
        }
        if below(r, 4) == 0 {
            blocks_meta.push((3, vec![0u8; 18 * below(r, 3) as usize]));
        }
        if below(r, 6) == 0 {
            blocks_meta.push((100, (0..below(r, 30)).map(|_| r.next_u32() as u8).collect()));
        }
        let last = blocks_meta.len() - 1;
        for (i, (kind, body)) in blocks_meta.iter().enumerate() {
            let mut h = W::new();
            h.put(u64::from(u8::from(i == last) << 7 | *kind as u8), 8);
            h.put(body.len() as u64, 24);
            out.extend_from_slice(&h.buf);
            out.extend_from_slice(body);
        }
        if below(r, 8) == 0 {
            out.extend(std::iter::repeat_n(0u8, below(r, 20) as usize));
        }
        // フレーム
        let mut seq = 0u64;
        for (i, b) in blocks.iter().enumerate() {
            let b = *b;
            let mut w = W::new();
            w.put(0xff, 8);
            w.put(0xf8 | u64::from(variable), 8);
            // ブロックの書き方
            let mut codes: Vec<(u64, Vec<u8>)> = Vec::new();
            if b == 192 {
                codes.push((1, vec![]));
            }
            for k in 0..4 {
                if b == 576 << k {
                    codes.push((2 + k, vec![]));
                }
            }
            for k in 0..8 {
                if b == 256 << k {
                    codes.push((8 + k, vec![]));
                }
            }
            if b <= 256 {
                codes.push((6, vec![(b - 1) as u8]));
            }
            codes.push((7, ((b - 1) as u16).to_be_bytes().to_vec()));
            let (bcode, bextra) = codes[below(r, codes.len() as u64) as usize].clone();
            // レートの書き方
            let mut rcodes: Vec<(u64, Vec<u8>)> = vec![(0, vec![])];
            if let Some((c, _)) = TABLE_RATES.iter().find(|(_, v)| *v == rate) {
                rcodes.push((*c, vec![]));
            }
            if rate.is_multiple_of(1_000) && rate / 1_000 <= 255 {
                rcodes.push((0xc, vec![(rate / 1_000) as u8]));
            }
            if rate <= 65_535 {
                rcodes.push((0xd, (rate as u16).to_be_bytes().to_vec()));
            }
            if rate.is_multiple_of(10) && rate / 10 <= 65_535 {
                rcodes.push((0xe, ((rate / 10) as u16).to_be_bytes().to_vec()));
            }
            let (rcode, rextra) = rcodes[below(r, rcodes.len() as u64) as usize].clone();
            let assign: u64 = if channels == 2 && bps < 32 {
                [1, 8, 9, 10][below(r, 4) as usize]
            } else {
                channels as u64 - 1
            };
            let bcode_bps = match bps_code {
                Some(c) if below(r, 2) == 0 => c,
                _ => 0,
            };
            w.put(bcode, 4);
            w.put(rcode, 4);
            w.put(assign, 4);
            w.put(bcode_bps, 3);
            w.put(0, 1);
            utf8_num(if variable { seq } else { i as u64 }, &mut w);
            for x in bextra.iter().chain(rextra.iter()) {
                w.put(u64::from(*x), 8);
            }
            let c8 = crc8(&w.buf);
            w.put(u64::from(c8), 8);
            for c in 0..channels {
                let side = match assign {
                    8 | 10 => c == 1,
                    9 => c == 0,
                    _ => false,
                };
                put_subframe(r, &mut w, bps + u32::from(side), b as usize);
            }
            w.align();
            // ときどき、詰め物と CRC-16 のあいだに余分なバイトを入れる（symphonia は CRC-16 を次の頭の直前で照らすので読めた）
            if below(r, 12) == 0 {
                for _ in 0..1 + below(r, 3) {
                    w.put(below(r, 0xf0), 8);
                }
            }
            let c16 = w.buf.iter().fold(0u16, |c, x| crc16_byte(c, *x));
            w.put(u64::from(c16), 16);
            out.extend_from_slice(&w.buf);
            seq += u64::from(b);
        }
        if write_total && below(r, 6) == 0 {
            out.extend_from_slice(b"TAG");
            out.extend((0..125).map(|_| r.next_u32() as u8));
        }
        out
    }

    /// 壊す：書き換え・挿入・削除・切り詰め。
    fn mutate(r: &mut ChaCha8Rng, data: &[u8]) -> Vec<u8> {
        let mut b = data.to_vec();
        for _ in 0..1 + below(r, 3) {
            if b.is_empty() {
                break;
            }
            let i = below(r, b.len() as u64) as usize;
            match below(r, 4) {
                0 | 1 => b[i] ^= 1 << below(r, 8),
                2 => b.insert(i, r.next_u32() as u8),
                _ => {
                    b.remove(i);
                }
            }
        }
        if below(r, 6) == 0 {
            b.truncate(below(r, b.len() as u64 + 1) as usize);
        }
        b
    }

    // -------------------------------------------------------------------------------------------------------
    // 照合

    #[test]
    fn same_values_as_symphonia_on_fixtures_and_encoder_outputs() {
        let mut t = Tally::default();
        for (name, data) in fixtures().into_iter().chain(encoded()) {
            compare(&data, &mut t, &name);
        }
        assert_eq!(t.neither + t.new_only + t.dropped, 0, "{t:?}");
    }

    #[test]
    fn same_values_as_symphonia_on_random_valid_streams() {
        let mut r = crate::determinism::rng(216);
        let mut t = Tally::default();
        for i in 0..3_000 {
            let data = random_stream(&mut r);
            compare(&data, &mut t, &format!("乱数の FLAC {i}"));
        }
        // 正しい FLAC は（symphonia が LPC の i32 の足し算であふれる形などを除き）両方で読める
        assert!(t.both > 2_500, "{t:?}");
        assert_eq!(t.dropped, 0, "{t:?}");
    }

    #[test]
    fn same_values_as_symphonia_on_broken_files() {
        let mut r = crate::determinism::rng(9_639);
        let mut t = Tally::default();
        let mut seeds: Vec<Vec<u8>> = (0..200).map(|_| random_stream(&mut r)).collect();
        seeds.extend(encoded().into_iter().map(|(_, d)| d));
        seeds.extend(fixtures().into_iter().take(4).map(|(_, d)| d));
        for (i, s) in seeds.iter().enumerate() {
            let rounds = if s.len() > 20_000 { 20 } else { 40 };
            for j in 0..rounds {
                let m = mutate(&mut r, s);
                compare(&m, &mut t, &format!("壊した FLAC {i}-{j}"));
            }
        }
        assert!(t.both > 100 && t.neither > 1_000, "{t:?}");
    }

    /// 速さの目安（release で `--ignored`）：5 分のステレオ 16 ビットを前の読み手と自作の読み手で読む（M16 段 1 の実測：
    /// M4 Max で symphonia 0.175 秒、自作 0.19 秒。うち MD5 が 0.075 秒）。
    #[test]
    #[ignore]
    // 計るだけの試験（値は時刻によらない）
    #[allow(clippy::disallowed_methods)]
    fn speed_against_symphonia() {
        let frames = 44_100 * 300;
        let planes = (0..2)
            .map(|c| {
                Arc::new(
                    (0..frames)
                        .map(|i| {
                            let w = crate::math::sin(i as f64 * (0.01 + c as f64 * 0.003)) * 0.5
                                + crate::math::sin(i as f64 * 0.37) * 0.1;
                            ((w * 32_768.0).round() / 32_768.0) as f32
                        })
                        .collect::<Vec<f32>>(),
                )
            })
            .collect();
        let v = Value::Audio(Arc::new(
            Audio::new(44_100, planes, AudioMeta::default()).unwrap(),
        ));
        let data = CodecSet::pinned()
            .encode_bytes(&v, "flac", &json!({ "bits": 16 }), &NoMonitor)
            .unwrap()
            .0;
        let mut best = [f64::MAX; 2];
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let a = old_decode(&data).unwrap();
            best[0] = best[0].min(t.elapsed().as_secs_f64());
            // 前の道と同じく、値のダイジェスト（CodecSet が読み取りの記録に付ける）を含めずに計る
            let t = std::time::Instant::now();
            let b = crate::codec::flac::FlacCodec
                .decode(
                    &mut Cursor::new(&data[..]),
                    "flac",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .unwrap();
            best[1] = best[1].min(t.elapsed().as_secs_f64());
            assert_eq!(a.digest().unwrap(), b.digest().unwrap());
        }
        println!(
            "{} バイト：symphonia {:.3} 秒、自作 {:.3} 秒",
            data.len(),
            best[0],
            best[1]
        );
    }

    /// STREAMINFO の総フレーム数と MD5 を 0 にする（「不明」にする）。
    fn unguard(data: &mut [u8]) {
        let si = &mut data[8..42];
        si[13] &= 0xf0;
        si[14..18].fill(0);
        si[18..34].fill(0);
    }

    /// 最初のフレームの頭と、2 つめのフレームの頭（固定のブロックの番号 1）。
    fn first_two_frames(data: &[u8]) -> (usize, usize) {
        let first = {
            let mut m = Media::Bytes(data);
            let r = open(&mut m, &Limits::default(), &NoMonitor).unwrap();
            r.input.pos
        };
        let second = (first + 2..data.len())
            .find(|&i| parse_header(&data[i..]).is_some_and(|h| h.seq == 1))
            .unwrap();
        (first, second)
    }

    #[test]
    fn truncated_last_frame_without_total_is_dropped_like_symphonia() {
        let (_, mut data) = encoded().into_iter().nth(1).unwrap();
        unguard(&mut data);
        let full = new_decode(&data).unwrap();
        for cut in [1usize, 7, 100, 2_000] {
            let d = &data[..data.len() - cut];
            let (a, b) = (old_decode(d).unwrap(), new_decode(d).unwrap());
            assert_eq!(a.digest().unwrap(), b.digest().unwrap(), "{cut}");
            assert!(frames_of(&b) < frames_of(&full), "{cut}");
        }
        // 総フレーム数があれば、切れたファイルは読まない
        let (_, guarded) = encoded().into_iter().nth(1).unwrap();
        assert!(new_decode(&guarded[..guarded.len() - 7]).is_err());
        // 壊れたフレームのあとにフレームが続けば、壊れたファイル（symphonia は壊れたフレームを黙って捨てていた）
        let (first, second) = first_two_frames(&data);
        let mut broken = data.clone();
        broken[(first + second) / 2] ^= 0x55;
        assert!(new_decode(&broken).is_err());
    }

    #[test]
    fn extra_bytes_before_the_crc_are_accepted_like_symphonia() {
        let (_, data) = encoded().into_iter().nth(1).unwrap();
        let (first, second) = first_two_frames(&data);
        // 1 つめのフレームの CRC-16 の前に 3 バイト足し、CRC-16 を付け直す（標本は同じなので MD5 も合う）
        let mut frame = data[first..second - 2].to_vec();
        frame.extend_from_slice(&[0x12, 0x00, 0x34]);
        let c = frame.iter().fold(0u16, |c, b| crc16_byte(c, *b));
        frame.extend_from_slice(&c.to_be_bytes());
        let mut padded = data[..first].to_vec();
        padded.extend_from_slice(&frame);
        padded.extend_from_slice(&data[second..]);
        let (a, b) = (old_decode(&padded).unwrap(), new_decode(&padded).unwrap());
        assert_eq!(a.digest().unwrap(), b.digest().unwrap());
        assert_eq!(
            b.digest().unwrap(),
            new_decode(&data).unwrap().digest().unwrap()
        );
        assert_eq!(
            new_decode_reader(&padded).unwrap().digest().unwrap(),
            new_decode_reader(&data).unwrap().digest().unwrap()
        );
    }

    /// 32 ビットのステレオの 1 フレーム（16 フレーム、生の値）。差のチャンネルは 33 ビット。
    fn stereo32(assign: u64, ch0: &[i64], ch1: &[i64], side_constant: bool) -> Vec<u8> {
        let n = ch0.len();
        let mut out = b"fLaC".to_vec();
        let mut si = W::new();
        si.put(16, 16);
        si.put(16, 16);
        si.put(0, 48);
        si.put(48_000, 20);
        si.put(1, 3);
        si.put(31, 5);
        si.put(n as u64, 36);
        si.put(0, 64);
        si.put(0, 64);
        out.extend_from_slice(&[0x80, 0, 0, 34]);
        out.extend_from_slice(&si.buf);
        let mut w = W::new();
        w.put(0xfff8, 16);
        w.put(0x6, 4);
        w.put(0, 4);
        w.put(assign, 4);
        w.put(7, 3);
        w.put(0, 1);
        w.put(0, 8);
        w.put(n as u64 - 1, 8);
        let c8 = crc8(&w.buf);
        w.put(u64::from(c8), 8);
        for (c, xs) in [ch0, ch1].into_iter().enumerate() {
            let side = match assign {
                8 | 10 => c == 1,
                _ => c == 0,
            };
            let bits = 32 + u32::from(side);
            w.bit(false);
            if side && side_constant {
                w.put(0, 6);
                w.bit(false);
                w.signed(xs[0], bits);
            } else {
                w.put(1, 6);
                w.bit(false);
                for x in xs {
                    w.signed(*x, bits);
                }
            }
        }
        w.align();
        let c16 = w.buf.iter().fold(0u16, |c, x| crc16_byte(c, *x));
        w.put(u64::from(c16), 16);
        out.extend_from_slice(&w.buf);
        out
    }

    #[test]
    fn side_channels_of_32_bit_stereo_are_33_bits() {
        let l: Vec<i64> = (0..16)
            .map(|i| [i64::from(i32::MAX), i64::from(i32::MIN), 7 * i - 50][i as usize % 3])
            .collect();
        let r: Vec<i64> = (0..16)
            .map(|i| [i64::from(i32::MIN), i64::from(i32::MAX), 3 - 11 * i][i as usize % 3])
            .collect();
        let s: Vec<i64> = l.iter().zip(&r).map(|(l, r)| l - r).collect();
        let m: Vec<i64> = l.iter().zip(&r).map(|(l, r)| (l + r) >> 1).collect();
        let want = |v: &Value| {
            let Value::Audio(a) = v else { panic!() };
            let mut out = Vec::new();
            for c in 0..2 {
                let mut p = vec![0f32; a.frames()];
                a.read(c, 0, &mut p).unwrap();
                out.push(p);
            }
            out
        };
        let as_f32 = |xs: &[i64]| {
            xs.iter()
                .map(|x| pcm::int_to_f32(*x as i32, 32))
                .collect::<Vec<f32>>()
        };
        for (assign, ch0, ch1) in [(8, &l, &s), (9, &s, &r), (10, &m, &s)] {
            let data = stereo32(assign, ch0, ch1, false);
            let v = new_decode(&data).unwrap();
            assert_eq!(want(&v), vec![as_f32(&l), as_f32(&r)], "{assign}");
            assert_eq!(new_decode_reader(&data).unwrap().digest().unwrap(), {
                let mut media = Media::Bytes(&data);
                let mut rd = open(&mut media, &Limits::default(), &NoMonitor).unwrap();
                let fr = rd.frames(&Limits::default()).unwrap();
                stream::collect(&NoMonitor, |out| {
                    out.begin(DecodeInfo {
                        rate: 48_000,
                        channels: 2,
                        frames: Dim::Exact(fr),
                        expect: fr,
                    })?;
                    rd.decode_all(fr, out)?;
                    out.end(AudioMeta::default())
                })
                .unwrap()
                .digest()
                .unwrap()
            });
        }
        // 左右が同じなら差は 0 の定数（33 ビット）。symphonia の release はこの形を正しく読めていた
        let same: Vec<i64> = l.clone();
        let data = stereo32(8, &same, &[0; 16], true);
        let v = new_decode(&data).unwrap();
        assert_eq!(want(&v), vec![as_f32(&same), as_f32(&same)]);
        if !cfg!(debug_assertions) {
            assert_eq!(
                old_decode(&data).unwrap().digest().unwrap(),
                v.digest().unwrap()
            );
        }
    }

    // -------------------------------------------------------------------------------------------------------
    // 細部

    /// [`full_scale`] が `pcm::int_to_f32(v, 32)` と同じビット（端の値、0 の近く、2 の累乗の前後、16・24 ビットを左詰めした
    /// 値、ChaCha8 の値）。
    #[test]
    fn full_scale_is_int_to_f32() {
        let mut r = crate::determinism::rng(59);
        let mut vals: Vec<i32> = vec![i32::MIN, i32::MIN + 1, i32::MAX, i32::MAX - 1, -1, 0, 1];
        for k in 0..31 {
            for d in -3..=3 {
                vals.push((1i32 << k).wrapping_add(d));
                vals.push((-(1i64 << k) as i32).wrapping_add(d));
            }
        }
        vals.extend((i32::from(i16::MIN)..=i32::from(i16::MAX)).map(|v| v << 16));
        vals.extend((0..200_000).map(|_| (r.next_u32() as i32) & !0xff));
        vals.extend((0..200_000).map(|_| r.next_u32() as i32));
        for v in vals {
            assert_eq!(
                full_scale(v).to_bits(),
                pcm::int_to_f32(v, 32).to_bits(),
                "{v}"
            );
        }
    }

    /// 速い並べ方（16 ビットのモノラル・ステレオ、24 ビットのステレオ）が、一般の形と同じバイト列（幅 1〜4 × チャンネル
    /// 1〜8 × 区間、ChaCha8 の標本）。
    #[test]
    fn packed_frames_are_the_same_bytes() {
        let mut r = crate::determinism::rng(53);
        for width in 1..=4usize {
            for ch in 1..=8usize {
                let n = 300usize;
                let planes: Vec<Vec<i32>> = (0..ch)
                    .map(|_| (0..n).map(|_| r.next_u32() as i32).collect())
                    .collect();
                for (t0, t1) in [(0, n), (0, 1), (5, 6), (17, 250), (n, n)] {
                    let len = (t1 - t0) * width * ch;
                    let (mut a, mut b) = (vec![0xaau8; len], vec![0x55u8; len]);
                    pack_frames(&planes, t0, t1, width, &mut a);
                    pack_frames_strided(&planes, t0, t1, width, &mut b);
                    assert_eq!(a, b, "{width} {ch} {t0} {t1}");
                }
            }
        }
    }

    /// 固定の予測の前の形（symphonia と同じく予測を i64 で求め、i32 の折り返しで足す）。
    fn fixed_predict_direct(order: usize, buf: &mut [i32]) {
        // 直前の標本を変数に持って回す（予測は i64、足し算は i32 の折り返し。symphonia と同じ）
        match order {
            1 => {
                let Some((first, rest)) = buf.split_first_mut() else {
                    return;
                };
                let mut a = *first;
                for v in rest {
                    *v = v.wrapping_add(a);
                    a = *v;
                }
            }
            2 if buf.len() > 2 => {
                let (mut a, mut b) = (i64::from(buf[0]), i64::from(buf[1]));
                for v in &mut buf[2..] {
                    *v = v.wrapping_add((2 * b - a) as i32);
                    (a, b) = (b, i64::from(*v));
                }
            }
            3 if buf.len() > 3 => {
                let (mut a, mut b, mut c) =
                    (i64::from(buf[0]), i64::from(buf[1]), i64::from(buf[2]));
                for v in &mut buf[3..] {
                    *v = v.wrapping_add((3 * c - 3 * b + a) as i32);
                    (a, b, c) = (b, c, i64::from(*v));
                }
            }
            4 if buf.len() > 4 => {
                let (mut a, mut b, mut c, mut d) = (
                    i64::from(buf[0]),
                    i64::from(buf[1]),
                    i64::from(buf[2]),
                    i64::from(buf[3]),
                );
                for v in &mut buf[4..] {
                    *v = v.wrapping_add((4 * d - 6 * c + 4 * b - a) as i32);
                    (a, b, c, d) = (b, c, d, i64::from(*v));
                }
            }
            _ => {}
        }
    }

    /// 階差で回す固定の予測が、前の形と同じ値（次数 0〜5、長さ 0〜40、ChaCha8 の残差と頭の標本。i32 の端の値も混ぜて
    /// 折り返しを通す）。
    #[test]
    fn fixed_predict_matches_the_direct_form() {
        let mut r = crate::determinism::rng(47);
        for round in 0..4_000 {
            let len = (r.next_u32() % 41) as usize;
            let buf: Vec<i32> = (0..len)
                .map(|_| match r.next_u32() % 8 {
                    0 => i32::MIN,
                    1 => i32::MAX,
                    2 => r.next_u32() as i32,
                    _ => (r.next_u32() % 2001) as i32 - 1000,
                })
                .collect();
            let order = round % 6;
            let (mut a, mut b) = (buf.clone(), buf);
            fixed_predict(order, &mut a);
            fixed_predict_direct(order, &mut b);
            assert_eq!(a, b, "{order} {len}");
        }
    }

    #[test]
    fn crc_tables_match_the_bitwise_definitions() {
        let bitwise16 = |data: &[u8]| {
            let mut c = 0u16;
            for &b in data {
                c ^= u16::from(b) << 8;
                for _ in 0..8 {
                    c = if c & 0x8000 != 0 {
                        (c << 1) ^ 0x8005
                    } else {
                        c << 1
                    };
                }
            }
            c
        };
        let bitwise8 = |data: &[u8]| {
            let mut c = 0u8;
            for &b in data {
                c ^= b;
                for _ in 0..8 {
                    c = if c & 0x80 != 0 {
                        (c << 1) ^ 0x07
                    } else {
                        c << 1
                    };
                }
            }
            c
        };
        let data: Vec<u8> = (0..=255u8)
            .chain((0..300).map(|i| (i * 7 % 256) as u8))
            .collect();
        for n in [0, 1, 2, 17, 256, data.len()] {
            assert_eq!(crc8(&data[..n]), bitwise8(&data[..n]));
            assert_eq!(
                data[..n].iter().fold(0u16, |c, b| crc16_byte(c, *b)),
                bitwise16(&data[..n])
            );
        }
        // 8 バイトずつ進める形は、どの長さでも 1 バイトずつと同じ
        for n in 0..data.len() {
            assert_eq!(crc16(&data[..n]), bitwise16(&data[..n]), "{n}");
        }
    }

    #[test]
    fn comments_follow_the_symphonia_rules() {
        let mut tags = Vec::new();
        let mut vc = Vec::new();
        vc.extend_from_slice(&0u32.to_le_bytes());
        let items: [&[u8]; 9] = [
            b"TITLE=a=b",
            b"NOEQUALS",
            b"CHAPTER001=x",
            b"chapter123name=x",
            b"CHAPTER12=x",
            b"Metadata_Block_Picture=AAAA",
            b"COVERART=x",
            b"\xe3\x81\x82K\x7fEY=\xff",
            b"=v",
        ];
        vc.extend_from_slice(&(items.len() as u32).to_le_bytes());
        for c in items {
            vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
            vc.extend_from_slice(c);
        }
        parse_comments(&vc, &mut tags).unwrap();
        assert_eq!(
            tags,
            vec![
                ("TITLE".to_string(), "a=b".to_string()),
                ("CHAPTER12".to_string(), "x".to_string()),
                ("KEY".to_string(), "\u{fffd}".to_string()),
                (String::new(), "v".to_string()),
            ]
        );
        // 箱の中で切れていれば壊れたファイル
        assert!(parse_comments(&vc[..vc.len() - 1], &mut Vec::new()).is_err());
    }

    #[test]
    fn trailing_data_and_gaps() {
        let (_, good) = encoded().into_iter().nth(1).unwrap();
        // 総フレーム数があれば、最後のフレームのあとの ID3v1 は読み飛ばす
        let mut tagged = good.clone();
        tagged.extend_from_slice(b"TAG");
        tagged.extend_from_slice(&[0u8; 125]);
        assert_eq!(
            new_decode(&tagged).unwrap().digest().unwrap(),
            new_decode(&good).unwrap().digest().unwrap()
        );
        // フレームのあいだの余分なバイトは壊れたファイル（最初のフレームの頭は 2 つめの箱のあと）
        let first = {
            let mut m = Media::Bytes(&good);
            let r = open(&mut m, &Limits::default(), &NoMonitor).unwrap();
            r.input.pos
        };
        let second = (first + 2..good.len())
            .find(|&i| parse_header(&good[i..]).is_some_and(|h| h.seq == 1))
            .unwrap();
        let mut gap = good.clone();
        gap.insert(second, 0);
        assert!(new_decode(&gap).is_err());
        // 頭の目印がなければ次の段へ
        assert!(matches!(
            new_decode(&good[4..]),
            Err(EngineError::Unsupported(_))
        ));
    }
}
