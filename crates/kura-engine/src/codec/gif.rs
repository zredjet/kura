//! GIF（`gif` クレート。版は `=` で固定。Pinned）。
//!
//! 読み取り（`gif-rs-dec/1`）：
//! - 論理画面の大きさのキャンバス（初めは透明）に、フレームを順に描く。透明の番号の画素は描かない（下が見える）。
//!   画面の外にはみ出す部分は切る。描いた直後のキャンバスを 1 フレームにし、そのあと dispose を当てる
//!   （Background はフレームの範囲を透明に、Previous は描く前に戻す。Any・Keep はそのまま）。背景色の番号は使わない（ブラウザと同じ）。
//! - フレームが 2 つ以上か、NETSCAPE の拡張（繰り返し）があればフレームの列（どれもキャンバスいっぱい）、ほかは 1 枚の画像。
//!   表示時間は 1/100 秒 × 10 ms。
//!   繰り返しは、NETSCAPE の拡張がなければ 1 回、0 なら無限（0）、n なら n + 1 回。
//! - 局所パレットがなく、大域パレットがあるときは Indexed8（パレットは大域パレット。透明が見えうるときは、
//!   後ろに透明な色 [0,0,0,0] を 1 つ足して消す色にする。大域パレットが 256 色でその余地がなければ Rgba8）。ほかは Rgba8。
//!   透明が見えうる：最初のフレームが画面いっぱいでない・透明の番号を持つ・Background か Previous のフレームがある。
//!   この判断と形は、LZW を展開しない走査で先に決める（形と中身を一致させる）。
//! - パレットの外の番号の画素はエラー。ICC（アプリケーションの拡張）を運ぶ。
//!
//! 書き出し（`gif-rs-enc/1`）：
//! - 設定は `alphaThreshold`（0〜255、既定 128）：アルファがこれより小さい画素を透明、ほかを不透明にする（GIF の透明は 2 値）。
//! - 色（不透明な画素の RGB）が、透明の分を除いた空きに収まればそのままのパレット、収まらなければ `imaging::quantize`（自作の
//!   メディアンカット）で減色し、いちばん近い色にする（ディザはかけない）。フレームの列は全フレームの色で 1 つの大域パレットを作る。
//! - フレームはキャンバスいっぱいで書く（透明がある列は dispose Background、ない列は Keep）。表示時間は 10 ms 単位に四捨五入。
//!   フレームの列は NETSCAPE の拡張で繰り返しを書く（1 回なら書かない）。1 フレームで 1 回の列は、読むと 1 枚の画像に戻ってしまうので書かない
//!   （`Unsupported`。先に 1 枚の画像にする）。
//! - 16 ビットの画像は書けない（先に 8 ビットへ変換する）。トリムしたフレームの列も書かない（先に広げる）。

use std::io::{BufReader, Write};
use std::num::NonZeroU64;
use std::sync::Arc;

use serde_json::json;

use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, caps_of, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::imaging::{convert, quantize};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Dim, Fullness, Shape};
use crate::value::{Frames, Image, ImageMeta, Palette, PixelFormat, Raster, Value};

/// GIF の読み書き。
pub struct GifCodec;

const DEC_ID: &str = "gif-rs-dec/1";
const ENC_ID: &str = "gif-rs-enc/1";

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

fn options(limits: &Limits, skip: bool) -> gif::DecodeOptions {
    let mut o = gif::DecodeOptions::new();
    o.set_color_output(gif::ColorOutput::Indexed);
    // 1 フレームの作業領域（番号は 1 画素 1 バイト）
    let bytes = NonZeroU64::new(limits.max_pixels.max(1)).unwrap_or(NonZeroU64::MIN);
    o.set_memory_limit(gif::MemoryLimit::Bytes(bytes));
    o.skip_frame_decoding(skip);
    o
}

/// フレームの記述（LZW を展開せずに読んだもの）。
#[derive(Debug, Clone, Copy)]
struct FrameInfo {
    left: u32,
    top: u32,
    w: u32,
    h: u32,
    local: bool,
    transparent: Option<u8>,
    dispose: gif::DisposalMethod,
}

