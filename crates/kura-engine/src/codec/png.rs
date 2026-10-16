//! PNG（`png` クレート。版は `=` で固定。Pinned）。
//!
//! 読み取り（`png-rs-dec/2`。`png-rs-dec/1` は旧版として残す）：
//! - 8/16 ビット、グレー・グレー＋アルファ・RGB・RGBA・パレット（1/2/4/8 ビット）。
//!   1/2/4 ビットのグレーは 8 ビットへ伸ばし（255/(2^n−1) 倍）、パレットは番号のまま Indexed8 にする。
//!   グレー・RGB の tRNS（透明にする 1 色）は、アルファ付きの形式にして、その色の画素のアルファを 0 にする。
//!   ICC・解像度（pHYs）・テキスト（tEXt と圧縮していない iTXt）を運ぶ。圧縮したテキストは展開爆弾を避けて読まない。
//! - eXIf（IDAT より前にあるものだけ。後ろのものは形を決める前に読めないので無視する）を運び、向きを画素に当てる。
//! - APNG：acTL と fcTL があれば、フレームが 1 つでもフレームの列にする（書いた APNG を読み直して同じ形に戻るように）。
//!   acTL だけで fcTL がなければ 1 枚の画像。acTL のフレームの数と fcTL の数が違えば壊れたファイル。フレームは IHDR の大きさのキャンバスに fcTL の blend（SOURCE・OVER）で重ね、
//!   描いた直後のキャンバスを 1 フレームにする（どれもキャンバスいっぱい）。そのあと dispose（NONE・BACKGROUND・PREVIOUS。
//!   最初のフレームの PREVIOUS は BACKGROUND）を当てる。キャンバスの初めと BACKGROUND は透明な黒（アルファのない形式では黒）。
//!   OVER は `imaging::composite::over` の式。表示時間は delay_num / delay_den 秒（den が 0 なら 100）を ms に四捨五入、
//!   繰り返しは num_plays（0 は無限）。
//!   パレットの画像は、番号のまま重ねられるとき（OVER で半透明の色を重ねない・消すときに透明な色がある）だけ Indexed8、
//!   ほかは Rgba8 にする。この判断と形は、チャンクの並び（`scan`）を先に読んで決める（形と中身を必ず一致させる）。
//! - `png-rs-dec/1`：eXIf を読まず、APNG は既定の画像だけを読む。
//!
//! 書き出し（`png-rs-enc/1`）：値の形式のまま（16 ビットはビッグエンディアンに戻す）。Indexed8 は PLTE と tRNS。
//! EXIF は eXIf に書く。フレームの列（キャンバスいっぱいのもの）は APNG（既定の画像が最初のフレーム、dispose NONE・blend SOURCE）。
//! 時刻（tIME）は書かない。

use std::borrow::Cow;
use std::io::{BufReader, Read, Write};
use std::sync::Arc;

use serde_json::json;

use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, caps_of, exif, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::imaging::{composite, convert, orient};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Dim, Fullness, Shape};
use crate::value::{Frames, Image, ImageMeta, Palette, PixelFormat, Raster, Value};

/// PNG の読み書き（読み取りは今の版）。
pub struct PngCodec;

/// PNG の読み取りの旧版（`png-rs-dec/1`）。作り直しで記録の識別子から選ぶときだけ使う。
pub struct PngDecoderV1;

/// PNG の読み取りの旧版の一覧（`CodecSet` が旧版として登録する）。
pub fn legacy_decoders() -> Vec<Arc<dyn Decoder>> {
    vec![Arc::new(PngDecoderV1)]
}

/// 読み取りの約束の版（読む値が変わる変更は上げ、旧版を残す。§9.1）。
const DEC_ID: &str = "png-rs-dec/2";
const DEC_ID_V1: &str = "png-rs-dec/1";
/// 書き出しの約束の版（書くバイト列が変わる変更は上げる。§9.1）。
const ENC_ID: &str = "png-rs-enc/1";

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: e.to_string(),
    }
}

fn enc_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Encode {
        codec: ENC_ID.to_string(),
        message: e.to_string(),
    }
}

/// PNG の色の種類とビット深度から、値の画素の形式を決める（tRNS があればアルファ付きに）。
fn target_format(
    color: png::ColorType,
    depth: png::BitDepth,
    has_trns: bool,
) -> Result<PixelFormat, EngineError> {
    use png::{BitDepth as B, ColorType as C};
    let sixteen = depth == B::Sixteen;
    Ok(match (color, sixteen, has_trns) {
        (C::Indexed, _, _) => PixelFormat::Indexed8,
        (C::Grayscale, false, false) => PixelFormat::Gray8,
        (C::Grayscale, false, true) => PixelFormat::GrayA8,
        (C::Grayscale, true, false) => PixelFormat::Gray16,
        (C::Grayscale, true, true) => PixelFormat::GrayA16,
        (C::GrayscaleAlpha, false, _) => PixelFormat::GrayA8,
        (C::GrayscaleAlpha, true, _) => PixelFormat::GrayA16,
        (C::Rgb, false, false) => PixelFormat::Rgb8,
        (C::Rgb, false, true) => PixelFormat::Rgba8,
        (C::Rgb, true, false) => PixelFormat::Rgb16,
        (C::Rgb, true, true) => PixelFormat::Rgba16,
        (C::Rgba, false, _) => PixelFormat::Rgba8,
        (C::Rgba, true, _) => PixelFormat::Rgba16,
    })
}

/// png の読み手を作る。png クレートの上限（`Limits.bytes`）は、行の作業領域とメタデータのチャンク（ICC の展開を含む）に使われるので、
/// 画素の上限ではなくメタデータの上限を渡す。形だけ読むときは ICC とテキストを読まない（展開もしない）。
fn reader<'a>(
    src: &'a mut dyn ReadSeek,
    limits: &Limits,
    header_only: bool,
) -> Result<png::Reader<BufReader<&'a mut dyn ReadSeek>>, EngineError> {
    let bytes = usize::try_from(limits.max_meta_bytes).unwrap_or(usize::MAX);
    let mut dec = png::Decoder::new_with_limits(BufReader::new(src), png::Limits { bytes });
    dec.set_transformations(png::Transformations::IDENTITY);
    if header_only {
        dec.set_ignore_iccp_chunk(true);
        dec.set_ignore_text_chunk(true);
    }
    dec.read_info().map_err(err)
}

/// fcTL の中身。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fctl {
    w: u32,
    h: u32,
    x: u32,
    y: u32,
    delay_num: u16,
    delay_den: u16,
    /// 0 NONE・1 BACKGROUND・2 PREVIOUS。
    dispose: u8,
    /// 0 SOURCE・1 OVER。
    blend: u8,
}

