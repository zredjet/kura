//! GIF の読み口（M16 段 8。ADR-225）。前の gif クレート 0.14.2（kura の使い方。`codec::gif_old`）が読めた GIF は同じ値で
//! 読む（`gif-rs-dec/1` のまま）。途中で切れた GIF はブラウザのように読む（利用者の決定）。
//!
//! ブロックの層（[`scan`]）は LZW を展開せずに前から読み、フレームの記述と LZW のデータの位置を集める：
//! - 署名 `GIF87a`・`GIF89a`、論理画面、大域パレット（色の数は印のとおり。空なら大域パレットなし）。
//! - 拡張（0x21）：ラベルは 0x01・0xF9・0xFE・0xFF だけ（ほかは誤り）。GCE は最後のサブブロックがちょうど 4 バイト
//!   （ほかは誤り。最初が長さ 0 なら中身は空で誤り）で、次の画像の dispose（4〜7 は Any）・遅延・透明の番号を決める（後の
//!   GCE が前を上書きする）。アプリケーションの拡張は、最初の画像より前のものだけを読む：名前のサブブロックがちょうど
//!   `NETSCAPE2.0` なら、次のサブブロックがちょうど `[1, 下位, 上位]` のとき繰り返し（0 は無限）、`ICCRGBG1012` なら残りの
//!   サブブロックを継いだものが ICC（後の拡張が前を置き換える）。ほかの拡張とサブブロックは読み飛ばす。
//! - 画像（0x2C）：記述子・局所パレット・LZW の最小の符号の長さ（1〜11。ほかは誤り）。局所パレットも大域パレットもない
//!   フレームは誤り。データの最初のサブブロックの長さが 0 なら、前の gif クレートと同じく次のバイトから長さとして読み直す
//!   （そのおかげで読めていたファイルを同じ値で読むため）。
//! - 終わり（0x3B）で止まり、そのあとは読まない。ほかのブロックの種類は誤り。
//! - 途中で切れたとき：ファイルの終わりで止まる。記述子・局所パレット・最小の符号の長さまで読めたフレームは、届いた画素まで
//!   描く（前は誤り）。フレームが 1 つもなければ誤り。ICC は、最後に残ったものが上限を超えていれば誤り。
//!
//! 画素（[`decode_frame`]）は、記録した位置からサブブロックを継いで LZW（`lzw::Decoder`）で 1 行ずつ展開する（フレーム
//! 全体の置き場は持たない）。幅 × 高さに満ちたら残りは読まない。ファイルが途中で切れたら届いた画素まで（途中で切れた
//! サブブロックの届いた分も）。データが終わりまであるのに画素が足りなければ、前と同じく誤り。表にない符号は誤り。

use std::io::{BufReader, Read, SeekFrom};

use super::lzw;
use crate::codec::ReadSeek;
use crate::error::EngineError;
use crate::limits::Limits;

/// 読み取りの識別子（`codec::gif` と同じ）。
const DEC_ID: &str = "gif-rs-dec/1";

pub(crate) fn err(message: impl Into<String>) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: message.into(),
    }
}

/// dispose。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dispose {
    Any,
    Keep,
    Background,
    Previous,
}

/// フレームの記述（LZW を展開せずに読んだもの）。
#[derive(Debug, Clone)]
pub(crate) struct FrameInfo {
    pub left: u32,
    pub top: u32,
    pub w: u32,
    pub h: u32,
    pub interlaced: bool,
    /// 局所パレット（印のとおりの色の数）。
    pub local: Option<Vec<[u8; 4]>>,
    pub transparent: Option<u8>,
    pub dispose: Dispose,
    /// 1/100 秒。
    pub delay: u16,
    pub min_size: u8,
    /// LZW のサブブロックの始まりの位置。
    pub data_at: u64,
    /// データのサブブロックが終わり（長さ 0）まで届いたか（偽ならファイルが途中で切れた）。
    pub complete: bool,
}

/// 繰り返し（NETSCAPE2.0）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Repeat {
    /// 拡張がない（1 回）。
    None,
    Infinite,
    Finite(u16),
}

/// ブロックの層を読んだもの。
#[derive(Debug, Clone)]
pub(crate) struct Scan {
    pub w: u32,
    pub h: u32,
    /// 大域パレット（印のとおりの色の数。空ならなし）。
    pub global: Option<Vec<[u8; 4]>>,
    pub frames: Vec<FrameInfo>,
    pub repeat: Repeat,
    pub icc: Option<Vec<u8>>,
}