struct Plan {
    w: u32,
    h: u32,
    frames: Vec<FrameInfo>,
    /// フレームの列にするか（2 つ以上か、繰り返しの拡張がある）。
    animated: bool,
    fmt: PixelFormat,
    /// Indexed8 のときのパレット（大域パレット＋必要なら透明な色）。
    palette: Option<Vec<[u8; 4]>>,
    /// 消す色（Indexed8 なら番号、Rgba8 なら [0,0,0,0]）。
    clear_index: u8,
    plays: u32,
}

fn plan(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Plan, EngineError> {
    let mut d = options(limits, true)
        .read_info(BufReader::new(src))
        .map_err(err)?;
    let (w, h) = (u32::from(d.width()), u32::from(d.height()));
    if w == 0 || h == 0 {
        return Err(err("論理画面の大きさが 0 です"));
    }
    let global: Option<Vec<[u8; 4]>> = d.global_palette().map(|p| {
        p.as_chunks::<3>()
            .0
            .iter()
            .map(|c| [c[0], c[1], c[2], 255])
            .collect()
    });
    let mut frames = Vec::new();
    while let Some(f) = d.read_next_frame().map_err(err)? {
        if frames.len() as u64 >= limits.max_frames {
            return Err(EngineError::Limit(format!(
                "フレームが {} を超えます",
                limits.max_frames
            )));
        }
        frames.push(FrameInfo {
            left: u32::from(f.left),
            top: u32::from(f.top),
            w: u32::from(f.width),
            h: u32::from(f.height),
            local: f.palette.is_some(),
            transparent: f.transparent,
            dispose: f.dispose,
        });
    }
    let Some(first) = frames.first().copied() else {
        return Err(err("フレームがありません"));
    };
    let plays = match d.repeat() {
        gif::Repeat::Infinite => 0,
        gif::Repeat::Finite(n) => u32::from(n) + 1,
    };
    // 拡張がないときは Finite(0)（1 回）。拡張の 0 は無限、n は Finite(n)
    let animated = frames.len() > 1 || d.repeat() != gif::Repeat::Finite(0);
    let need_clear = !(first.left == 0
        && first.top == 0
        && first.w >= w
        && first.h >= h
        && first.transparent.is_none()
        && frames.iter().all(|f| {
            matches!(
                f.dispose,
                gif::DisposalMethod::Any | gif::DisposalMethod::Keep
            )
        }));
    let indexed_palette = match &global {
        Some(g) if !g.is_empty() && !frames.iter().any(|f| f.local) => {
            if !need_clear {
                Some((g.clone(), 0u8))
            } else if g.len() < 256 {
                let mut p = g.clone();
                p.push([0, 0, 0, 0]);
                Some((p, g.len() as u8))
            } else {
                None
            }
        }
        _ => None,
    };
    let (fmt, palette, clear_index) = match indexed_palette {
        Some((p, c)) => (PixelFormat::Indexed8, Some(p), c),
        None => (PixelFormat::Rgba8, None, 0),
    };
    Ok(Plan {
        w,
        h,
        frames,
        animated,
        fmt,
        palette,
        clear_index,
        plays,
    })
}

impl Decoder for GifCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "gif"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let p = plan(src, limits)?;
        let (w, h) = (Dim::Exact(u64::from(p.w)), Dim::Exact(u64::from(p.h)));
        Ok(if !p.animated {
            Shape::Image { w, h, fmt: p.fmt }
        } else {
            Shape::Frames {
                count: Dim::Exact(p.frames.len() as u64),
                w,
                h,
                fmt: p.fmt,
                full: Fullness::Full,
            }
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
        let p = plan(src, limits)?;
        src.rewind()?;
        let mut d = options(limits, false)
            .read_info(BufReader::new(src))
            .map_err(err)?;
        let global: Vec<[u8; 4]> = d
            .global_palette()
            .map(|g| {
                g.as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2], 255])
                    .collect()
            })
            .unwrap_or_default();
        let mut meta = ImageMeta::default();
        if let Some(icc) = d.icc_profile() {
            if icc.len() as u64 > limits.max_meta_bytes {
                return Err(EngineError::Limit("ICC プロファイルが大きすぎます".into()));
            }
            meta.icc = Some(Arc::new(icc.to_vec()));
        }
        let (cw, ch) = (p.w as usize, p.h as usize);
        let indexed = p.fmt == PixelFormat::Indexed8;
        let bpp = p.fmt.bytes_per_pixel();
        let clear: Vec<u8> = if indexed {
            vec![p.clear_index]
        } else {
            vec![0, 0, 0, 0]
        };
        let mut canvas = try_vec(cw * ch * bpp, 0u8)?;
        if indexed {
            canvas.fill(p.clear_index);
        }
        let palette = p.palette.clone().map(|colors| Palette { colors });
        let mut out: Vec<(Raster, u32)> = Vec::new();
        let mut k = 0usize;
        while let Some(f) = d.read_next_frame().map_err(err)? {
            let info = *p
                .frames
                .get(k)
                .ok_or_else(|| err("フレームの数が合いません"))?;
            k += 1;
            if (u32::from(f.width), u32::from(f.height)) != (info.w, info.h) {
                return Err(err("フレームの記述が合いません"));
            }
            // 局所パレットがあるのは Rgba8 のときだけ（計画でそう決めてある）
            let colors: Vec<[u8; 4]> = match &f.palette {
                Some(local) if !indexed => local
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2], 255])
                    .collect(),
                _ => global.clone(),
            };
            // 画面の中に切った範囲
            let x1 = (info.left + info.w).min(p.w);
            let y1 = (info.top + info.h).min(p.h);
            let saved = (info.dispose == gif::DisposalMethod::Previous).then(|| canvas.clone());
            for y in info.top..y1 {
                for x in info.left..x1 {
                    let idx = *f
                        .buffer
                        .get(((y - info.top) * info.w + (x - info.left)) as usize)
                        .ok_or_else(|| err("フレームの画素が足りません"))?;
                    if Some(idx) == f.transparent {
                        continue;
                    }
                    if usize::from(idx) >= colors.len() {
                        return Err(err("パレットにない番号の画素があります"));
                    }
                    let at = (y as usize * cw + x as usize) * bpp;
                    if indexed {
                        canvas[at] = idx;
                    } else {
                        canvas[at..at + 4].copy_from_slice(&colors[usize::from(idx)]);
                    }
                }
            }
            let ms = u32::from(f.delay) * 10;
            out.push((
                Raster::from_rows(p.w, p.h, p.fmt, palette.clone(), &canvas)?,
                ms,
            ));
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            match (info.dispose, saved) {
                (gif::DisposalMethod::Previous, Some(prev)) => canvas = prev,
                (gif::DisposalMethod::Background, _) => {
                    for y in info.top..y1 {
                        for x in info.left..x1 {
                            let at = (y as usize * cw + x as usize) * bpp;
                            canvas[at..at + bpp].copy_from_slice(&clear);
                        }
                    }
                }
                _ => {}
            }
        }
        if k != p.frames.len() {
            return Err(err("フレームの数が合いません"));
        }
        if !p.animated {
            let (px, _) = out.remove(0);
            return Ok(Value::Image(Arc::new(Image { px, meta })));
        }
        Ok(Value::Frames(Arc::new(Frames::full(out, meta, p.plays)?)))
    }
}

