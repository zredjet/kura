//! PNG の読み口（M16 段 6。png クレート 0.18.1 の読み取りの代わり。docs/editing.md §9.5、ADR-223）。
//!
//! `png-rs-dec/1`・`/2` の識別子のまま実装を替えるので、png クレート（kura の使い方：上限 `Limits { bytes }`・変換なし・形だけ
//! 読むときは iCCP とテキストを読まない）が読めた PNG は同じ値（画素・メタデータ）で読む（利用者の決定）。png クレートの甘さ
//! もまねる：
//! - CRC：読んだチャンクだけ照らす。必須チャンクの誤りは壊れたファイル、補助チャンクの誤りはそのチャンクを黙って捨てる。IDAT の
//!   CRC は、その先のデータを展開に本当に使ったときだけ照らす（png クレートは要る行が揃ったら先を読まないので、静止画の最後の
//!   IDAT の CRC は見ない。こちらの展開は先読みするので、誤りを覚えておき、使ったビットで決める）。APNG で次のフレームへ進む
//!   ときは、残りの IDAT を読み通して照らす。fdAT（補助）の CRC の誤りは無視（中身はもう使っている）。
//! - IDAT の zlib の Adler-32 は見ない。流れの終わりの尾・あとのデータ・後ろのチャンク・IEND は、要る行が揃えば読まない。
//! - 展開の受け付けは fdeflate と同じ（`zlib::Flavor::Fdeflate`：固定のブロックの 286・287 は end-of-block など）。
//! - チャンクの層（長さの範囲・tRNS・PLTE・iCCP・テキスト・pHYs・acTL・fcTL・fdAT の番号）と上限の数え方（作業領域の伸び・
//!   テキストの長さ・ICC の展開）は png クレートと同じ。
//!
//! png クレートが読み込み単位（8 KiB）の境目・幅 4 以下のインターレースの数え違い・APNG のフレームごとの 1 行の上限の積み上げ・
//! cICP と bKGD の値で断っていたものは読む（前の値がないので識別子の約束に反しない。利用者の決定）。既定の画像の 1 行分を上限から
//! 引くのは同じ（1 行が上限を超える画像は断る）。APNG のフレームの間の tRNS・PLTE は
//! 使わない（png クレートは足して、kura の前の読み手はフレームの途中から画素の幅がずれた。利用者の決定）。

use std::io::Read;

use crate::codec::zlib::{self, Flavor, Inflate, Input, InputError};
use crate::error::EngineError;

/// 読み取りの識別子（誤りの文に付ける。旧版の `/1` も同じ読み口なので、前と同じく `/2`）。
const CODEC: &str = "png-rs-dec/2";

/// 署名。
const SIG: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

fn err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: CODEC.to_string(),
        message: message.into(),
    }
}

fn io_err(e: std::io::Error) -> EngineError {
    err(format!("読めません: {e}"))
}

fn zerr(e: zlib::Error) -> EngineError {
    match e {
        zlib::Error::Z(z) => err(z.to_string()),
        zlib::Error::Input(e) => e,
    }
}

/// 色の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorType {
    Grayscale,
    Rgb,
    Indexed,
    GrayscaleAlpha,
    Rgba,
}

impl ColorType {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Grayscale,
            2 => Self::Rgb,
            3 => Self::Indexed,
            4 => Self::GrayscaleAlpha,
            6 => Self::Rgba,
            _ => return None,
        })
    }

    /// 1 画素のサンプルの数。
    pub(crate) fn samples(self) -> usize {
        match self {
            Self::Grayscale | Self::Indexed => 1,
            Self::GrayscaleAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }
}

/// fcTL の中身。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameControl {
    pub sequence_number: u32,
    pub width: u32,
    pub height: u32,
    pub x_offset: u32,
    pub y_offset: u32,
    pub delay_num: u16,
    pub delay_den: u16,
    pub dispose_op: u8,
    pub blend_op: u8,
}

/// 最初の IDAT より前に読んだもの（png クレートの `read_info` の結果と同じ）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Info {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_type: Option<ColorType>,
    pub interlaced: bool,
    pub palette: Option<Vec<u8>>,
    pub trns: Option<Vec<u8>>,
    pub icc_profile: Option<Vec<u8>>,
    /// pHYs（x・y・単位）。
    pub pixel_dims: Option<(u32, u32, u8)>,
    /// tEXt（キーワード・本文。Latin-1 で読む）。
    pub uncompressed_latin1_text: Vec<(String, String)>,
    /// 圧縮していない iTXt（キーワード・本文）。
    pub utf8_text: Vec<(String, String)>,
    /// acTL（フレームの数・繰り返し）。
    pub animation_control: Option<(u32, u32)>,
    /// 最後に読んだ fcTL。
    pub frame_control: Option<FrameControl>,
}

impl Info {
    /// 色の種類（IHDR を読んだあとはいつもある）。
    pub(crate) fn color(&self) -> ColorType {
        self.color_type.unwrap_or(ColorType::Grayscale)
    }
}