/// 読んだバイトの数を数える読み口（ファイルの終わりは `None`）。
struct Src<'a> {
    r: BufReader<&'a mut dyn ReadSeek>,
    pos: u64,
}

impl Src<'_> {
    fn byte(&mut self) -> Result<Option<u8>, EngineError> {
        let mut b = [0u8; 1];
        loop {
            match self.r.read(&mut b) {
                Ok(0) => return Ok(None),
                Ok(_) => {
                    self.pos += 1;
                    return Ok(Some(b[0]));
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// `buf` をできるだけ満たし、読めたバイトの数を返す（終わりに着けば短い）。
    fn fill_some(&mut self, buf: &mut [u8]) -> Result<usize, EngineError> {
        let mut at = 0;
        while at < buf.len() {
            match self.r.read(&mut buf[at..]) {
                Ok(0) => break,
                Ok(n) => {
                    at += n;
                    self.pos += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(at)
    }

    /// `buf` を満たす。終わりに着けば偽（読めた分は捨てる）。
    fn fill(&mut self, buf: &mut [u8]) -> Result<bool, EngineError> {
        Ok(self.fill_some(buf)? == buf.len())
    }

    /// 色の表（`n` 色）。終わりに着けば None。
    fn table(&mut self, n: usize) -> Result<Option<Vec<[u8; 4]>>, EngineError> {
        let mut raw = vec![0u8; n * 3];
        if !self.fill(&mut raw)? {
            return Ok(None);
        }
        Ok(Some(
            raw.as_chunks::<3>()
                .0
                .iter()
                .map(|c| [c[0], c[1], c[2], 255])
                .collect(),
        ))
    }
}

/// サブブロックの列を読む。`f` にサブブロックごとの中身を渡す。終わり（長さ 0）まで読めれば真、ファイルの終わりなら偽
/// （途中で切れたサブブロックは、届いた分を渡してから偽）。
fn sub_blocks(
    src: &mut Src<'_>,
    mut f: impl FnMut(&[u8]) -> Result<(), EngineError>,
) -> Result<bool, EngineError> {
    loop {
        let Some(n) = src.byte()? else {
            return Ok(false);
        };
        if n == 0 {
            return Ok(true);
        }
        if !one_block(src, usize::from(n), &mut f)? {
            return Ok(false);
        }
    }
}

/// 長さ `n` のサブブロックを読んで `f` に渡す。途中で切れたら届いた分を渡して偽。
fn one_block(
    src: &mut Src<'_>,
    n: usize,
    f: &mut impl FnMut(&[u8]) -> Result<(), EngineError>,
) -> Result<bool, EngineError> {
    let mut buf = [0u8; 255];
    let got = src.fill_some(&mut buf[..n])?;
    if got > 0 {
        f(&buf[..got])?;
    }
    Ok(got == n)
}

/// 画像のデータのサブブロックを読む（[`sub_blocks`] と同じ。ただし前の gif クレートと同じく、最初の長さが 0 なら終わりと
/// せず、その次のバイトから長さとして読み直す。`m 00 00` がデータなし、`m 00 3B` はファイルの終わりになる）。
fn image_blocks(
    src: &mut Src<'_>,
    mut f: impl FnMut(&[u8]) -> Result<(), EngineError>,
) -> Result<bool, EngineError> {
    let Some(n) = src.byte()? else {
        return Ok(false);
    };
    if n > 0 && !one_block(src, usize::from(n), &mut f)? {
        return Ok(false);
    }
    sub_blocks(src, f)
}

/// GCE の中身（次の画像に当てる）。
#[derive(Debug, Clone, Copy)]
struct Gce {
    transparent: Option<u8>,
    dispose: Dispose,
    delay: u16,
}

/// アプリケーションの拡張の読み方。
enum App {
    Name,
    Netscape,
    Icc,
    Skip,
}

/// ブロックの層を読む（LZW は展開しない）。
pub(crate) fn scan(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Scan, EngineError> {
    src.rewind()?;
    let mut s = Src {
        r: BufReader::new(src),
        pos: 0,
    };
    let mut sig = [0u8; 6];
    if !s.fill(&mut sig)? {
        return Err(err("GIF の頭が足りません"));
    }
    if &sig != b"GIF87a" && &sig != b"GIF89a" {
        return Err(err("GIF の署名がありません"));
    }
    let mut lsd = [0u8; 7];
    if !s.fill(&mut lsd)? {
        return Err(err("論理画面の記述が足りません"));
    }
    let w = u32::from(u16::from_le_bytes([lsd[0], lsd[1]]));
    let h = u32::from(u16::from_le_bytes([lsd[2], lsd[3]]));
    let global = if lsd[4] & 0x80 != 0 {
        let n = 1usize << ((lsd[4] & 7) + 1);
        match s.table(n)? {
            Some(t) => Some(t),
            None => return Err(err("画像のデータがありません")),
        }
    } else {
        None
    };
    let mut frames: Vec<FrameInfo> = Vec::new();
    let mut gce: Option<Gce> = None;
    let mut repeat = Repeat::None;
    let mut icc: Option<Vec<u8>> = None;
    // 残った ICC が上限を超えていたか（前の gif クレートと kura と同じく、置き換えられたものの大きさは問わない）
    let mut icc_over = false;
    let mut header_end = false;
    while let Some(b) = s.byte()? {
        if b != 0x21 {
            header_end = true;
        }
        match b {
            0x21 => {
                let Some(label) = s.byte()? else {
                    break;
                };
                match label {
                    0xF9 => {
                        let mut last: Vec<u8> = Vec::new();
                        if !sub_blocks(&mut s, |d| {
                            last.clear();
                            last.extend_from_slice(d);
                            Ok(())
                        })? {
                            break;
                        }
                        if last.len() != 4 {
                            return Err(err("GCE の長さが違います"));
                        }
                        let flags = last[0];
                        gce = Some(Gce {
                            transparent: (flags & 1 != 0).then_some(last[3]),
                            dispose: match (flags >> 2) & 7 {
                                1 => Dispose::Keep,
                                2 => Dispose::Background,
                                3 => Dispose::Previous,
                                _ => Dispose::Any,
                            },
                            delay: u16::from_le_bytes([last[1], last[2]]),
                        });
                    }
                    0xFF if !header_end => {
                        let mut state = App::Name;
                        let mut new_repeat = None;
                        let mut new_icc: Option<Vec<u8>> = None;
                        let mut too_big = false;
                        let complete = sub_blocks(&mut s, |d| {
                            match state {
                                App::Name => {
                                    state = match d {
                                        b"NETSCAPE2.0" => App::Netscape,
                                        b"ICCRGBG1012" => {
                                            new_icc = Some(Vec::new());
                                            App::Icc
                                        }
                                        _ => App::Skip,
                                    };
                                }
                                App::Netscape => {
                                    if let [1, lo, hi] = d {
                                        let n = u16::from_le_bytes([*lo, *hi]);
                                        new_repeat = Some(if n == 0 {
                                            Repeat::Infinite
                                        } else {
                                            Repeat::Finite(n)
                                        });
                                    }
                                    state = App::Skip;
                                }
                                App::Icc => {
                                    if let Some(v) = new_icc.as_mut() {
                                        if (v.len() + d.len()) as u64 > limits.max_meta_bytes {
                                            too_big = true;
                                        } else {
                                            v.extend_from_slice(d);
                                        }
                                    }
                                }
                                App::Skip => {}
                            }
                            Ok(())
                        })?;
                        if !complete {
                            break;
                        }
                        if let Some(r) = new_repeat {
                            repeat = r;
                        }
                        if new_icc.is_some() {
                            icc = new_icc;
                            icc_over = too_big;
                        }
                    }
                    0x01 | 0xFE | 0xFF => {
                        if !sub_blocks(&mut s, |_| Ok(()))? {
                            break;
                        }
                    }
                    _ => return Err(err("知らない拡張のブロックがあります")),
                }
            }
            0x2C => {
                let mut d = [0u8; 9];
                if !s.fill(&mut d)? {
                    break;
                }
                let flags = d[8];
                let local = if flags & 0x80 != 0 {
                    match s.table(1usize << ((flags & 7) + 1))? {
                        Some(t) => Some(t),
                        None => break,
                    }
                } else {
                    None
                };
                let Some(min_size) = s.byte()? else {
                    break;
                };
                if !(1..=11).contains(&min_size) {
                    return Err(err("LZW の最小の符号の長さが範囲の外です"));
                }
                if local.is_none() && global.is_none() {
                    return Err(err("フレームに色の表がありません"));
                }
                if frames.len() as u64 >= limits.max_frames {
                    return Err(EngineError::Limit(format!(
                        "フレームが {} を超えます",
                        limits.max_frames
                    )));
                }
                let g = gce.take();
                frames.push(FrameInfo {
                    left: u32::from(u16::from_le_bytes([d[0], d[1]])),
                    top: u32::from(u16::from_le_bytes([d[2], d[3]])),
                    w: u32::from(u16::from_le_bytes([d[4], d[5]])),
                    h: u32::from(u16::from_le_bytes([d[6], d[7]])),
                    interlaced: flags & 0x40 != 0,
                    local,
                    transparent: g.and_then(|g| g.transparent),
                    // GCE のない画像は Keep（前の gif クレートの既定。合成では Any と同じ）
                    dispose: g.map_or(Dispose::Keep, |g| g.dispose),
                    delay: g.map_or(0, |g| g.delay),
                    min_size,
                    data_at: s.pos,
                    complete: false,
                });
                let complete = image_blocks(&mut s, |_| Ok(()))?;
                if let Some(f) = frames.last_mut() {
                    f.complete = complete;
                }
                if !complete {
                    break;
                }
            }
            0x3B => break,
            _ => return Err(err("知らないブロックがあります")),
        }
    }
    if frames.is_empty() {
        return Err(err("画像のデータがありません"));
    }
    if icc_over {
        return Err(EngineError::Limit("ICC プロファイルが大きすぎます".into()));
    }
    Ok(Scan {
        w,
        h,
        global: global.filter(|g| !g.is_empty()),
        frames,
        repeat,
        icc,
    })
}

/// 展開した行を受け取る関数（画像の行と、左からの番号）。
type RowFn<'a> = dyn FnMut(usize, &[u8]) -> Result<(), EngineError> + 'a;

/// フレーム `f` の番号を 1 行ずつ展開し、`row(y, 番号)` に渡す（y は画像の行。インターレースは流れの順に並べ替えた行。
/// 番号は左から。フレームの置き場は持たない）。ファイルが途中で切れたフレームは届いた画素まで（最後の行は短い）。データの
/// サブブロックが終わりまであるのに画素が足りなければ誤り（前と同じ）。表にない符号も誤り。
pub(crate) fn decode_frame(
    src: &mut dyn ReadSeek,
    f: &FrameInfo,
    mut row: impl FnMut(usize, &[u8]) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let (w, h) = (f.w as usize, f.h as usize);
    if w == 0 || h == 0 {
        return Ok(());
    }
    // 流れの k 番目の行 → 画像の行
    let order: Vec<u32> = if f.interlaced {
        [(0u32, 8u32), (4, 8), (2, 4), (1, 2)]
            .iter()
            .flat_map(|&(start, step)| (start..f.h).step_by(step as usize))
            .collect()
    } else {
        (0..f.h).collect()
    };
    src.seek(SeekFrom::Start(f.data_at))?;
    let mut s = Src {
        r: BufReader::new(src),
        pos: f.data_at,
    };
    let mut dec = lzw::Decoder::new(u32::from(f.min_size));
    let mut buf = vec![0u8; w];
    let mut filled = 0usize;
    let mut k = 0usize;
    let feed = |d: &[u8],
                dec: &mut lzw::Decoder,
                buf: &mut [u8],
                filled: &mut usize,
                k: &mut usize,
                row: &mut RowFn<'_>|
     -> Result<(), EngineError> {
        let mut d = d;
        while *k < h && !dec.ended() {
            let used = dec
                .feed(d, buf, filled)
                .map_err(|_| err("LZW の符号が表にありません"))?;
            d = &d[used..];
            if *filled == w {
                row(order[*k] as usize, buf)?;
                *k += 1;
                *filled = 0;
            } else if d.is_empty() {
                break;
            }
        }
        Ok(())
    };
    let complete = image_blocks(&mut s, |d| {
        feed(d, &mut dec, &mut buf, &mut filled, &mut k, &mut row)
    })?;
    // 書きかけの文字列の残り
    while k < h && dec.has_pending() {
        feed(&[], &mut dec, &mut buf, &mut filled, &mut k, &mut row)?;
        if filled < w {
            break;
        }
    }
    if k < h {
        if complete {
            return Err(err("フレームの画素のデータが足りません"));
        }
        if filled > 0 {
            row(order[k] as usize, &buf[..filled])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