/// チャンクの並びを先に読んだ結果（画素は読まない）。IHDR・PLTE・tRNS は png クレートの読み方（`read_info`）に任せる。
#[derive(Debug, Default)]
struct Scan {
    /// acTL（フレームの数・繰り返し）。
    actl: Option<(u32, u32)>,
    /// fcTL（並びの順）。
    frames: Vec<Fctl>,
    /// 最初の IDAT より前に fcTL があった（既定の画像がアニメの最初のフレーム）。
    default_is_frame: bool,
    /// IDAT より前の eXIf（`Exif\0\0` は外す）。
    exif: Option<Vec<u8>>,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// `n` バイトを読み捨てる（seek しない。BufReader の読み置きを捨てないように、前から順に読む）。
fn skip(src: &mut dyn ReadSeek, n: u64) -> Result<(), EngineError> {
    let copied = std::io::copy(&mut src.take(n), &mut std::io::sink())?;
    if copied != n {
        return Err(err("チャンクが途中で切れています"));
    }
    Ok(())
}

/// チャンクの並びを前から順に読む。acTL・fcTL・eXIf だけ中身を読み、ほかは読み捨てる（CRC は見ない）。
/// 読み終えたら位置は不定（呼ぶ側が巻き戻す）。
fn scan(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Scan, EngineError> {
    src.rewind()?;
    let mut sig = [0u8; 8];
    src.read_exact(&mut sig)
        .map_err(|_| err("PNG ではありません"))?;
    if &sig != b"\x89PNG\r\n\x1a\n" {
        return Err(err("PNG ではありません"));
    }
    let mut s = Scan::default();
    let mut seen_idat = false;
    loop {
        let mut head = [0u8; 8];
        // 壊れた末尾（途中で切れたファイル）は png クレートの読み取りに任せる
        if src.read_exact(&mut head).is_err() {
            break;
        }
        let len = u64::from(be32(&head, 0));
        let kind = [head[4], head[5], head[6], head[7]];
        let mut read_body = |max: u64| -> Result<Vec<u8>, EngineError> {
            if len > max {
                return Err(err(format!(
                    "{} チャンクが大きすぎます",
                    String::from_utf8_lossy(&kind)
                )));
            }
            let mut b = try_vec(len as usize, 0u8)?;
            src.read_exact(&mut b)
                .map_err(|_| err("チャンクが途中で切れています"))?;
            Ok(b)
        };
        let consumed = match &kind {
            b"acTL" if !seen_idat => {
                let b = read_body(8)?;
                if b.len() == 8 {
                    s.actl = Some((be32(&b, 0), be32(&b, 4)));
                }
                true
            }
            b"fcTL" => {
                let b = read_body(26)?;
                if b.len() != 26 {
                    return Err(err("fcTL が正しくありません"));
                }
                if s.frames.len() as u64 >= limits.max_frames {
                    return Err(EngineError::Limit(format!(
                        "フレームが {} を超えます",
                        limits.max_frames
                    )));
                }
                if !seen_idat && s.frames.is_empty() {
                    s.default_is_frame = true;
                }
                s.frames.push(Fctl {
                    w: be32(&b, 4),
                    h: be32(&b, 8),
                    x: be32(&b, 12),
                    y: be32(&b, 16),
                    delay_num: u16::from_be_bytes([b[20], b[21]]),
                    delay_den: u16::from_be_bytes([b[22], b[23]]),
                    dispose: b[24],
                    blend: b[25],
                });
                true
            }
            b"eXIf" if !seen_idat && s.exif.is_none() => {
                let b = read_body(limits.max_meta_bytes)?;
                s.exif = Some(exif::strip_header(&b).to_vec());
                true
            }
            b"IEND" => break,
            _ => {
                if &kind == b"IDAT" {
                    seen_idat = true;
                }
                false
            }
        };
        if !consumed {
            skip(src, len)?;
        }
        // CRC
        skip(src, 4)?;
    }
    Ok(s)
}

/// 読み方の計画（形と中身の両方がこれに従う）。
struct Plan {
    w: u32,
    h: u32,
    /// フレームの列にするなら、そのフレーム（アニメの順）。
    frames: Option<Vec<Fctl>>,
    plays: u32,
    /// 値の画素の形式（向きを当てる前後で同じ）。
    fmt: PixelFormat,
    /// フレームを重ねるときに、パレットの番号のまま重ねるか（Indexed8 の APNG）。消す色の番号。
    clear_index: u8,
    orientation: u8,
}

/// 計画を決める。色・深さ・パレット・tRNS は png クレートが読んだもの（`read_info` の結果。形だけ読むときも同じ）を使う。
fn plan_of(s: &Scan, info: &png::Info<'_>) -> Result<Plan, EngineError> {
    let (color, depth) = (info.color_type, info.bit_depth);
    let trns = info.trns.as_deref();
    let base = target_format(
        color,
        depth,
        trns.is_some() && color != png::ColorType::Indexed,
    )?;
    let orientation = s.exif.as_deref().map_or(1, exif::orientation);
    let (w, h) = (info.width, info.height);
    let animated = match s.actl {
        Some((n, _)) if !s.frames.is_empty() => {
            if n as usize != s.frames.len() {
                return Err(err("acTL のフレームの数と fcTL の数が合いません"));
            }
            true
        }
        _ => false,
    };
    if !animated {
        return Ok(Plan {
            w,
            h,
            frames: None,
            plays: 0,
            fmt: base,
            clear_index: 0,
            orientation,
        });
    }
    for f in &s.frames {
        let inside = f.w > 0
            && f.h > 0
            && f.x.checked_add(f.w).is_some_and(|r| r <= w)
            && f.y.checked_add(f.h).is_some_and(|b| b <= h);
        if !inside || f.dispose > 2 || f.blend > 1 {
            return Err(err("fcTL の値が正しくありません"));
        }
    }
    let mut fmt = base;
    let mut clear_index = 0u8;
    if base == PixelFormat::Indexed8 {
        let palette_len = info.palette.as_ref().map_or(0, |p| p.len() / 3);
        let alpha = |i: usize| trns.and_then(|t| t.get(i)).copied().unwrap_or(255);
        let partial = (0..palette_len).any(|i| (1..=254).contains(&alpha(i)));
        let over = s.frames.iter().any(|f| f.blend == 1);
        let first = s.frames[0];
        let needs_clear = s.frames.iter().any(|f| f.dispose == 1)
            || first.dispose == 2
            || !(first.x == 0 && first.y == 0 && first.w == w && first.h == h);
        let clear = (0..palette_len.min(256)).find(|i| alpha(*i) == 0);
        if (partial && over) || (needs_clear && clear.is_none()) {
            fmt = PixelFormat::Rgba8;
        } else {
            clear_index = clear.unwrap_or(0) as u8;
        }
    }
    Ok(Plan {
        w,
        h,
        plays: s.actl.map_or(0, |a| a.1),
        frames: Some(s.frames.clone()),
        fmt,
        clear_index,
        orientation,
    })
}

fn shape_of(p: &Plan) -> Shape {
    let (w, h) = orient::oriented_size(p.w, p.h, p.orientation);
    match &p.frames {
        Some(frames) => Shape::Frames {
            count: Dim::Exact(frames.len() as u64),
            w: Dim::Exact(u64::from(w)),
            h: Dim::Exact(u64::from(h)),
            fmt: p.fmt,
            full: Fullness::Full,
        },
        None => Shape::Image {
            w: Dim::Exact(u64::from(w)),
            h: Dim::Exact(u64::from(h)),
            fmt: p.fmt,
        },
    }
}

/// eXIf に使ったメタデータの予算を引いた上限（ICC・テキストは png クレートがこの残りで抑える）。
fn rest_of(limits: &Limits, s: &Scan) -> Limits {
    let used = s.exif.as_ref().map_or(0, |e| e.len() as u64);
    Limits {
        max_meta_bytes: limits.max_meta_bytes.saturating_sub(used),
        ..limits.clone()
    }
}

impl Decoder for PngCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "png"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let s = scan(src, limits)?;
        src.rewind()?;
        let r = reader(src, &rest_of(limits, &s), true)?;
        Ok(shape_of(&plan_of(&s, r.info())?))
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let s = scan(src, limits)?;
        src.rewind()?;
        let mut r = reader(src, &rest_of(limits, &s), false)?;
        let p = plan_of(&s, r.info())?;
        let mut meta = read_meta(r.info());
        if let Some(e) = &s.exif {
            meta.exif = Some(Arc::new(exif::with_orientation_reset(e)));
        }
        let Some(frames) = &p.frames else {
            let px = read_still(&mut r, mon)?;
            let px = orient::orient(&px, p.orientation, mon)?;
            return Ok(Value::Image(Arc::new(Image { px, meta })));
        };
        let out = read_animation(&mut r, &s, &p, frames, mon)?;
        Ok(Value::Frames(Arc::new(Frames::full(out, meta, p.plays)?)))
    }
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

/// 今の画像（既定の画像か、いまのフレーム）の行を読み、値の形式の行を上から順に `put` に渡す。
/// インターレースは、png クレートの行の並べ方に頼らず、行が来る順（Adam7 の段の順）から自分で戻す。
#[allow(clippy::too_many_arguments)]
fn read_rows<R: std::io::BufRead + std::io::Seek>(
    r: &mut png::Reader<R>,
    w: u32,
    h: u32,
    interlaced: bool,
    palette_len: Option<usize>,
    mon: &dyn Monitor,
    mut put: impl FnMut(&[u8]) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let (color, depth) = (r.info().color_type, r.info().bit_depth);
    let trns: Option<Vec<u8>> = r.info().trns.as_ref().map(|t| t.to_vec());
    let fmt = target_format(
        color,
        depth,
        color != png::ColorType::Indexed && trns.is_some(),
    )?;
    let bpp = fmt.bytes_per_pixel();
    let (bits, channels) = (depth as u8, color.samples());
    let row_len = (w as usize)
        .checked_mul(bpp)
        .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?;
    let check = |row: &[u8]| -> Result<(), EngineError> {
        match palette_len {
            Some(n) if row.iter().any(|i| usize::from(*i) >= n) => {
                Err(err("パレットにない番号の画素があります"))
            }
            _ => Ok(()),
        }
    };
    if !interlaced {
        let mut dst = try_vec(row_len, 0u8)?;
        for y in 0..h {
            let row = r
                .next_row()
                .map_err(err)?
                .ok_or_else(|| err("行が足りません"))?;
            unpack_row(
                row.data(),
                &mut dst,
                w as usize,
                color,
                bits,
                channels,
                trns.as_deref(),
            )?;
            check(&dst)?;
            put(&dst)?;
            if y % 256 == 255 && mon.canceled() {
                return Err(EngineError::Canceled);
            }
        }
        return Ok(());
    }
    let mut buf = try_vec(
        row_len
            .checked_mul(h as usize)
            .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?,
        0u8,
    )?;
    let mut tmp = try_vec(row_len, 0u8)?;
    for (x0, y0, dx, dy) in ADAM7 {
        let pw = if w > x0 { (w - x0).div_ceil(dx) } else { 0 };
        let ph = if h > y0 { (h - y0).div_ceil(dy) } else { 0 };
        if pw == 0 || ph == 0 {
            continue;
        }
        for line in 0..ph {
            let row = r
                .next_row()
                .map_err(err)?
                .ok_or_else(|| err("行が足りません"))?;
            let part = &mut tmp[..pw as usize * bpp];
            unpack_row(
                row.data(),
                part,
                pw as usize,
                color,
                bits,
                channels,
                trns.as_deref(),
            )?;
            let y = (y0 + line * dy) as usize;
            for i in 0..pw as usize {
                let x = x0 as usize + i * dx as usize;
                buf[y * row_len + x * bpp..y * row_len + (x + 1) * bpp]
                    .copy_from_slice(&part[i * bpp..(i + 1) * bpp]);
            }
        }
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
    }
    check(&buf)?;
    for row in buf.chunks_exact(row_len) {
        put(row)?;
    }
    Ok(())
}

/// 1 枚の画像を読む。インターレースでなければ行ごとに組む（作業領域は 1 行と 1 タイル行。値の大きさの写しを持たない）。
fn read_still<R: std::io::BufRead + std::io::Seek>(
    r: &mut png::Reader<R>,
    mon: &dyn Monitor,
) -> Result<Raster, EngineError> {
    let (w, h, color, depth, interlaced) = {
        let info = r.info();
        (
            info.width,
            info.height,
            info.color_type,
            info.bit_depth,
            info.interlaced,
        )
    };
    let trns: Option<Vec<u8>> = r.info().trns.as_ref().map(|t| t.to_vec());
    let fmt = target_format(
        color,
        depth,
        color != png::ColorType::Indexed && trns.is_some(),
    )?;
    let palette = palette_of(color, r.info().palette.as_deref(), trns.as_deref())?;
    let palette_len = palette.as_ref().map(|p| p.colors.len());
    let mut b = crate::value::RasterBuilder::new(w, h, fmt, palette)?;
    read_rows(r, w, h, interlaced, palette_len, mon, |row| b.push_row(row))?;
    b.finish()
}

/// PLTE と tRNS からパレットを作る（パレットの画像でなければ None）。
fn palette_of(
    color: png::ColorType,
    rgb: Option<&[u8]>,
    trns: Option<&[u8]>,
) -> Result<Option<Palette>, EngineError> {
    match (color, rgb) {
        (png::ColorType::Indexed, Some(rgb)) => {
            let alphas = trns.unwrap_or_default();
            Ok(Some(Palette {
                colors: rgb
                    .chunks_exact(3)
                    .enumerate()
                    .map(|(i, c)| [c[0], c[1], c[2], alphas.get(i).copied().unwrap_or(255)])
                    .collect(),
            }))
        }
        (png::ColorType::Indexed, None) => Err(err("パレットがありません")),
        _ => Ok(None),
    }
}

/// APNG のフレームを重ねて、キャンバスいっぱいのフレームの列にする。
/// Indexed8 の経路で消した画素は、消す色の番号（パレットの色、アルファ 0）になる（RGB は仕様の「透明な黒」と違うが見えない）。
fn read_animation<R: std::io::BufRead + std::io::Seek>(
    r: &mut png::Reader<R>,
    s: &Scan,
    p: &Plan,
    frames: &[Fctl],
    mon: &dyn Monitor,
) -> Result<Vec<(Raster, u32)>, EngineError> {
    let (cw, ch) = (p.w as usize, p.h as usize);
    let fmt = p.fmt;
    let bpp = fmt.bytes_per_pixel();
    let indexed = fmt == PixelFormat::Indexed8;
    let interlaced = r.info().interlaced;
    let color = r.info().color_type;
    let trns: Option<Vec<u8>> = r.info().trns.as_ref().map(|t| t.to_vec());
    let src_palette = palette_of(color, r.info().palette.as_deref(), trns.as_deref())?;
    let src_fmt = target_format(
        color,
        r.info().bit_depth,
        color != png::ColorType::Indexed && trns.is_some(),
    )?;
    let palette_len = src_palette.as_ref().map(|q| q.colors.len());
    let colors: Option<Vec<[u8; 4]>> = src_palette.as_ref().map(|q| q.colors.clone());
    let alpha_of = |i: u8| {
        colors
            .as_ref()
            .and_then(|c| c.get(usize::from(i)))
            .map_or(255, |c| c[3])
    };
    let mut canvas = try_vec(cw * ch * bpp, 0u8)?;
    if indexed {
        canvas.fill(p.clear_index);
    }
    let out_palette = if indexed { src_palette.clone() } else { None };
    let mut out = Vec::new();
    let mut wide = Vec::new();
    for (k, f) in frames.iter().enumerate() {
        // 既定の画像がアニメの最初のフレームなら、読み手はもうその画素の前にいる。ほかは次のフレームへ進む
        // （既定の画像がアニメの外なら、その画素は読み捨てられる）
        if k > 0 || !s.default_is_frame {
            let fc = r.next_frame_info().map_err(err)?;
            if (fc.width, fc.height, fc.x_offset, fc.y_offset) != (f.w, f.h, f.x, f.y) {
                return Err(err("fcTL の読み方が合いません"));
            }
        }
        let previous = (f.dispose == 2).then(|| {
            let mut v = Vec::with_capacity(f.w as usize * f.h as usize * bpp);
            for y in 0..f.h as usize {
                let start = ((f.y as usize + y) * cw + f.x as usize) * bpp;
                v.extend_from_slice(&canvas[start..start + f.w as usize * bpp]);
            }
            v
        });
        let mut y = 0usize;
        read_rows(r, f.w, f.h, interlaced, palette_len, mon, |row| {
            // フレームの画素を値の形式へ（パレットの画像を Rgba8 で重ねるときは広げる）
            let row: &[u8] = if src_fmt != fmt {
                wide.resize(f.w as usize * 4, 0);
                convert::expand_into(src_fmt, colors.as_deref(), row, &mut wide)?;
                &wide
            } else {
                row
            };
            for x in 0..f.w as usize {
                let src = &row[x * bpp..(x + 1) * bpp];
                let at = ((f.y as usize + y) * cw + f.x as usize + x) * bpp;
                let dst = &mut canvas[at..at + bpp];
                if f.blend == 0 {
                    dst.copy_from_slice(src);
                } else if indexed {
                    // 計画で、半透明の色を OVER で重ねないことを確かめてある
                    if alpha_of(src[0]) == 255 {
                        dst.copy_from_slice(src);
                    }
                } else {
                    composite::over(fmt, dst, src)?;
                }
            }
            y += 1;
            Ok(())
        })?;
        let delay_den = if f.delay_den == 0 {
            100
        } else {
            u64::from(f.delay_den)
        };
        let ms = (u64::from(f.delay_num) * 1000 + delay_den / 2) / delay_den;
        let frame = Raster::from_rows(p.w, p.h, fmt, out_palette.clone(), &canvas)?;
        out.push((orient::orient(&frame, p.orientation, mon)?, ms as u32));
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        // 後始末（最初のフレームの PREVIOUS は、描く前の透明なキャンバスに戻るので BACKGROUND と同じ）
        match (f.dispose, previous) {
            (2, Some(prev)) => {
                for y in 0..f.h as usize {
                    let start = ((f.y as usize + y) * cw + f.x as usize) * bpp;
                    let n = f.w as usize * bpp;
                    canvas[start..start + n].copy_from_slice(&prev[y * n..(y + 1) * n]);
                }
            }
            (1, _) => {
                let fill = if indexed { p.clear_index } else { 0 };
                for y in 0..f.h as usize {
                    let start = ((f.y as usize + y) * cw + f.x as usize) * bpp;
                    canvas[start..start + f.w as usize * bpp].fill(fill);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

impl Decoder for PngDecoderV1 {
    fn id(&self) -> &'static str {
        DEC_ID_V1
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "png"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let r = reader(src, limits, true)?;
        let info = r.info();
        let fmt = target_format(
            info.color_type,
            info.bit_depth,
            info.trns.is_some() && info.color_type != png::ColorType::Indexed,
        )?;
        Ok(Shape::Image {
            w: Dim::Exact(u64::from(info.width)),
            h: Dim::Exact(u64::from(info.height)),
            fmt,
        })
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let mut r = reader(src, limits, false)?;
        let meta = read_meta(r.info());
        let px = read_still(&mut r, mon)?;
        Ok(Value::Image(Arc::new(Image { px, meta })))
    }
}

/// 1 行を値の形式へ写す（ビットの詰め、ビッグエンディアン → リトルエンディアン、tRNS → アルファ）。
fn unpack_row(
    row: &[u8],
    dst: &mut [u8],
    w: usize,
    color: png::ColorType,
    bits: u8,
    channels: usize,
    trns: Option<&[u8]>,
) -> Result<(), EngineError> {
    use png::ColorType as C;
    if bits < 8 {
        // グレーかパレットの 1/2/4 ビット
        let per_byte = 8 / bits as usize;
        let mask = (1u16 << bits) - 1;
        let scale = if color == C::Grayscale { 255 / mask } else { 1 };
        let trns_value = trns.and_then(|t| trns_sample(t, 0, false));
        let alpha_out = color == C::Grayscale && trns_value.is_some();
        for x in 0..w {
            let byte = *row.get(x / per_byte).ok_or_else(|| err("行が短すぎます"))?;
            let shift = 8 - bits as usize * (x % per_byte + 1);
            let v = (u16::from(byte) >> shift) & mask;
            if alpha_out {
                dst[x * 2] = (v * scale) as u8;
                dst[x * 2 + 1] = if Some(v) == trns_value { 0 } else { 255 };
            } else {
                dst[x] = (v * scale) as u8;
            }
        }
        return Ok(());
    }
    let sample_bytes = bits as usize / 8;
    let src_px = channels * sample_bytes;
    if row.len() < w * src_px {
        return Err(err("行が短すぎます"));
    }
    let add_alpha = matches!(color, C::Grayscale | C::Rgb) && trns.is_some();
    let dst_px = (channels + usize::from(add_alpha)) * sample_bytes;
    for x in 0..w {
        let s = &row[x * src_px..(x + 1) * src_px];
        let d = &mut dst[x * dst_px..(x + 1) * dst_px];
        if sample_bytes == 2 {
            for c in 0..channels {
                d[c * 2] = s[c * 2 + 1];
                d[c * 2 + 1] = s[c * 2];
            }
        } else {
            d[..src_px].copy_from_slice(s);
        }
        if add_alpha {
            let t = trns.unwrap_or_default();
            let matches = (0..channels).all(|c| {
                let sv = if sample_bytes == 2 {
                    u16::from_be_bytes([s[c * 2], s[c * 2 + 1]])
                } else {
                    u16::from(s[c])
                };
                trns_sample(t, c, sample_bytes == 2) == Some(sv)
            });
            let a = channels * sample_bytes;
            if sample_bytes == 2 {
                let alpha: u16 = if matches { 0 } else { u16::MAX };
                d[a..a + 2].copy_from_slice(&alpha.to_le_bytes());
            } else {
                d[a] = if matches { 0 } else { 255 };
            }
        }
    }
    Ok(())
}

/// tRNS のサンプルの値。png クレートは 16 ビット未満なら下位の 1 バイトだけ、16 ビットなら 2 バイト（ビッグエンディアン）で持つ。
fn trns_sample(t: &[u8], c: usize, sixteen: bool) -> Option<u16> {
    if sixteen {
        t.get(c * 2..c * 2 + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
    } else {
        t.get(c).map(|b| u16::from(*b))
    }
}

fn read_meta(info: &png::Info<'_>) -> ImageMeta {
    let mut meta = ImageMeta::default();
    if let Some(icc) = &info.icc_profile {
        meta.icc = Some(Arc::new(icc.to_vec()));
    }
    if let Some(d) = info.pixel_dims
        && d.unit == png::Unit::Meter
    {
        // 1 メートルあたり → 1 インチあたり（四捨五入。整数で計算する）
        let dpi =
            |ppm: u32| u32::try_from((u64::from(ppm) * 254 + 5_000) / 10_000).unwrap_or(u32::MAX);
        meta.dpi = Some((dpi(d.xppu), dpi(d.yppu)));
    }
    for t in &info.uncompressed_latin1_text {
        meta.text
            .entry(t.keyword.clone())
            .or_insert_with(|| t.text.clone());
    }
    for t in info.utf8_text.iter().filter(|t| !t.compressed) {
        if let Ok(text) = t.get_text() {
            meta.text.entry(t.keyword.clone()).or_insert(text);
        }
    }
    meta
}

impl Encoder for PngCodec {
    fn id(&self) -> &'static str {
        ENC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "png"
            && match shape {
                Shape::Image { .. } => true,
                // APNG はキャンバスいっぱいのフレームだけ（トリムしたフレームは先に広げる）
                Shape::Frames { full, .. } => *full == Fullness::Full,
                Shape::Audio { .. } => false,
            }
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "compression",
                ParamKind::Enum {
                    values: &["fast", "balanced", "high"],
                },
                Unit::None,
                json!("balanced"),
            )],
        }
    }

    fn caps(&self, format: &str, _opts: &serde_json::Value) -> MetaCaps {
        caps_of(format)
    }

    fn encode(
        &self,
        value: &Value,
        _format: &str,
        opts: &serde_json::Value,
        out: &mut dyn Write,
        mon: &dyn Monitor,
    ) -> Result<u64, EngineError> {
        let (frames, meta, plays): (Vec<(&Raster, u32)>, &ImageMeta, Option<u32>) = match value {
            Value::Image(img) => (vec![(&img.px, 0)], &img.meta, None),
            Value::Frames(fr) if fr.is_full() => (
                fr.frames().iter().map(|f| (&f.px, f.duration_ms)).collect(),
                &fr.meta,
                Some(fr.loop_count),
            ),
            _ => {
                return Err(EngineError::Unsupported(
                    "PNG には画像か、キャンバスいっぱいのフレームの列だけを書けます".into(),
                ));
            }
        };
        let Some((px, _)) = frames.first().copied() else {
            return Err(EngineError::Unsupported("フレームがありません".into()));
        };
        let (color, depth) = match px.format() {
            PixelFormat::Gray8 => (png::ColorType::Grayscale, png::BitDepth::Eight),
            PixelFormat::GrayA8 => (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight),
            PixelFormat::Rgb8 => (png::ColorType::Rgb, png::BitDepth::Eight),
            PixelFormat::Rgba8 => (png::ColorType::Rgba, png::BitDepth::Eight),
            PixelFormat::Gray16 => (png::ColorType::Grayscale, png::BitDepth::Sixteen),
            PixelFormat::GrayA16 => (png::ColorType::GrayscaleAlpha, png::BitDepth::Sixteen),
            PixelFormat::Rgb16 => (png::ColorType::Rgb, png::BitDepth::Sixteen),
            PixelFormat::Rgba16 => (png::ColorType::Rgba, png::BitDepth::Sixteen),
            PixelFormat::Indexed8 => (png::ColorType::Indexed, png::BitDepth::Eight),
        };
        let compression = match opts
            .get("compression")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("balanced")
        {
            "fast" => png::Compression::Fast,
            "high" => png::Compression::High,
            _ => png::Compression::Balanced,
        };
        let rows_of = |px: &Raster| -> Result<Vec<u8>, EngineError> {
            let mut rows = px.to_rows()?;
            if depth == png::BitDepth::Sixteen {
                // 値はリトルエンディアン、PNG はビッグエンディアン
                for pair in rows.chunks_exact_mut(2) {
                    pair.swap(0, 1);
                }
            }
            Ok(rows)
        };
        let mut info = png::Info::with_size(px.width(), px.height());
        info.color_type = color;
        info.bit_depth = depth;
        if let Some(p) = px.palette() {
            let rgb: Vec<u8> = p.colors.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
            let mut alphas: Vec<u8> = p.colors.iter().map(|c| c[3]).collect();
            while alphas.last() == Some(&255) {
                alphas.pop();
            }
            info.palette = Some(Cow::Owned(rgb));
            if !alphas.is_empty() {
                info.trns = Some(Cow::Owned(alphas));
            }
        }
        if let Some((x, y)) = meta.dpi {
            // 1 インチあたり → 1 メートルあたり（四捨五入）
            let ppm =
                |dpi: u32| u32::try_from((u64::from(dpi) * 10_000 + 127) / 254).unwrap_or(u32::MAX);
            info.pixel_dims = Some(png::PixelDimensions {
                xppu: ppm(x),
                yppu: ppm(y),
                unit: png::Unit::Meter,
            });
        }
        if let Some(icc) = &meta.icc {
            info.icc_profile = Some(Cow::Owned(icc.as_ref().clone()));
        }
        if let Some(e) = &meta.exif {
            info.exif_metadata = Some(Cow::Owned(e.as_ref().clone()));
        }
        let mut counter = CountingWriter {
            inner: out,
            count: 0,
        };
        {
            let mut enc = png::Encoder::with_info(&mut counter, info).map_err(enc_err)?;
            enc.set_compression(compression);
            if let Some(plays) = plays {
                enc.set_animated(frames.len() as u32, plays)
                    .map_err(enc_err)?;
            }
            for (k, v) in &meta.text {
                if v.is_ascii() && k.is_ascii() {
                    enc.add_text_chunk(k.clone(), v.clone()).map_err(enc_err)?;
                } else {
                    enc.add_itxt_chunk(k.clone(), v.clone()).map_err(enc_err)?;
                }
            }
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let mut writer = enc.write_header().map_err(enc_err)?;
            for (frame, ms) in &frames {
                if plays.is_some() {
                    let (num, den) = delay_fraction(*ms);
                    writer.set_frame_delay(num, den).map_err(enc_err)?;
                }
                writer.write_image_data(&rows_of(frame)?).map_err(enc_err)?;
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
            }
            writer.finish().map_err(enc_err)?;
        }
        Ok(counter.count)
    }
}

/// 表示時間（ms）を fcTL の分数（秒）にする。65,535 ms までは ms のまま（読むと同じ値に戻る）、
/// それより長いと 10 ms、さらに長いと 1 秒の単位に四捨五入する。
fn delay_fraction(ms: u32) -> (u16, u16) {
    if let Ok(n) = u16::try_from(ms) {
        (n, 1000)
    } else if let Ok(n) = u16::try_from((u64::from(ms) + 5) / 10) {
        (n, 100)
    } else {
        (
            u16::try_from((u64::from(ms) + 500) / 1000).unwrap_or(u16::MAX),
            1,
        )
    }
}

/// 書いたバイト数を数える。
struct CountingWriter<'a> {
    inner: &'a mut dyn Write,
    count: u64,
}

impl Write for CountingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::NoMonitor;
    use crate::testgen::InputGen;
    use std::io::Cursor;

