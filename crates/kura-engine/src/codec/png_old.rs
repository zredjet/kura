//! 前の PNG の読み手と書き手（png クレート 0.18.1。M16 段 6・7 で自作の `png_read`・`png_write` に替える前の `png.rs` の
//! 読み取りと書き出しをそのまま写したもの）。試験だけで使う照合の相手（`png-rs-dec/1`・`/2` の識別子のまま読み手を替えた
//! ので、前に読めた PNG を同じ値で読むかを照らす。書き手は大きさ・速さを比べ、同じ値に読めるかを照らす）。png クレートは
//! 段 7 から dev 依存。

#![allow(dead_code)]

use std::borrow::Cow;
use std::io::{BufReader, Read, Write};
use std::sync::Arc;

use crate::codec::{Decoder, ReadSeek, Tier, exif, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::imaging::{composite, convert, orient};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Fullness, Shape};
use crate::value::{Frames, Image, ImageMeta, Palette, PixelFormat, Raster, Value};

/// PNG の読み書き（読み取りは今の版）。
pub struct OldPng;

/// PNG の読み取りの旧版（`png-rs-dec/1`）。作り直しで記録の識別子から選ぶときだけ使う。
pub struct OldPngV1;

/// 読み取りの約束の版（読む値が変わる変更は上げ、旧版を残す。§9.1）。
const DEC_ID: &str = "png-rs-dec/2";
const DEC_ID_V1: &str = "png-rs-dec/1";

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
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

impl Decoder for OldPng {
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
                    .as_chunks::<3>()
                    .0
                    .iter()
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

impl Decoder for OldPngV1 {
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

/// 前の PNG の書き手（png クレート 0.18.1。M16 段 7 で自作の `png_write` に替える前の `png.rs` の書き出しをそのまま写したもの）。
/// 試験だけで使う照合の相手（大きさ・速さを比べ、読み直した値が同じかを照らす）。
pub struct OldPngEnc;

impl OldPngEnc {
    /// 前の書き出し（`compression` は fast・balanced・high）。
    pub fn encode(
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
                for pair in rows.as_chunks_mut::<2>().0 {
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

/// 表示時間（ms）を fcTL の分数（秒）にする（前の書き手と同じ）。
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

fn enc_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Encode {
        codec: "png-rs-enc/1".to_string(),
        message: e.to_string(),
    }
}