impl Encoder for GifCodec {
    fn id(&self) -> &'static str {
        ENC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        let eight = |fmt: &PixelFormat| {
            matches!(
                fmt,
                PixelFormat::Gray8
                    | PixelFormat::GrayA8
                    | PixelFormat::Rgb8
                    | PixelFormat::Rgba8
                    | PixelFormat::Indexed8
            )
        };
        let fits =
            |w: &Dim, h: &Dim| w.max() <= u64::from(u16::MAX) && h.max() <= u64::from(u16::MAX);
        format == "gif"
            && match shape {
                Shape::Image { w, h, fmt } => eight(fmt) && fits(w, h),
                Shape::Frames {
                    w, h, fmt, full, ..
                } => eight(fmt) && fits(w, h) && *full == Fullness::Full,
                Shape::Audio { .. } => false,
            }
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "alphaThreshold",
                ParamKind::Int {
                    min: 0,
                    max: 255,
                    step: 1,
                },
                Unit::None,
                json!(128),
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
        let threshold = opts
            .get("alphaThreshold")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(128)
            .min(255) as u8;
        let (rasters, plays): (Vec<(&Raster, u32)>, Option<u32>) = match value {
            Value::Image(img) => (vec![(&img.px, 0)], None),
            Value::Frames(fr) if fr.is_full() => (
                fr.frames().iter().map(|f| (&f.px, f.duration_ms)).collect(),
                Some(fr.loop_count),
            ),
            _ => {
                return Err(EngineError::Unsupported(
                    "GIF には画像か、キャンバスいっぱいのフレームの列だけを書けます".into(),
                ));
            }
        };
        let Some((first, _)) = rasters.first().copied() else {
            return Err(EngineError::Unsupported("フレームがありません".into()));
        };
        if plays == Some(1) && rasters.len() == 1 {
            return Err(EngineError::Unsupported(
                "1 フレームで 1 回のフレームの列は GIF に書けません（読むと 1 枚の画像になります）。先に 1 枚の画像にしてください".into(),
            ));
        }
        let (w, h) = (
            u16::try_from(first.width()).map_err(|_| enc_err("GIF の幅は 65,535 まで"))?,
            u16::try_from(first.height()).map_err(|_| enc_err("GIF の高さは 65,535 まで"))?,
        );
        // 各フレームを RGBA8 に広げ、透明（アルファがしきい値より小さい）を [0,0,0,0]、ほかを不透明にそろえる
        let mut rgba_frames: Vec<Vec<u8>> = Vec::with_capacity(rasters.len());
        let mut any_transparent = false;
        for (px, _) in &rasters {
            let mut rgba = convert::to_rgba8_rows(px)?;
            for p in rgba.as_chunks_mut::<4>().0 {
                if p[3] < threshold {
                    p.copy_from_slice(&[0, 0, 0, 0]);
                    any_transparent = true;
                } else {
                    p[3] = 255;
                }
            }
            rgba_frames.push(rgba);
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
        }
        // 不透明な色だけでパレットを作る（透明は別の番号）
        let opaque: Vec<u8> = rgba_frames
            .iter()
            .flat_map(|f| {
                f.as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p[3] == 255)
                    .flatten()
                    .copied()
            })
            .collect();
        let hist = quantize::histogram(&opaque, mon)?;
        drop(opaque);
        let room = if any_transparent { 255 } else { 256 };
        let mut palette = if hist.is_empty() {
            Vec::new()
        } else {
            quantize::palette_for(&hist, room, mon)?
        };
        let transparent = any_transparent.then_some(palette.len() as u8);
        if any_transparent {
            palette.push([0, 0, 0, 0]);
        }
        if palette.is_empty() {
            palette.push([0, 0, 0, 255]);
        }
        let rgb: Vec<u8> = palette.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
        let mut bytes = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut bytes, w, h, &rgb).map_err(enc_err)?;
            if let Some(plays) = plays {
                let repeat = match plays {
                    0 => gif::Repeat::Infinite,
                    n => gif::Repeat::Finite(u16::try_from(n - 1).unwrap_or(u16::MAX)),
                };
                enc.set_repeat(repeat).map_err(enc_err)?;
            }
            let dispose = if any_transparent {
                gif::DisposalMethod::Background
            } else {
                gif::DisposalMethod::Keep
            };
            for (rgba, (_, ms)) in rgba_frames.iter().zip(&rasters) {
                let fh = quantize::histogram(rgba, mon)?;
                // 透明は透明の番号へ、ほかはいちばん近いパレットの色へ
                let opaque_palette: Vec<[u8; 4]> =
                    palette.iter().copied().filter(|c| c[3] == 255).collect();
                let mut idx = quantize::map_to_palette(
                    rgba,
                    &fh,
                    if opaque_palette.is_empty() {
                        &palette
                    } else {
                        &opaque_palette
                    },
                    mon,
                )?;
                if let Some(t) = transparent {
                    for (i, p) in idx.iter_mut().zip(rgba.as_chunks::<4>().0.iter()) {
                        if p[3] == 0 {
                            *i = t;
                        }
                    }
                }
                let delay = u16::try_from((u64::from(*ms) + 5) / 10).unwrap_or(u16::MAX);
                let frame = gif::Frame {
                    width: w,
                    height: h,
                    buffer: std::borrow::Cow::Owned(idx),
                    transparent,
                    delay: if plays.is_some() { delay } else { 0 },
                    dispose: if plays.is_some() {
                        dispose
                    } else {
                        gif::DisposalMethod::Any
                    },
                    ..gif::Frame::default()
                };
                enc.write_frame(&frame).map_err(enc_err)?;
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
            }
        }
        out.write_all(&bytes)?;
        Ok(bytes.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::NoMonitor;
    use crate::testfiles::{FileGen, GifFrame};
    use crate::testgen::InputGen;
    use std::io::Cursor;

    fn decode(bytes: Vec<u8>) -> Value {
        let shape = GifCodec
            .shape(
                &mut Cursor::new(bytes.clone()),
                "gif",
                &DecodeOptions::default(),
                &Limits::default(),
            )
            .unwrap();
        let v = GifCodec
            .decode(
                &mut Cursor::new(bytes),
                "gif",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert!(shape.admits(&v.shape()), "{shape:?} {:?}", v.shape());
        v
    }

    #[allow(clippy::too_many_arguments)]
    fn frame(
        left: u16,
        top: u16,
        w: u16,
        h: u16,
        local_len: u16,
        transparent: Option<u8>,
        dispose: u8,
        interlaced: bool,
    ) -> GifFrame {
        GifFrame {
            left,
            top,
            w,
            h,
            local_len,
            transparent,
            dispose,
            delay: 3,
            interlaced,
        }
    }

    fn rgba_frames(v: &Value) -> Vec<Vec<u8>> {
        match v {
            Value::Image(i) => vec![convert::to_rgba8_rows(&i.px).unwrap()],
            Value::Frames(f) => f
                .frames()
                .iter()
                .map(|x| convert::to_rgba8_rows(&x.px).unwrap())
                .collect(),
            Value::Audio(_) => panic!(),
        }
    }

    /// 参照：gif クレートで各フレームを RGBA に読み、ブラウザと同じ規則で重ねる（透明は描かない・Background は透明に・Previous は戻す）。
    fn reference(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut o = gif::DecodeOptions::new();
        o.set_color_output(gif::ColorOutput::RGBA);
        let mut d = o.read_info(Cursor::new(bytes)).unwrap();
        let (cw, ch) = (usize::from(d.width()), usize::from(d.height()));
        let mut canvas = vec![0u8; cw * ch * 4];
        let mut out = Vec::new();
        while let Some(f) = d.read_next_frame().unwrap() {
            let saved = canvas.clone();
            for y in 0..usize::from(f.height) {
                for x in 0..usize::from(f.width) {
                    let (cx, cy) = (usize::from(f.left) + x, usize::from(f.top) + y);
                    if cx >= cw || cy >= ch {
                        continue;
                    }
                    let s = &f.buffer[(y * usize::from(f.width) + x) * 4..][..4];
                    if s[3] == 0 {
                        continue;
                    }
                    canvas[(cy * cw + cx) * 4..][..4].copy_from_slice(s);
                }
            }
            out.push(canvas.clone());
            match f.dispose {
                gif::DisposalMethod::Background => {
                    for y in 0..usize::from(f.height) {
                        for x in 0..usize::from(f.width) {
                            let (cx, cy) = (usize::from(f.left) + x, usize::from(f.top) + y);
                            if cx < cw && cy < ch {
                                canvas[(cy * cw + cx) * 4..][..4].fill(0);
                            }
                        }
                    }
                }
                gif::DisposalMethod::Previous => canvas = saved,
                _ => {}
            }
        }
        out
    }

    #[test]
    fn frames_are_composited_like_browsers() {
        let cases = vec![
            // 大域パレット・透明・dispose の全部・画面の外にはみ出すフレーム・インターレース
            FileGen::Gif {
                w: 12,
                h: 9,
                global_len: 16,
                repeat: Some(0),
                frames: vec![
                    frame(0, 0, 12, 9, 0, None, 1, false),
                    frame(2, 1, 5, 4, 0, Some(3), 2, true),
                    frame(6, 5, 8, 6, 0, Some(1), 3, false),
                    frame(1, 1, 3, 3, 0, None, 0, true),
                ],
                seed: 1,
            },
            // 局所パレットあり
            FileGen::Gif {
                w: 10,
                h: 7,
                global_len: 4,
                repeat: Some(2),
                frames: vec![
                    frame(0, 0, 10, 7, 8, None, 1, false),
                    frame(3, 2, 4, 4, 0, Some(0), 2, false),
                ],
                seed: 2,
            },
            // 大域パレットなし、最初のフレームが画面より小さい
            FileGen::Gif {
                w: 9,
                h: 9,
                global_len: 0,
                repeat: None,
                frames: vec![
                    frame(2, 2, 4, 4, 32, Some(5), 0, false),
                    frame(0, 0, 9, 9, 2, None, 1, false),
                ],
                seed: 3,
            },
        ];
        for g in cases {
            let bytes = g.bytes().unwrap();
            assert_eq!(
                rgba_frames(&decode(bytes.clone())),
                reference(&bytes),
                "{g:?}"
            );
        }
    }

    /// パレットのまま表せるときだけ Indexed8（消す色が要れば透明な色を後ろに足す）。繰り返しの数の読み方。
    #[test]
    fn indexed_when_exact_and_repeat_counts() {
        let fmt = |g: FileGen| -> (PixelFormat, usize, u32) {
            match decode(g.bytes().unwrap()) {
                Value::Image(i) => (
                    i.px.format(),
                    i.px.palette().map_or(0, |p| p.colors.len()),
                    u32::MAX,
                ),
                Value::Frames(f) => (
                    f.frames()[0].px.format(),
                    f.frames()[0].px.palette().map_or(0, |p| p.colors.len()),
                    f.loop_count,
                ),
                Value::Audio(_) => panic!(),
            }
        };
        let full = |dispose| frame(0, 0, 6, 6, 0, None, dispose, false);
        let g = |global_len, frames, repeat| FileGen::Gif {
            w: 6,
            h: 6,
            global_len,
            repeat,
            frames,
            seed: 4,
        };
        // 消す必要がない → 大域パレットそのまま
        assert_eq!(
            fmt(g(16, vec![full(1), full(1)], None)),
            (PixelFormat::Indexed8, 16, 1)
        );
        // 消す必要がある → 透明な色を足す
        assert_eq!(
            fmt(g(16, vec![full(1), full(2)], Some(0))),
            (PixelFormat::Indexed8, 17, 0)
        );
        // 256 色で足せない → Rgba8
        assert_eq!(
            fmt(g(256, vec![full(2), full(1)], Some(3))),
            (PixelFormat::Rgba8, 0, 4)
        );
        // 局所パレット → Rgba8
        assert_eq!(
            fmt(g(
                16,
                vec![full(1), frame(0, 0, 6, 6, 4, None, 1, false)],
                None
            ))
            .0,
            PixelFormat::Rgba8
        );
        // 1 フレームは画像
        assert_eq!(
            fmt(g(16, vec![full(0)], None)),
            (PixelFormat::Indexed8, 16, u32::MAX)
        );
    }

    /// 書いて読むと、色の数が収まる画像は色も透明も同じ。多い画像は減色して、決まったバイト列になる。
    #[test]
    fn round_trips_and_quantizes() {
        let set = CodecSet::pinned();
        for v in [
            InputGen::NoiseFrames {
                w: 11,
                h: 8,
                fmt: PixelFormat::Indexed8,
                count: 3,
                seed: 5,
            }
            .generate()
            .unwrap(),
            InputGen::Sprite {
                w: 9,
                h: 9,
                rect: crate::geom::IRect::new(2, 3, 4, 4),
            }
            .generate()
            .unwrap(),
        ] {
            let (bytes, _) = set.encode_bytes(&v, "gif", &json!({}), &NoMonitor).unwrap();
            // GIF は透明な画素の色を持てないので、透明な画素は [0,0,0,0] にそろえて比べる
            let clear = |frames: Vec<Vec<u8>>| -> Vec<Vec<u8>> {
                frames
                    .into_iter()
                    .map(|mut f| {
                        for p in f.as_chunks_mut::<4>().0.iter_mut() {
                            if p[3] == 0 {
                                p.fill(0);
                            }
                        }
                        f
                    })
                    .collect()
            };
            assert_eq!(clear(rgba_frames(&decode(bytes))), clear(rgba_frames(&v)));
        }
        let many = InputGen::Noise {
            w: 40,
            h: 30,
            fmt: PixelFormat::Rgb8,
            seed: 6,
        }
        .generate()
        .unwrap();
        let (a, _) = set
            .encode_bytes(&many, "gif", &json!({}), &NoMonitor)
            .unwrap();
        let (b, _) = set
            .encode_bytes(&many, "gif", &json!({}), &NoMonitor)
            .unwrap();
        assert_eq!(a, b);
        let back = decode(a);
        let Value::Image(img) = back else { panic!() };
        assert!(img.px.palette().is_some_and(|p| p.colors.len() <= 256));
        // しきい値：アルファ 100 は既定（128）では透明、50 にすると不透明
        let half = Value::Image(Arc::new(Image {
            px: Raster::filled(2, 2, PixelFormat::Rgba8, None, &[9, 8, 7, 100]).unwrap(),
            meta: ImageMeta::default(),
        }));
        let alpha = |opts| {
            let (b, _) = set.encode_bytes(&half, "gif", &opts, &NoMonitor).unwrap();
            rgba_frames(&decode(b))[0][3]
        };
        assert_eq!(alpha(json!({})), 0);
        assert_eq!(alpha(json!({ "alphaThreshold": 50 })), 255);
        // 16 ビットは書けない
        let deep = InputGen::Noise {
            w: 2,
            h: 2,
            fmt: PixelFormat::Rgba16,
            seed: 1,
        }
        .generate()
        .unwrap();
        assert!(!GifCodec.can_encode("gif", &deep.shape()));
    }

    /// 壊れたファイルを `n` 個作って読み、自前のコードの panic がないこと、読めたものは形と中身が一致することを確かめる。
    /// （読めた数・断った数）を返す。
    fn mutate_and_read(
        dec: &dyn Decoder,
        format: &str,
        good: &[u8],
        n: usize,
        seed: u64,
        limits: &Limits,
    ) -> (usize, usize) {
        use rand_chacha::rand_core::Rng;
        let mut r = crate::determinism::rng(seed);
        let (mut ok, mut failed) = (0, 0);
        for _ in 0..n {
            let mut b = good.to_vec();
            for _ in 0..1 + r.next_u32() % 4 {
                let i = r.next_u32() as usize % b.len();
                b[i] = r.next_u32() as u8;
            }
            let res = crate::testing::catch_own_panic(|| {
                let s = dec.shape(
                    &mut Cursor::new(b.clone()),
                    format,
                    &DecodeOptions::default(),
                    limits,
                )?;
                limits.check(&s).map_err(EngineError::Limit)?;
                let v = dec.decode(
                    &mut Cursor::new(b.clone()),
                    format,
                    &DecodeOptions::default(),
                    limits,
                    &NoMonitor,
                )?;
                if !s.admits(&v.shape()) {
                    return Err(EngineError::Internal(format!(
                        "形と中身が違う：{s:?} {:?}",
                        v.shape()
                    )));
                }
                Ok(v)
            });
            match res {
                Err(at) => panic!("自前のコードの panic：{at}"),
                Ok(Ok(_)) => ok += 1,
                Ok(Err(EngineError::Internal(m))) => panic!("{m}"),
                Ok(Err(_)) => failed += 1,
            }
        }
        (ok, failed)
    }

    /// 壊れたファイルは落ちずにエラー。自前のコード（計画・合成）の panic はテストの失敗にする。
    #[test]
    fn broken_files_are_errors() {
        let good = FileGen::Gif {
            w: 20,
            h: 16,
            global_len: 8,
            repeat: Some(0),
            frames: vec![
                frame(0, 0, 20, 16, 0, Some(2), 2, false),
                frame(4, 4, 30, 30, 16, None, 3, true),
            ],
            seed: 9,
        }
        .bytes()
        .unwrap();
        let tight = Limits {
            max_pixels: 1 << 14,
            max_frames: 8,
            ..Limits::default()
        };
        let (ok, failed) = mutate_and_read(&GifCodec, "gif", &good, 400, 3, &tight);
        assert!(ok > 10 && failed > 10, "ok {ok} failed {failed}");
    }

    /// 1 フレームでも繰り返しの拡張があればフレームの列。1 フレームで 1 回の列は書かない（読むと画像に戻るので）。
    #[test]
    fn single_frame_animations() {
        let set = CodecSet::pinned();
        let one = |plays: u32| {
            let Value::Frames(f) = (InputGen::NoiseFrames {
                w: 5,
                h: 4,
                fmt: PixelFormat::Indexed8,
                count: 1,
                seed: 3,
            })
            .generate()
            .unwrap() else {
                panic!()
            };
            let mut f = Arc::unwrap_or_clone(f);
            f.loop_count = plays;
            Value::Frames(Arc::new(f))
        };
        for plays in [0u32, 3] {
            let (bytes, _) = set
                .encode_bytes(&one(plays), "gif", &json!({}), &NoMonitor)
                .unwrap();
            let Value::Frames(back) = decode(bytes) else {
                panic!("{plays}")
            };
            assert_eq!((back.frames().len(), back.loop_count), (1, plays));
        }
        assert!(matches!(
            set.encode_bytes(&one(1), "gif", &json!({}), &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
    }
}