    fn round_trip(v: &Value, opts: serde_json::Value) -> Value {
        let set = CodecSet::pinned();
        let (bytes, stamp) = set.encode_bytes(v, "png", &opts, &NoMonitor).unwrap();
        assert_eq!(stamp.bytes as usize, bytes.len());
        let (back, dstamp) = set
            .decode(
                &mut Cursor::new(bytes),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(dstamp.codec, DEC_ID);
        back
    }

    /// すべての画素の形式で、書いて読むと同じ値に戻る（パレット・16 ビット・メタデータを含む。E2 の一部）。
    #[test]
    fn round_trips_every_pixel_format() {
        for fmt in PixelFormat::ALL {
            let Value::Image(img) = (InputGen::Noise {
                w: 300,
                h: 7,
                fmt,
                seed: 3,
            })
            .generate()
            .unwrap() else {
                panic!()
            };
            let mut img = Arc::unwrap_or_clone(img);
            img.meta.dpi = Some((144, 72));
            img.meta.text.insert("Software".into(), "Kura".into());
            img.meta.text.insert("Title".into(), "ドット".into());
            img.meta.icc = Some(Arc::new(vec![9, 8, 7]));
            let v = Value::Image(Arc::new(img));
            for c in ["fast", "balanced", "high"] {
                assert_eq!(
                    round_trip(&v, json!({ "compression": c })),
                    v,
                    "{fmt:?} {c}"
                );
            }
        }
    }

    /// 同じ値からは同じバイト列ができる（R2）。
    #[test]
    fn encoding_is_deterministic() {
        let v = (InputGen::Noise {
            w: 517,
            h: 300,
            fmt: PixelFormat::Rgba8,
            seed: 4,
        })
        .generate()
        .unwrap();
        let set = CodecSet::pinned();
        let a = set.encode_bytes(&v, "png", &json!({}), &NoMonitor).unwrap();
        let b = set.encode_bytes(&v, "png", &json!({}), &NoMonitor).unwrap();
        assert_eq!(a.0, b.0);
        assert_eq!(a.1.file_digest, b.1.file_digest);
    }

    /// 1/2/4 ビットのグレー・パレットと、グレー・RGB の tRNS を正しく読む。
    #[test]
    fn decodes_low_bit_depths_and_trns() {
        // 2 ビットのグレー、4 画素：0,1,2,3 → 0,85,170,255。tRNS で 1 を透明に
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, 4, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Two);
            enc.set_trns(vec![0u8, 1]);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0b0001_1011]).unwrap();
        }
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = v else { panic!() };
        assert_eq!(img.px.format(), PixelFormat::GrayA8);
        assert_eq!(
            img.px.to_rows().unwrap(),
            vec![0, 255, 85, 0, 170, 255, 255, 255]
        );

        // 8 ビットの RGB の tRNS（(1,2,3) を透明に）と、16 ビットのグレーの tRNS
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, 2, 1);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_trns(vec![0u8, 1, 0, 2, 0, 3]);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[1, 2, 3, 1, 2, 4]).unwrap();
        }
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = v else { panic!() };
        assert_eq!(img.px.to_rows().unwrap(), vec![1, 2, 3, 0, 1, 2, 4, 255]);
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, 2, 1);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Sixteen);
            enc.set_trns(vec![0x12u8, 0x34]);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0x12, 0x34, 0x12, 0x35]).unwrap();
        }
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = v else { panic!() };
        assert_eq!(img.px.format(), PixelFormat::GrayA16);
        assert_eq!(
            img.px.to_rows().unwrap(),
            vec![0x34, 0x12, 0, 0, 0x35, 0x12, 0xff, 0xff]
        );

        // 1 ビットのパレット（2 色、0 番は半透明）
        let mut bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut bytes, 9, 1);
            enc.set_color(png::ColorType::Indexed);
            enc.set_depth(png::BitDepth::One);
            enc.set_palette(vec![10u8, 20, 30, 40, 50, 60]);
            enc.set_trns(vec![128u8]);
            let mut w = enc.write_header().unwrap();
            w.write_image_data(&[0b1010_1010, 0b1000_0000]).unwrap();
        }
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = v else { panic!() };
        assert_eq!(img.px.format(), PixelFormat::Indexed8);
        assert_eq!(img.px.to_rows().unwrap(), vec![1, 0, 1, 0, 1, 0, 1, 0, 1]);
        assert_eq!(
            img.px.palette().unwrap().colors,
            vec![[10, 20, 30, 128], [40, 50, 60, 255]]
        );
    }

    /// 壊れたファイル・上限を超えるファイルは、落ちずにエラーになる（E25）。壊したあとで CRC を付け直し、
    /// png クレートの検査の先（自前の行の展開・パレットの番号の確かめ）まで届かせる。panic を受け止めずに直接呼ぶ。
    #[test]
    fn broken_or_huge_files_are_errors() {
        use crate::testfiles::{FileGen, crc32};
        let set = CodecSet::pinned();
        let v = (InputGen::Noise {
            w: 64,
            h: 64,
            fmt: PixelFormat::Rgb8,
            seed: 5,
        })
        .generate()
        .unwrap();
        let (bytes, _) = set.encode_bytes(&v, "png", &json!({}), &NoMonitor).unwrap();
        let small = Limits {
            max_dim: 32,
            ..Limits::default()
        };
        assert!(matches!(
            set.decode(
                &mut Cursor::new(bytes.clone()),
                "png",
                &DecodeOptions::default(),
                &small,
                &NoMonitor
            ),
            Err(EngineError::Limit(_))
        ));
        // チャンクの中身だけを壊して CRC を付け直す
        fn mutate(bytes: &[u8], r: &mut rand_chacha::ChaCha8Rng) -> Vec<u8> {
            use rand_chacha::rand_core::Rng;
            let mut out = bytes.to_vec();
            let mut chunks = Vec::new();
            let mut o = 8usize;
            while o + 12 <= out.len() {
                let len = u32::from_be_bytes([out[o], out[o + 1], out[o + 2], out[o + 3]]) as usize;
                if o + 12 + len > out.len() {
                    break;
                }
                chunks.push((o, len));
                o += 12 + len;
            }
            for _ in 0..1 + r.next_u32() % 4 {
                let (o, len) = chunks[r.next_u32() as usize % chunks.len()];
                if len > 0 {
                    let i = o + 8 + r.next_u32() as usize % len;
                    out[i] = r.next_u32() as u8;
                }
                let crc = crc32(&out[o + 4..o + 8 + len]);
                out[o + 8 + len..o + 12 + len].copy_from_slice(&crc.to_be_bytes());
            }
            out
        }
        let sources = [
            bytes,
            FileGen::Png {
                w: 23,
                h: 9,
                color: 3,
                depth: 4,
                interlace: true,
                palette_len: 11,
                trns: Some(vec![0, 128]),
                text: vec![],
                icc: None,
                phys: None,
                seed: 1,
                exif: None,
            }
            .bytes()
            .unwrap(),
            FileGen::Png {
                w: 17,
                h: 5,
                color: 0,
                depth: 2,
                interlace: false,
                palette_len: 0,
                trns: Some(vec![0, 1]),
                text: vec![("k".into(), "v".into())],
                icc: Some(vec![1; 100]),
                phys: Some((2835, 2835, 1)),
                seed: 2,
                exif: None,
            }
            .bytes()
            .unwrap(),
        ];
        let mut r = crate::determinism::rng(9);
        let (mut ok, mut failed) = (0, 0);
        // IHDR の幅・高さが壊れても巨大な画像を読まないよう、上限を絞る（形を上限で確かめる経路も通る）
        let tight = Limits {
            max_pixels: 1 << 16,
            max_meta_bytes: 1 << 16,
            ..Limits::default()
        };
        for src in &sources {
            for _ in 0..400 {
                let broken = mutate(src, &mut r);
                let shape = PngCodec.shape(
                    &mut Cursor::new(broken.clone()),
                    "png",
                    &DecodeOptions::default(),
                    &tight,
                );
                if !shape.is_ok_and(|s| tight.check(&s).is_ok()) {
                    failed += 1;
                    continue;
                }
                match PngCodec.decode(
                    &mut Cursor::new(broken),
                    "png",
                    &DecodeOptions::default(),
                    &tight,
                    &NoMonitor,
                ) {
                    Ok(_) => ok += 1,
                    Err(_) => failed += 1,
                }
            }
        }
        // 壊し方が自前のコードまで届いている（読めるものも、壊れたと分かるものもある）
        assert!(ok > 50 && failed > 50, "ok {ok} failed {failed}");
    }

    /// インターレースの PNG は、同じ画素のインターレースなしの PNG と同じ値に読める（1/2/4/8/16 ビット、各色の種類）。
    #[test]
    fn interlaced_files_decode_to_the_same_value() {
        use crate::testfiles::FileGen;
        for (color, depth, palette_len) in [
            (0u8, 1u8, 0u16),
            (0, 2, 0),
            (0, 4, 0),
            (0, 16, 0),
            (2, 8, 0),
            (2, 16, 0),
            (3, 1, 2),
            (3, 4, 16),
            (3, 8, 200),
            (4, 8, 0),
            (6, 16, 0),
        ] {
            for (w, h) in [(1, 1), (7, 3), (13, 17), (33, 9)] {
                let make = |interlace| FileGen::Png {
                    w,
                    h,
                    color,
                    depth,
                    interlace,
                    palette_len,
                    trns: None,
                    text: vec![],
                    icc: None,
                    phys: None,
                    seed: 7,
                    exif: None,
                };
                let decode = |g: FileGen| {
                    PngCodec
                        .decode(
                            &mut Cursor::new(g.bytes().unwrap()),
                            "png",
                            &DecodeOptions::default(),
                            &Limits::default(),
                            &NoMonitor,
                        )
                        .unwrap()
                };
                assert_eq!(
                    decode(make(true)),
                    decode(make(false)),
                    "色 {color}・{depth} ビット・{w}×{h}"
                );
            }
        }
    }

    /// メタデータの上限：形だけ読むときは ICC を読まず、中身を読むときは上限を超える ICC で断る。
    #[test]
    fn metadata_is_bounded() {
        use crate::testfiles::FileGen;
        let bytes = FileGen::Png {
            w: 1,
            h: 1,
            color: 0,
            depth: 8,
            interlace: false,
            palette_len: 0,
            trns: None,
            text: vec![],
            icc: Some(vec![7; 5_000]),
            phys: None,
            seed: 1,
            exif: None,
        }
        .bytes()
        .unwrap();
        let small = Limits {
            max_meta_bytes: 1_000,
            ..Limits::default()
        };
        assert!(
            PngCodec
                .shape(
                    &mut Cursor::new(bytes.clone()),
                    "png",
                    &DecodeOptions::default(),
                    &small
                )
                .is_ok()
        );
        assert!(
            PngCodec
                .decode(
                    &mut Cursor::new(bytes.clone()),
                    "png",
                    &DecodeOptions::default(),
                    &small,
                    &NoMonitor
                )
                .is_err()
        );
        let (v, _) = CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "png",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(img) = v else { panic!() };
        assert_eq!(img.meta.icc.as_deref().map(Vec::len), Some(5_000));
    }

    fn decode_with(d: &dyn Decoder, bytes: Vec<u8>) -> Result<Value, EngineError> {
        let shape = d.shape(
            &mut Cursor::new(bytes.clone()),
            "png",
            &DecodeOptions::default(),
            &Limits::default(),
        )?;
        let v = d.decode(
            &mut Cursor::new(bytes),
            "png",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )?;
        // 形と中身は必ず一致する
        assert!(shape.admits(&v.shape()), "{shape:?} {:?}", v.shape());
        Ok(v)
    }

    fn frame(
        x: u32,
        y: u32,
        w: u32,
        h: u32,
        dispose: u8,
        blend: u8,
    ) -> crate::testfiles::ApngFrame {
        crate::testfiles::ApngFrame {
            x,
            y,
            w,
            h,
            delay: (7, 100),
            dispose,
            blend,
        }
    }

    /// APNG の重ね方：SOURCE・OVER と、dispose の NONE・BACKGROUND・PREVIOUS を、画素ごとの素直な計算と照らす。
    #[test]
    fn apng_frames_are_composited_by_the_spec() {
        use crate::testfiles::FileGen;
        let frames = vec![
            frame(0, 0, 8, 6, 0, 0),
            frame(2, 1, 4, 3, 1, 0),
            frame(1, 2, 5, 3, 2, 1),
            frame(0, 0, 8, 6, 0, 1),
        ];
        let anim = FileGen::Apng {
            w: 8,
            h: 6,
            color: 6,
            palette_len: 0,
            trns: None,
            separate_default: false,
            plays: 3,
            frames: frames.clone(),
            exif: None,
            seed: 4,
            interlace: false,
        };
        let v = decode_with(&PngCodec, anim.bytes().unwrap()).unwrap();
        let Value::Frames(fr) = &v else {
            panic!("{v:?}")
        };
        assert_eq!(fr.frames().len(), 4);
        assert_eq!(fr.loop_count, 3);
        assert!(fr.frames().iter().all(|f| f.duration_ms == 70));
        // 参照：各フレームの画素を v1（既定の画像だけ）では読めないので、フレームごとの APNG を 1 枚ずつ作って読む
        let sub = |k: usize| -> Vec<u8> {
            let f = &frames[k];
            // フレーム k と同じ中身を持つ 1 フレームの APNG（既定の画像の外に置き、同じ番号 k で作る）
            let mut fs: Vec<crate::testfiles::ApngFrame> =
                (0..=k).map(|_| frame(0, 0, 1, 1, 0, 0)).collect();
            fs[k] = frame(0, 0, f.w, f.h, 0, 0);
            let one = FileGen::Apng {
                w: f.w.max(1),
                h: f.h.max(1),
                color: 6,
                palette_len: 0,
                trns: None,
                separate_default: true,
                plays: 0,
                frames: fs,
                exif: None,
                seed: 4,
                interlace: false,
            };
            let Value::Frames(x) = decode_with(&PngCodec, one.bytes().unwrap()).unwrap() else {
                panic!()
            };
            x.frames()[k].px.to_rows().unwrap()
        };
        let mut canvas = vec![0u8; 8 * 6 * 4];
        for (k, f) in frames.iter().enumerate() {
            let data = sub(k);
            let before = canvas.clone();
            for y in 0..f.h as usize {
                for x in 0..f.w as usize {
                    let s = &data[(y * f.w as usize + x) * 4..][..4];
                    let at = ((f.y as usize + y) * 8 + f.x as usize + x) * 4;
                    if f.blend == 0 {
                        canvas[at..at + 4].copy_from_slice(s);
                    } else {
                        composite::over(PixelFormat::Rgba8, &mut canvas[at..at + 4], s).unwrap();
                    }
                }
            }
            assert_eq!(fr.frames()[k].px.to_rows().unwrap(), canvas, "フレーム {k}");
            for y in 0..f.h as usize {
                for x in 0..f.w as usize {
                    let at = ((f.y as usize + y) * 8 + f.x as usize + x) * 4;
                    match f.dispose {
                        1 => canvas[at..at + 4].fill(0),
                        2 => canvas[at..at + 4].copy_from_slice(&before[at..at + 4]),
                        _ => {}
                    }
                }
            }
        }
    }

    /// パレットの APNG：番号のまま重ねられるときだけ Indexed8、ほかは Rgba8。どちらでも、広げた色は同じ。
    #[test]
    fn indexed_apng_stays_indexed_only_when_exact() {
        use crate::testfiles::FileGen;
        let make =
            |trns: Option<Vec<u8>>, frames: Vec<crate::testfiles::ApngFrame>| FileGen::Apng {
                w: 6,
                h: 5,
                color: 3,
                palette_len: 8,
                trns,
                separate_default: false,
                plays: 0,
                frames,
                exif: None,
                seed: 9,
                interlace: false,
            };
        let fmt_of = |g: &FileGen| match decode_with(&PngCodec, g.bytes().unwrap()).unwrap() {
            Value::Frames(f) => f.frames()[0].px.format(),
            _ => panic!(),
        };
        let clear_and_over = vec![frame(0, 0, 6, 5, 1, 0), frame(1, 1, 3, 3, 0, 1)];
        // 透明な色（番号 1）があり、半透明がない → Indexed8
        assert_eq!(
            fmt_of(&make(Some(vec![255, 0]), clear_and_over.clone())),
            PixelFormat::Indexed8
        );
        // 消すのに透明な色がない → Rgba8
        assert_eq!(
            fmt_of(&make(None, clear_and_over.clone())),
            PixelFormat::Rgba8
        );
        // 半透明の色を OVER で重ねる → Rgba8
        assert_eq!(
            fmt_of(&make(Some(vec![0, 128]), clear_and_over)),
            PixelFormat::Rgba8
        );
        // 半透明があっても SOURCE だけで消さないなら Indexed8
        let source_only = vec![frame(0, 0, 6, 5, 0, 0), frame(2, 0, 2, 5, 0, 0)];
        assert_eq!(
            fmt_of(&make(Some(vec![0, 128]), source_only)),
            PixelFormat::Indexed8
        );
        // Indexed8 で読んだものを広げると、同じ重ね方を RGBA で手計算したものと、見える画素で同じ
        // （消した画素は、消す色の番号のパレットの RGB とアルファ 0。仕様の透明な黒とは RGB だけ違う）
        let frames = vec![
            frame(0, 0, 6, 5, 1, 0),
            frame(1, 1, 3, 3, 2, 1),
            frame(2, 0, 3, 4, 0, 0),
        ];
        let g = make(Some(vec![255, 0]), frames.clone());
        let Value::Frames(idx) = decode_with(&PngCodec, g.bytes().unwrap()).unwrap() else {
            panic!()
        };
        assert_eq!(idx.frames()[0].px.format(), PixelFormat::Indexed8);
        let want = reference_frames(&g, 6, 5, &frames);
        for (k, f) in idx.frames().iter().enumerate() {
            assert_eq!(
                visible(crate::imaging::convert::to_rgba8_rows(&f.px).unwrap()),
                visible(want[k].clone()),
                "{k}"
            );
        }
    }

    /// 完全に透明な画素の RGB を 0 にそろえる（見えず、後の合成にも効かない）。
    fn visible(mut v: Vec<u8>) -> Vec<u8> {
        for p in v.chunks_exact_mut(4) {
            if p[3] == 0 {
                p.fill(0);
            }
        }
        v
    }

    /// APNG の参照の合成：各フレームを 1 フレームの APNG として読んで RGBA に広げ、仕様どおりに RGBA で重ねる。
    fn reference_frames(
        g: &crate::testfiles::FileGen,
        w: u32,
        h: u32,
        frames: &[crate::testfiles::ApngFrame],
    ) -> Vec<Vec<u8>> {
        use crate::testfiles::FileGen;
        let FileGen::Apng {
            color,
            palette_len,
            trns,
            seed,
            ..
        } = g.clone()
        else {
            panic!()
        };
        let sub = |k: usize| -> Vec<u8> {
            let f = &frames[k];
            let mut fs: Vec<crate::testfiles::ApngFrame> =
                (0..=k).map(|_| frame(0, 0, 1, 1, 0, 0)).collect();
            fs[k] = frame(0, 0, f.w, f.h, 0, 0);
            let one = FileGen::Apng {
                w: f.w,
                h: f.h,
                color,
                palette_len,
                trns: trns.clone(),
                separate_default: true,
                plays: 0,
                frames: fs,
                exif: None,
                seed,
                interlace: false,
            };
            let Value::Frames(x) = decode_with(&PngCodec, one.bytes().unwrap()).unwrap() else {
                panic!()
            };
            crate::imaging::convert::to_rgba8_rows(&x.frames()[k].px).unwrap()
        };
        let (cw, ch) = (w as usize, h as usize);
        let mut canvas = vec![0u8; cw * ch * 4];
        let mut out = Vec::new();
        for (k, f) in frames.iter().enumerate() {
            let data = sub(k);
            let before = canvas.clone();
            for y in 0..f.h as usize {
                for x in 0..f.w as usize {
                    let s = &data[(y * f.w as usize + x) * 4..][..4];
                    let at = ((f.y as usize + y) * cw + f.x as usize + x) * 4;
                    if f.blend == 0 {
                        canvas[at..at + 4].copy_from_slice(s);
                    } else {
                        composite::over(PixelFormat::Rgba8, &mut canvas[at..at + 4], s).unwrap();
                    }
                }
            }
            out.push(canvas.clone());
            for y in 0..f.h as usize {
                for x in 0..f.w as usize {
                    let at = ((f.y as usize + y) * cw + f.x as usize + x) * 4;
                    match f.dispose {
                        1 => canvas[at..at + 4].fill(0),
                        2 => canvas[at..at + 4].copy_from_slice(&before[at..at + 4]),
                        _ => {}
                    }
                }
            }
        }
        let _ = ch;
        out
    }

    /// インターレースの APNG（部分フレームを含む）は、インターレースでない同じ絵と同じ値になる。
    #[test]
    fn interlaced_apng_matches_the_progressive_one() {
        use crate::testfiles::FileGen;
        for color in [2u8, 3, 6] {
            let make = |interlace| FileGen::Apng {
                w: 13,
                h: 11,
                color,
                palette_len: 12,
                trns: (color == 3).then(|| vec![255, 0]),
                separate_default: false,
                plays: 1,
                frames: vec![
                    frame(0, 0, 13, 11, 1, 0),
                    frame(2, 1, 5, 4, 0, 1),
                    frame(3, 3, 9, 7, 2, 0),
                ],
                exif: None,
                seed: 6,
                interlace,
            };
            assert_eq!(
                decode_with(&PngCodec, make(true).bytes().unwrap()).unwrap(),
                decode_with(&PngCodec, make(false).bytes().unwrap()).unwrap(),
                "{color}"
            );
        }
    }

    /// tRNS は png クレートと同じ読み方：短いもの・PLTE より前のもの・IDAT より後ろのものは無視する。形と中身は一致し、旧版と同じ値。
    #[test]
    fn odd_trns_chunks_follow_the_png_crate() {
        use crate::testfiles::{FileGen, crc32};
        let insert = |mut bytes: Vec<u8>, after: &[u8; 4], body: &[u8]| -> Vec<u8> {
            let at = bytes.windows(4).position(|w| w == after).unwrap() - 4;
            let len = u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
                as usize;
            let end = at + 12 + len;
            let mut chunk = (body.len() as u32).to_be_bytes().to_vec();
            chunk.extend_from_slice(b"tRNS");
            chunk.extend_from_slice(body);
            let crc = crc32(&chunk[4..]);
            chunk.extend_from_slice(&crc.to_be_bytes());
            bytes.splice(end..end, chunk);
            bytes
        };
        let rgb = FileGen::Png {
            w: 4,
            h: 2,
            color: 2,
            depth: 8,
            interlace: false,
            palette_len: 0,
            trns: None,
            text: vec![],
            icc: None,
            phys: None,
            seed: 1,
            exif: None,
        }
        .bytes()
        .unwrap();
        for bytes in [
            insert(rgb.clone(), b"IHDR", &[0, 1]),
            insert(rgb.clone(), b"IDAT", &[0, 1, 0, 2, 0, 3]),
        ] {
            let now = decode_with(&PngCodec, bytes.clone()).unwrap();
            let old = decode_with(&PngDecoderV1, bytes).unwrap();
            assert_eq!(now, old);
            assert!(matches!(&now, Value::Image(i) if i.px.format() == PixelFormat::Rgb8));
        }
        // パレットの APNG：PLTE より前の tRNS は無視されるので、透明な色はない → 消すなら Rgba8
        let pal = FileGen::Apng {
            w: 10,
            h: 8,
            color: 3,
            palette_len: 8,
            trns: None,
            separate_default: false,
            plays: 0,
            frames: vec![frame(0, 0, 10, 8, 1, 0), frame(2, 1, 5, 4, 0, 0)],
            exif: None,
            seed: 3,
            interlace: false,
        }
        .bytes()
        .unwrap();
        let v = decode_with(&PngCodec, insert(pal, b"IHDR", &[0])).unwrap();
        let Value::Frames(f) = v else { panic!() };
        assert_eq!(f.frames()[0].px.format(), PixelFormat::Rgba8);
        assert_eq!(f.frames()[1].px.pixel(0, 0), Some(&[0u8, 0, 0, 0][..]));
    }

    /// 既定の画像がアニメの外ならフレームに数えない。既定の画像だけの APNG は 1 枚の画像。壊れた fcTL はエラー。
    #[test]
    fn apng_default_image_and_broken_control_chunks() {
        use crate::testfiles::FileGen;
        let base = |separate: bool, frames: Vec<crate::testfiles::ApngFrame>| FileGen::Apng {
            w: 4,
            h: 4,
            color: 2,
            palette_len: 0,
            trns: None,
            separate_default: separate,
            plays: 0,
            frames,
            exif: None,
            seed: 1,
            interlace: false,
        };
        let v = decode_with(
            &PngCodec,
            base(true, vec![frame(0, 0, 4, 4, 0, 0)]).bytes().unwrap(),
        )
        .unwrap();
        assert!(matches!(&v, Value::Frames(f) if f.frames().len() == 1));
        // 既定の画像だけの APNG も、acTL があればフレームの列（表示時間と繰り返しを失わない）
        let v = decode_with(
            &PngCodec,
            base(false, vec![frame(0, 0, 4, 4, 0, 0)]).bytes().unwrap(),
        )
        .unwrap();
        assert!(matches!(&v, Value::Frames(f) if f.frames().len() == 1));
        // 旧版は既定の画像だけを読む
        let two = base(true, vec![frame(0, 0, 4, 4, 0, 0), frame(1, 1, 2, 2, 0, 0)]);
        assert!(matches!(
            decode_with(&PngDecoderV1, two.bytes().unwrap()).unwrap(),
            Value::Image(_)
        ));
        // キャンバスの外の fcTL、acTL と fcTL の数の食い違いはエラー
        let outside = base(true, vec![frame(3, 3, 2, 2, 0, 0)]);
        assert!(decode_with(&PngCodec, outside.bytes().unwrap()).is_err());
        let mut bytes = two.bytes().unwrap();
        let at = bytes.windows(4).position(|w| w == b"acTL").unwrap();
        bytes[at + 7] = 5;
        let crc = crate::testfiles::crc32(&bytes[at..at + 12]);
        bytes[at + 12..at + 16].copy_from_slice(&crc.to_be_bytes());
        assert!(decode_with(&PngCodec, bytes).is_err());
    }

    /// eXIf の向きを画素に当て、EXIF の向きは 1 にして運ぶ。形も向きを当てた大きさ。旧版は eXIf を読まない。
    #[test]
    fn exif_orientation_is_applied() {
        use crate::testfiles::FileGen;
        let make = |exif: Option<Vec<u8>>| FileGen::Png {
            w: 5,
            h: 3,
            color: 2,
            depth: 8,
            interlace: false,
            palette_len: 0,
            trns: None,
            text: vec![],
            icc: None,
            phys: Some((3_780, 2_835, 1)),
            seed: 3,
            exif,
        };
        let plain = decode_with(&PngCodec, make(None).bytes().unwrap()).unwrap();
        let Value::Image(plain) = plain else { panic!() };
        let turned = decode_with(
            &PngCodec,
            make(Some(crate::codec::exif::minimal(6, true)))
                .bytes()
                .unwrap(),
        )
        .unwrap();
        let Value::Image(turned) = turned else {
            panic!()
        };
        assert_eq!((turned.px.width(), turned.px.height()), (3, 5));
        assert_eq!(turned.px, orient::orient(&plain.px, 6, &NoMonitor).unwrap());
        assert_eq!(exif::orientation(turned.meta.exif.as_deref().unwrap()), 1);
        let old = decode_with(
            &PngDecoderV1,
            make(Some(crate::codec::exif::minimal(6, true)))
                .bytes()
                .unwrap(),
        )
        .unwrap();
        let Value::Image(old) = old else { panic!() };
        assert_eq!(old.px, plain.px);
        assert!(old.meta.exif.is_none());
    }

    /// フレームの列を APNG に書いて読むと、同じ値に戻る（形式・パレット・表示時間・繰り返し・EXIF）。
    #[test]
    fn frames_round_trip_through_apng() {
        for fmt in [
            PixelFormat::Rgba8,
            PixelFormat::Indexed8,
            PixelFormat::Rgba16,
            PixelFormat::Gray8,
        ] {
            let Value::Frames(fr) = (InputGen::NoiseFrames {
                w: 9,
                h: 7,
                fmt,
                count: 3,
                seed: 2,
            })
            .generate()
            .unwrap() else {
                panic!()
            };
            let mut fr = Arc::unwrap_or_clone(fr);
            fr.loop_count = 2;
            fr.meta.exif = Some(Arc::new(crate::codec::exif::minimal(1, false)));
            let durations = [10u32, 65_535, 1_234_567];
            let frames: Vec<(Raster, u32)> = fr
                .frames()
                .iter()
                .zip(durations)
                .map(|(f, d)| (f.px.clone(), d))
                .collect();
            let fr = Frames::full(frames, fr.meta.clone(), fr.loop_count).unwrap();
            let back = round_trip(&Value::Frames(Arc::new(fr.clone())), json!({}));
            let Value::Frames(back) = back else { panic!() };
            assert_eq!(back.loop_count, 2);
            assert_eq!(back.meta.exif, fr.meta.exif);
            let ms: Vec<u32> = back.frames().iter().map(|f| f.duration_ms).collect();
            // 65,535 ms までは同じ、それより長いと丸める
            assert_eq!(ms, vec![10, 65_535, 1_235_000], "{fmt:?}");
            for (a, b) in back.frames().iter().zip(fr.frames()) {
                assert_eq!(a.px, b.px, "{fmt:?}");
            }
        }
        assert_eq!(delay_fraction(0), (0, 1000));
        assert_eq!(delay_fraction(65_536), (6_554, 100));
        assert_eq!(delay_fraction(u32::MAX), (u16::MAX, 1));
    }
}