/// 読み方。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// png クレートの `Limits.bytes` と同じ上限（作業領域の伸び・テキストの長さ・ICC の展開に使う）。
    pub limit: u64,
    /// 形だけ読む（iCCP とテキストを読み飛ばし、それらの分の上限は使わない。作業領域の伸びと既定の画像の 1 行分は、png クレート
    /// と同じく形だけ読むときも上限から引く）。
    pub header_only: bool,
}

/// `n` バイトちょうど読む。足りなければ（途中で終われば）None。
fn read_full<R: Read>(src: &mut R, buf: &mut [u8]) -> Result<bool, EngineError> {
    let mut got = 0;
    while got < buf.len() {
        match src.read(&mut buf[got..]) {
            Ok(0) => return Ok(false),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(io_err(e)),
        }
    }
    Ok(true)
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// 必須チャンク（型の 1 バイト目の bit5 が 0）か。
fn critical(kind: &[u8; 4]) -> bool {
    kind[0] & 0x20 == 0
}

/// 既知の補助チャンクの長さの範囲（png クレートの `start_chunk` と同じ。範囲の外は中身を持たずに捨てる）。
fn ancillary_range(kind: &[u8; 4]) -> Option<(u32, u32)> {
    const BIG: u32 = 0x7fff_ffff;
    Some(match kind {
        b"sBIT" => (1, 4),
        b"tRNS" => (1, 256),
        b"pHYs" => (9, 9),
        b"gAMA" => (4, 4),
        b"acTL" => (8, 8),
        b"fcTL" => (26, 26),
        b"cHRM" => (32, 32),
        b"sRGB" => (1, 1),
        b"cICP" => (4, 4),
        b"mDCV" => (24, 24),
        b"cLLI" => (8, 8),
        b"bKGD" => (1, 6),
        b"eXIf" | b"iCCP" | b"tEXt" | b"zTXt" | b"iTXt" => (0, BIG),
        _ => return None,
    })
}

/// 上限（png クレートの `Limits` と、チャンクの作業領域の容量）。
struct Budget {
    left: u64,
    /// チャンクの作業領域の容量（png クレートは 128 から倍々に伸ばし、縮めない）。
    cap: u64,
}

impl Budget {
    /// `n` バイトの中身を読むために作業領域を伸ばす（伸ばす分を上限から引く。尽きたら誤り）。
    fn grow(&mut self, n: u64) -> Result<(), EngineError> {
        while self.cap < n {
            if self.left == 0 {
                return Err(err("メタデータの上限を超えます"));
            }
            let r = self.cap.min(self.left);
            self.left -= r;
            self.cap += r;
        }
        Ok(())
    }

    /// `n` バイトを上限から引く（足りなければ誤り）。
    fn reserve(&mut self, n: u64) -> Result<(), EngineError> {
        if n > self.left {
            return Err(err("メタデータの上限を超えます"));
        }
        self.left -= n;
        Ok(())
    }
}

/// IDAT・fdAT の列を展開の入力にする（チャンクをまたいで中身を渡す。CRC と fdAT の番号を照らす）。
pub(crate) struct Chunks<R: Read> {
    src: R,
    /// 今の列の型（IDAT か fdAT）。
    kind: [u8; 4],
    /// 今のチャンクの中身の残り。
    left: u32,
    /// 今のチャンクの CRC（型と、ここまでの中身）。
    crc: u32,
    /// 列が終わった（次の頭を読んで違う型だった・ファイルが終わった）。
    ended: bool,
    /// 列のあとの次のチャンクの頭（型・長さ）。
    pending: Option<([u8; 4], u32)>,
    /// この流れで渡したバイトの数。
    delivered: u64,
    /// CRC が合わなかった最初の IDAT の終わり（この流れの中の位置）。
    bad_crc_at: Option<u64>,
    /// 最後の番号（fcTL・fdAT。次は +1）。
    seq: Option<u32>,
    /// 先読みで続きのチャンクを始めたときの誤り（fdAT の番号・長さ）。png クレートはそのデータが要るときか次のフレームへ進む
    /// ときにだけ読むので、ここで列を終えて覚えておく（データが要れば展開が切れた流れとして断り、進むときにこの誤りを返す）。
    deferred: Option<EngineError>,
}

impl<R: Read> Chunks<R> {
    /// 中身の `n` バイトを読み捨てる（CRC に足す）。足りなければ誤り。
    fn skip_data(&mut self, mut n: u64) -> Result<(), EngineError> {
        let mut buf = [0u8; 8192];
        while n > 0 {
            let k = (n as usize).min(buf.len());
            if !read_full(&mut self.src, &mut buf[..k])? {
                return Err(err("チャンクが途中で切れています"));
            }
            self.crc = zlib::crc32_update(self.crc, &buf[..k]);
            n -= k as u64;
        }
        Ok(())
    }

    /// 次のチャンクの頭を読む（`pending` があればそれ）。ファイルが終わっていれば None。
    fn header(&mut self) -> Result<Option<([u8; 4], u32)>, EngineError> {
        if let Some(h) = self.pending.take() {
            return Ok(Some(h));
        }
        let mut h = [0u8; 8];
        if !read_full(&mut self.src, &mut h)? {
            return Ok(None);
        }
        Ok(Some(([h[4], h[5], h[6], h[7]], be32(&h))))
    }

    /// CRC の 4 バイトを読んで照らす（読めなければ None）。
    fn read_crc(&mut self) -> Result<Option<bool>, EngineError> {
        let mut c = [0u8; 4];
        if !read_full(&mut self.src, &mut c)? {
            return Ok(None);
        }
        Ok(Some(be32(&c) == self.crc))
    }

    /// 番号を確かめる（前 +1。最初は 0）。
    fn check_seq(&mut self, n: u32) -> Result<(), EngineError> {
        let want = self.seq.map_or(Some(0), |s| s.checked_add(1));
        if want != Some(n) {
            return Err(err("APNG の番号が飛んでいます"));
        }
        self.seq = Some(n);
        Ok(())
    }

    /// `kind` の長さ `len` のチャンクから列を始める（fdAT なら番号を読んで照らす）。
    fn begin(&mut self, kind: [u8; 4], len: u32) -> Result<(), EngineError> {
        self.kind = kind;
        self.crc = zlib::crc32(&kind);
        self.ended = false;
        if &kind == b"fdAT" {
            if len < 4 {
                return Err(err("fdAT が短すぎます"));
            }
            let mut s = [0u8; 4];
            if !read_full(&mut self.src, &mut s)? {
                return Err(err("チャンクが途中で切れています"));
            }
            self.crc = zlib::crc32_update(self.crc, &s);
            self.check_seq(be32(&s))?;
            self.left = len - 4;
        } else {
            self.left = len;
        }
        Ok(())
    }

    /// 今のチャンクの中身を終えたあと：CRC を照らし、同じ型が続けば次へ、違えば列を終える。
    fn next_chunk(&mut self) -> Result<(), EngineError> {
        match self.read_crc()? {
            None => {
                self.ended = true;
                return Ok(());
            }
            Some(false) if &self.kind == b"IDAT" => {
                self.bad_crc_at.get_or_insert(self.delivered);
            }
            Some(_) => {}
        }
        match self.header()? {
            Some((kind, len)) if kind == self.kind => {
                if let Err(e) = self.begin(kind, len) {
                    self.deferred = Some(e);
                    self.ended = true;
                }
                Ok(())
            }
            other => {
                self.pending = other;
                self.ended = true;
                Ok(())
            }
        }
    }

    /// 列の残りを展開せずに読み通す（CRC と番号を照らす。IDAT の CRC の誤りはここで壊れたファイル）。
    fn skip_run(&mut self) -> Result<(), EngineError> {
        while !self.ended {
            let n = u64::from(self.left);
            self.skip_data(n)?;
            self.delivered += n;
            self.left = 0;
            self.next_chunk()?;
        }
        if let Some(e) = self.deferred.take() {
            return Err(e);
        }
        if self.bad_crc_at.is_some() {
            return Err(err("IDAT の CRC が合いません"));
        }
        Ok(())
    }
}

impl<R: Read> Input for Chunks<R> {
    fn more(&mut self, dst: &mut Vec<u8>) -> Result<usize, InputError> {
        loop {
            if self.ended {
                return Ok(0);
            }
            if self.left > 0 {
                let n = (self.left as usize).min(64 * 1024);
                let at = dst.len();
                dst.resize(at + n, 0);
                let mut got = 0;
                while got < n {
                    match self.src.read(&mut dst[at + got..at + n]) {
                        Ok(0) => break,
                        Ok(k) => got += k,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(InputError(io_err(e))),
                    }
                }
                dst.truncate(at + got);
                self.crc = zlib::crc32_update(self.crc, &dst[at..]);
                self.left -= got as u32;
                self.delivered += got as u64;
                if got < n {
                    // ファイルが途中で終わった
                    self.ended = true;
                }
                if got > 0 {
                    return Ok(got);
                }
                continue;
            }
            self.next_chunk().map_err(InputError)?;
        }
    }
}

/// 今のフレームの行の進み。
#[derive(Debug, Clone, Copy)]
struct Rows {
    w: u32,
    h: u32,
    interlaced: bool,
    /// Adam7 の段（0〜6。インターレースでなければ 0 だけ）。
    pass: usize,
    /// 段の中の行。
    line: u32,
    /// 終わった。
    done: bool,
}

/// Adam7 の各段の（x の始め・y の始め・x の間隔・y の間隔）。
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

impl Rows {
    fn new(w: u32, h: u32, interlaced: bool) -> Self {
        let mut r = Self {
            w,
            h,
            interlaced,
            pass: 0,
            line: 0,
            done: false,
        };
        r.skip_empty();
        r
    }

    /// 段の幅と行の数。
    fn dims(&self) -> (u32, u32) {
        if !self.interlaced {
            return (self.w, self.h);
        }
        let (x0, y0, dx, dy) = ADAM7[self.pass];
        let pw = if self.w > x0 {
            (self.w - x0).div_ceil(dx)
        } else {
            0
        };
        let ph = if self.h > y0 {
            (self.h - y0).div_ceil(dy)
        } else {
            0
        };
        (pw, ph)
    }

    /// 幅か行の数が 0 の段を飛ばす。
    fn skip_empty(&mut self) {
        loop {
            let (pw, ph) = self.dims();
            if pw > 0 && ph > 0 && self.line < ph {
                return;
            }
            if !self.interlaced || self.pass == 6 {
                self.done = true;
                return;
            }
            self.pass += 1;
            self.line = 0;
        }
    }

    /// 次の行へ。段が変わったら真。
    fn advance(&mut self) -> bool {
        self.line += 1;
        let (_, ph) = self.dims();
        if self.line < ph {
            return false;
        }
        if !self.interlaced || self.pass == 6 {
            self.done = true;
            return true;
        }
        self.pass += 1;
        self.line = 0;
        self.skip_empty();
        true
    }
}

/// PNG の読み手。
pub(crate) struct Reader<R: Read> {
    info: Info,
    z: Inflate<Chunks<R>>,
    rows: Rows,
    /// 前の行と今の行（フィルタのバイトを含む）。
    prev: Vec<u8>,
    cur: Vec<u8>,
    /// 段の最初の行（前の行は 0）。
    first_line: bool,
    /// 残りのフレーム（`next_frame_info` で呼べる数）。
    frames_left: u32,
}

/// 1 フレームの展開の総量（フィルタのバイトを含む。段の幅か行の数が 0 の段は数えない）。展開の置き場の見込みだけに使う。
fn frame_bytes(w: u32, h: u32, interlaced: bool, color: ColorType, depth: u8) -> u64 {
    let mut rows = Rows::new(w, h, interlaced);
    let mut total = 0u64;
    while !rows.done {
        let (pw, ph) = rows.dims();
        let len = row_bytes(pw, color.samples(), depth).unwrap_or(usize::MAX) as u64;
        total = total.saturating_add(len.saturating_add(1).saturating_mul(u64::from(ph)));
        if !rows.interlaced || rows.pass == 6 {
            break;
        }
        rows.pass += 1;
        rows.line = 0;
        rows.skip_empty();
    }
    total
}

/// 1 行のバイト数（フィルタのバイトを除く）。
fn row_bytes(w: u32, samples: usize, depth: u8) -> Option<usize> {
    (w as usize)
        .checked_mul(samples)?
        .checked_mul(usize::from(depth))
        .map(|bits| bits.div_ceil(8))
}

/// `src` の PNG を、最初の IDAT の頭まで読む（png クレートの `read_info` と同じ）。
pub(crate) fn read_info<R: Read>(mut src: R, opts: Options) -> Result<Reader<R>, EngineError> {
    let mut sig = [0u8; 8];
    if !read_full(&mut src, &mut sig)? {
        return Err(err("PNG ではありません"));
    }
    if sig != SIG {
        return Err(err("PNG ではありません"));
    }
    let mut info = Info::default();
    let mut budget = Budget {
        left: opts.limit,
        cap: 128,
    };
    let mut chunks = Chunks {
        src,
        kind: *b"IDAT",
        left: 0,
        crc: 0,
        ended: false,
        pending: None,
        delivered: 0,
        bad_crc_at: None,
        seq: None,
        deferred: None,
    };
    let mut have_ihdr = false;
    let mut have_iccp = false;
    let mut have_phys = false;
    // 最初の IDAT より前の fcTL（既定の画像がアニメの最初のフレーム）
    let mut fctl_before_idat = false;
    loop {
        let Some((kind, len)) = chunks.header()? else {
            return Err(err("画像のデータがありません"));
        };
        if !have_ihdr && &kind != b"IHDR" {
            return Err(err("IHDR が最初にありません"));
        }
        match &kind {
            b"IDAT" => {
                chunks.begin(kind, len)?;
                break;
            }
            b"IEND" => return Err(err("画像のデータがありません")),
            b"fdAT" => return Err(err("IDAT より前に fdAT があります")),
            b"IHDR" | b"PLTE" => {
                let ok = if &kind == b"IHDR" {
                    len == 13
                } else {
                    (3..=768).contains(&len)
                };
                if !ok {
                    return Err(err(format!(
                        "{} の長さが正しくありません",
                        String::from_utf8_lossy(&kind)
                    )));
                }
                budget.grow(u64::from(len))?;
                let data = read_chunk(&mut chunks, kind, len)?;
                if !data.1 {
                    return Err(err("必須チャンクの CRC が合いません"));
                }
                let d = data.0;
                if &kind == b"IHDR" {
                    if have_ihdr {
                        return Err(err("IHDR が 2 つあります"));
                    }
                    parse_ihdr(&mut info, &d)?;
                    have_ihdr = true;
                } else {
                    if info.palette.is_some() {
                        return Err(err("PLTE が 2 つあります"));
                    }
                    info.palette = Some(d);
                }
            }
            _ if critical(&kind) => {
                return Err(err(format!(
                    "知らない必須チャンク {} があります",
                    String::from_utf8_lossy(&kind)
                )));
            }
            _ => {
                let skip_meta =
                    opts.header_only && matches!(&kind, b"iCCP" | b"tEXt" | b"zTXt" | b"iTXt");
                let range = ancillary_range(&kind).filter(|_| !skip_meta);
                let process = range.is_some_and(|(lo, hi)| (lo..=hi).contains(&len));
                if &kind == b"fcTL" && range.is_some() && !process {
                    return Err(err("fcTL の長さが正しくありません"));
                }
                if !process {
                    // 知らない補助チャンク・範囲の外・読まないもの：中身を読み飛ばす（CRC は見るが、捨てるだけ）
                    chunks.crc = zlib::crc32(&kind);
                    chunks.skip_data(u64::from(len))?;
                    if chunks.read_crc()?.is_none() {
                        return Err(err("チャンクが途中で切れています"));
                    }
                    continue;
                }
                budget.grow(u64::from(len))?;
                let (d, crc_ok) = read_chunk(&mut chunks, kind, len)?;
                if !crc_ok {
                    continue;
                }
                match &kind {
                    b"tRNS" => parse_trns(&mut info, &d),
                    b"pHYs" if !have_phys => {
                        if d[8] <= 1 {
                            info.pixel_dims = Some((be32(&d[0..4]), be32(&d[4..8]), d[8]));
                            have_phys = true;
                        }
                    }
                    b"acTL" if info.animation_control.is_none() => {
                        let (n, plays) = (be32(&d[0..4]), be32(&d[4..8]));
                        if n != 0 && n <= 0x7fff_ffff && plays <= 0x7fff_ffff {
                            info.animation_control = Some((n, plays));
                        }
                    }
                    b"fcTL" => {
                        let fc = parse_fctl(&info, &d)?;
                        chunks.check_seq(fc.sequence_number)?;
                        if (fc.x_offset, fc.y_offset, fc.width, fc.height)
                            != (0, 0, info.width, info.height)
                        {
                            return Err(err(
                                "既定の画像の fcTL がキャンバスいっぱいではありません",
                            ));
                        }
                        info.frame_control = Some(fc);
                        fctl_before_idat = true;
                    }
                    b"iCCP" if !have_iccp => {
                        have_iccp = true;
                        info.icc_profile = parse_iccp(&d, &mut budget);
                    }
                    b"tEXt" | b"zTXt" | b"iTXt" => {
                        budget.reserve(u64::from(len))?;
                        parse_text(&mut info, &kind, &d);
                    }
                    _ => {}
                }
            }
        }
    }
    let color = info.color();
    // png クレートと同じく、1 行のバイト数 × 高さが isize::MAX を超える画像は断る
    let row = row_bytes(info.width, color.samples(), info.bit_depth)
        .filter(|r| {
            r.checked_mul(info.height as usize)
                .is_some_and(|t| t <= isize::MAX as usize)
        })
        .ok_or_else(|| err("画像が大きすぎます"))?;
    // png クレートと同じく、最初の IDAT で既定の画像の 1 行分を上限から引く（1 行が上限を超える画像は断る。行の作業領域を
    // 上限で抑える）。APNG のフレームごとの積み上げはまねない（利用者の決定）
    budget.reserve(row as u64)?;
    let frames = match info.animation_control {
        Some((n, _)) => n.saturating_add(u32::from(!fctl_before_idat)),
        None => 1,
    };
    let z = Inflate::new(
        chunks,
        zlib::Options {
            zlib: true,
            check_adler: false,
            limit: u64::MAX,
            flavor: Flavor::Fdeflate,
        },
    );
    let rows = Rows::new(info.width, info.height, info.interlaced);
    // 行の置き場（1 行分は上の上限で抑えてある。外から来た大きさなので確かめて確保する）
    let mut cur = Vec::new();
    cur.try_reserve_exact(row + 1)
        .map_err(|_| err("画像が大きすぎます"))?;
    let mut prev = Vec::new();
    prev.try_reserve_exact(row + 1)
        .map_err(|_| err("画像が大きすぎます"))?;
    let mut z = z;
    z.set_size_hint(frame_bytes(
        info.width,
        info.height,
        info.interlaced,
        color,
        info.bit_depth,
    ));
    Ok(Reader {
        info,
        z,
        rows,
        prev,
        cur,
        first_line: true,
        frames_left: frames - 1,
    })
}

/// 長さ `len` の中身と CRC を読む（中身・CRC が合ったか）。
fn read_chunk<R: Read>(
    chunks: &mut Chunks<R>,
    kind: [u8; 4],
    len: u32,
) -> Result<(Vec<u8>, bool), EngineError> {
    let mut d = Vec::new();
    d.try_reserve_exact(len as usize)
        .map_err(|_| err("チャンクが大きすぎます"))?;
    d.resize(len as usize, 0);
    if !read_full(&mut chunks.src, &mut d)? {
        return Err(err("チャンクが途中で切れています"));
    }
    chunks.crc = zlib::crc32_update(zlib::crc32(&kind), &d);
    match chunks.read_crc()? {
        Some(ok) => Ok((d, ok)),
        None => Err(err("チャンクが途中で切れています")),
    }
}

fn parse_ihdr(info: &mut Info, d: &[u8]) -> Result<(), EngineError> {
    let (w, h) = (be32(&d[0..4]), be32(&d[4..8]));
    let (depth, color, comp, filter, interlace) = (d[8], d[9], d[10], d[11], d[12]);
    if w == 0 || h == 0 {
        return Err(err("画像の大きさが 0 です"));
    }
    if !matches!(depth, 1 | 2 | 4 | 8 | 16) {
        return Err(err(format!("ビット深度 {depth} は読めません")));
    }
    let Some(c) = ColorType::from_u8(color) else {
        return Err(err(format!("色の種類 {color} は読めません")));
    };
    let bad = match depth {
        1 | 2 | 4 => matches!(
            c,
            ColorType::Rgb | ColorType::GrayscaleAlpha | ColorType::Rgba
        ),
        16 => c == ColorType::Indexed,
        _ => false,
    };
    if bad {
        return Err(err("ビット深度と色の種類の組み合わせが正しくありません"));
    }
    if comp != 0 || filter != 0 || interlace > 1 {
        return Err(err(
            "圧縮・フィルタ・インターレースの方式が正しくありません",
        ));
    }
    info.width = w;
    info.height = h;
    info.bit_depth = depth;
    info.color_type = Some(c);
    info.interlaced = interlace == 1;
    Ok(())
}

/// tRNS（png クレートと同じ持ち方：グレーは 16 ビット未満なら [1]、RGB は [1, 3, 5]、16 ビットは全体、パレットは PLTE のあと
/// だけ全体。グレー＋アルファ・RGBA・短いもの・2 つ目は捨てる）。
fn parse_trns(info: &mut Info, d: &[u8]) {
    if info.trns.is_some() {
        return;
    }
    let sixteen = info.bit_depth == 16;
    info.trns = match info.color() {
        ColorType::Grayscale if d.len() >= 2 => Some(if sixteen { d.to_vec() } else { vec![d[1]] }),
        ColorType::Rgb if d.len() >= 6 => Some(if sixteen {
            d.to_vec()
        } else {
            vec![d[1], d[3], d[5]]
        }),
        ColorType::Indexed if info.palette.is_some() => Some(d.to_vec()),
        _ => None,
    };
}

/// fcTL を読んで確かめる（どの誤りも壊れたファイル）。
fn parse_fctl(info: &Info, d: &[u8]) -> Result<FrameControl, EngineError> {
    let fc = FrameControl {
        sequence_number: be32(&d[0..4]),
        width: be32(&d[4..8]),
        height: be32(&d[8..12]),
        x_offset: be32(&d[12..16]),
        y_offset: be32(&d[16..20]),
        delay_num: u16::from_be_bytes([d[20], d[21]]),
        delay_den: u16::from_be_bytes([d[22], d[23]]),
        dispose_op: d[24],
        blend_op: d[25],
    };
    let inside = fc.width > 0
        && fc.height > 0
        && fc
            .x_offset
            .checked_add(fc.width)
            .is_some_and(|r| r <= info.width)
        && fc
            .y_offset
            .checked_add(fc.height)
            .is_some_and(|b| b <= info.height);
    if !inside || fc.dispose_op > 2 || fc.blend_op > 1 {
        return Err(err("fcTL の値が正しくありません"));
    }
    Ok(fc)
}

/// iCCP（名前 1〜80 バイトと NUL・方式 0・残りを上限で展開。どの誤りも ICC なし。受け付けたら上限から長さを引く）。
fn parse_iccp(d: &[u8], budget: &mut Budget) -> Option<Vec<u8>> {
    let nul = d.iter().take(81).position(|&b| b == 0)?;
    if nul == 0 {
        return None;
    }
    if *d.get(nul + 1)? != 0 {
        return None;
    }
    let profile = inflate_bounded(&d[nul + 2..], budget.left)?;
    budget.reserve(profile.len() as u64).ok()?;
    Some(profile)
}

/// zlib を上限 `limit` までに展開する（fdeflate の `decompress_to_vec_bounded` と同じ：頭と Adler-32 を照らし、尾まで要る。
/// あとのデータは無視。超えれば None）。
fn inflate_bounded(src: &[u8], limit: u64) -> Option<Vec<u8>> {
    let mut z = Inflate::new(
        zlib::SliceInput::new(src),
        zlib::Options {
            zlib: true,
            check_adler: true,
            limit,
            flavor: Flavor::Fdeflate,
        },
    );
    let mut out = Vec::new();
    while !z.finished() {
        let n = z.fill(64 * 1024).ok()?;
        out.try_reserve(n).ok()?;
        out.extend_from_slice(&z.ready()[..n]);
        z.consume(n);
    }
    Some(out)
}

/// Latin-1 のバイト列を文字列に（各バイトを U+00XX に）。
fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

/// テキスト（tEXt・iTXt を png クレートと同じ規則で持つ。zTXt は持たない。壊れたものは捨てる）。
fn parse_text(info: &mut Info, kind: &[u8; 4], d: &[u8]) {
    let Some(nul) = d.iter().position(|&b| b == 0) else {
        return;
    };
    if nul == 0 || nul > 79 {
        return;
    }
    let (key, rest) = (&d[..nul], &d[nul + 1..]);
    match kind {
        b"tEXt" => info
            .uncompressed_latin1_text
            .push((latin1(key), latin1(rest))),
        b"iTXt" => {
            let (Some(&flag), Some(&method)) = (rest.first(), rest.get(1)) else {
                return;
            };
            let Some(l) = rest[2..].iter().position(|&b| b == 0) else {
                return;
            };
            let lang = &rest[2..2 + l];
            let after = &rest[2 + l + 1..];
            let Some(t) = after.iter().position(|&b| b == 0) else {
                return;
            };
            let (translated, text) = (&after[..t], &after[t + 1..]);
            let compressed = match flag {
                0 => false,
                1 => true,
                _ => return,
            };
            if compressed && method != 0 {
                return;
            }
            if !lang.is_ascii() || std::str::from_utf8(translated).is_err() {
                return;
            }
            if compressed {
                // 圧縮した本文は kura は使わない（展開爆弾を避けて読まない）
                return;
            }
            if let Ok(text) = std::str::from_utf8(text) {
                info.utf8_text.push((latin1(key), text.to_string()));
            }
        }
        _ => {}
    }
}

impl<R: Read> Reader<R> {
    /// 最初の IDAT より前に読んだもの。
    pub(crate) fn info(&self) -> &Info {
        &self.info
    }

    /// 今のフレームの次の行（インターレースなら段ごとの生の行。フィルタを戻し、フィルタのバイトを除いたもの）。行がもうなければ
    /// None。
    pub(crate) fn next_row(&mut self) -> Result<Option<&[u8]>, EngineError> {
        if self.rows.done {
            return Ok(None);
        }
        let color = self.info.color();
        let (pw, _) = self.rows.dims();
        let len = row_bytes(pw, color.samples(), self.info.bit_depth)
            .ok_or_else(|| err("画像が大きすぎます"))?;
        let stride = len + 1;
        let avail = self.z.fill(stride).map_err(zerr)?;
        if avail < stride {
            return Err(err("画像のデータが足りません"));
        }
        self.cur.clear();
        self.cur.extend_from_slice(&self.z.ready()[..stride]);
        self.z.consume(stride);
        if self.first_line {
            self.prev.clear();
            self.prev.resize(stride, 0);
        }
        let bpp = (color.samples() * usize::from(self.info.bit_depth)).div_ceil(8);
        unfilter(&mut self.cur, &self.prev, bpp)?;
        std::mem::swap(&mut self.cur, &mut self.prev);
        self.first_line = self.rows.advance();
        if self.rows.done {
            // 要る行が揃った：その先のデータを使ったなら、png クレートはそこまでの IDAT の CRC を照らしていた
            let used = self.z.consumed_bits();
            if self
                .z
                .input_mut()
                .bad_crc_at
                .is_some_and(|e| used > e.saturating_mul(8))
            {
                return Err(err("IDAT の CRC が合いません"));
            }
        }
        Ok(Some(&self.prev[1..]))
    }

    /// 次のフレームへ進み、その fcTL を返す（png クレートの `next_frame_info` と同じ：残りのデータは展開せずに読み通し、次の fdAT
    /// の頭までのチャンクを読む）。
    pub(crate) fn next_frame_info(&mut self) -> Result<FrameControl, EngineError> {
        if self.frames_left == 0 {
            return Err(err("フレームがもうありません"));
        }
        self.frames_left -= 1;
        let info = &mut self.info;
        let chunks = self.z.input_mut();
        chunks.skip_run()?;
        let mut fctl_seen = false;
        loop {
            let Some((kind, len)) = chunks.header()? else {
                return Err(err("画像のデータが途中で切れています"));
            };
            match &kind {
                b"fdAT" => {
                    if !fctl_seen {
                        return Err(err("fcTL のない fdAT があります"));
                    }
                    chunks.delivered = 0;
                    chunks.bad_crc_at = None;
                    chunks.begin(kind, len)?;
                    break;
                }
                b"IDAT" => return Err(err("IDAT がフレームの間にあります")),
                b"IEND" => return Err(err("フレームが足りません")),
                b"PLTE" => {
                    // png クレートと同じく、パレットがあれば 2 つ目として断り、なければ読むだけ（使わない。利用者の決定）
                    if info.palette.is_some() {
                        return Err(err("PLTE が 2 つあります"));
                    }
                    if !(3..=768).contains(&len) {
                        return Err(err("PLTE の長さが正しくありません"));
                    }
                    let (_, ok) = read_chunk(chunks, kind, len)?;
                    if !ok {
                        return Err(err("必須チャンクの CRC が合いません"));
                    }
                }
                b"fcTL" => {
                    if len != 26 {
                        return Err(err("fcTL の長さが正しくありません"));
                    }
                    let (d, ok) = read_chunk(chunks, kind, len)?;
                    if !ok {
                        continue;
                    }
                    let fc = parse_fctl(info, &d)?;
                    chunks.check_seq(fc.sequence_number)?;
                    info.frame_control = Some(fc);
                    fctl_seen = true;
                }
                _ if critical(&kind) => {
                    return Err(err(format!(
                        "知らない必須チャンク {} があります",
                        String::from_utf8_lossy(&kind)
                    )));
                }
                _ => {
                    // フレームの間の補助チャンクは使わない（tRNS・PLTE も。利用者の決定）
                    chunks.crc = zlib::crc32(&kind);
                    chunks.skip_data(u64::from(len))?;
                    if chunks.read_crc()?.is_none() {
                        return Err(err("チャンクが途中で切れています"));
                    }
                }
            }
        }
        let fc = info.frame_control.ok_or_else(|| err("fcTL がありません"))?;
        self.z.restart();
        let color = self.info.color();
        self.z.set_size_hint(frame_bytes(
            fc.width,
            fc.height,
            self.info.interlaced,
            color,
            self.info.bit_depth,
        ));
        self.rows = Rows::new(fc.width, fc.height, self.info.interlaced);
        self.first_line = true;
        Ok(fc)
    }
}

/// フィルタを戻す（`cur` は頭にフィルタの型のバイトを持つ 1 行、`prev` は前の行（同じ長さ。段の最初は 0））。値は PNG の仕様の
/// バイトごとの式どおり（1 画素のバイト数 `bpp` ごとに分けて、画素をまとめて計算するだけ）。
fn unfilter(cur: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), EngineError> {
    let kind = cur[0];
    let (row, up) = (&mut cur[1..], &prev[1..]);
    match kind {
        0 => {}
        2 => {
            for (x, &b) in row.iter_mut().zip(up) {
                *x = x.wrapping_add(b);
            }
        }
        1 | 3 | 4 => match bpp {
            1 => unfilter_px::<1>(kind, row, up),
            2 => unfilter_px::<2>(kind, row, up),
            3 => unfilter_px::<3>(kind, row, up),
            4 => unfilter_px::<4>(kind, row, up),
            6 => unfilter_px::<6>(kind, row, up),
            8 => unfilter_px::<8>(kind, row, up),
            _ => return Err(err("1 画素のバイト数が正しくありません")),
        },
        _ => return Err(err(format!("フィルタの型 {kind} は読めません"))),
    }
    Ok(())
}

/// Sub・Average・Paeth を 1 画素（`B` バイト）ずつ戻す（行の末尾の画素に満たない分は 1 バイトずつ同じ式で）。
fn unfilter_px<const B: usize>(kind: u8, row: &mut [u8], up: &[u8]) {
    let n = row.len().min(up.len());
    let (row, up) = (&mut row[..n], &up[..n]);
    // 左の画素（a）と左上の画素（c）。最初の画素は 0
    let mut a = [0u8; B];
    let mut c = [0u8; B];
    let full = n / B * B;
    let (body, tail) = row.split_at_mut(full);
    let (ubody, utail) = up.split_at(full);
    let (px, _) = body.as_chunks_mut::<B>();
    let (upx, _) = ubody.as_chunks::<B>();
    match kind {
        1 => {
            for x in px.iter_mut() {
                let mut v = [0u8; B];
                for k in 0..B {
                    v[k] = x[k].wrapping_add(a[k]);
                }
                *x = v;
                a = v;
            }
        }
        3 => {
            for (x, b) in px.iter_mut().zip(upx) {
                let mut v = [0u8; B];
                for k in 0..B {
                    v[k] = x[k].wrapping_add(((u16::from(a[k]) + u16::from(b[k])) / 2) as u8);
                }
                *x = v;
                a = v;
            }
        }
        _ => {
            for (x, b) in px.iter_mut().zip(upx) {
                // 画素の値を新しい配列に組んでから書く（1 画素の中のバイトをまとめて計算させる）
                let mut v = [0u8; B];
                for k in 0..B {
                    v[k] = x[k].wrapping_add(paeth(a[k], b[k], c[k]));
                }
                *x = v;
                a = v;
                c = *b;
            }
        }
    }
    // 1 画素に満たない末尾（1/2/4 ビットの行では起きない。B は 1）
    for (k, (x, &b)) in tail.iter_mut().zip(utail).enumerate() {
        let (ak, ck) = (a[k], c[k]);
        *x = match kind {
            1 => x.wrapping_add(ak),
            3 => x.wrapping_add(((u16::from(ak) + u16::from(b)) / 2) as u8),
            _ => x.wrapping_add(paeth(ak, b, ck)),
        };
    }
}

/// Paeth の予測（PNG の仕様どおり：pa・pb・pc の最小、同じなら a・b・c の順）。前から最小を更新していく形（png クレートが aarch64 で
/// 使う形。仕様の if の並びと同じ値で、分岐のないコードになりやすい）。
#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
    let pa = (ib - ic).abs();
    let pb = (ia - ic).abs();
    let pc = ((ia - ic) + (ib - ic)).abs();
    let mut out = a;
    let mut min = pa;
    if pb < min {
        min = pb;
        out = b;
    }
    if pc < min {
        out = c;
    }
    out
}

#[cfg(test)]
mod tests;
