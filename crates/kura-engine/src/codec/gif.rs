//! GIF（Pinned）。読み取りは自作の読み口 `gif_read`、書き出しは自作の入れ物 `gif_write` と LZW `lzw`（M16 段 8。ADR-225。
//! 前は `gif` クレート 0.14.2。前の読み書きは試験だけの `gif_old` に写して照らす）。
//!
//! 読み取り（`gif-rs-dec/1`。前の gif クレートが読めた GIF は同じ値で読む。ブロックの層の受け付けは `gif_read` の冒頭）：
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
//! - 途中で切れた GIF（終わりのブロックがない・フレームのデータが途中で終わる）はブラウザのように読み、届いた画素まで描く
//!   （利用者の決定。前は誤り。最初のフレームが切れていれば、届かなかった画素は透明に見えるので消す色を持つ形式にする）。
//!   ファイルは終わりまであるのに画素が足りないフレームは前と同じく誤り。画素は 1 行ずつ展開してキャンバスに描く。インターレースのフレームは素直な LZW で読む（前の gif クレートは、行が符号の切れ目で終わると
//!   正しいファイルを断ったり、まれにずれた値で読んだりしていた。利用者の決定で、その稀な値は正しい値に変わる）。
//!
//! 書き出し（`gif-kura-enc/1`。前の `gif-rs-enc/1` は退いた書き手。入れ物の書き方は前と同じ意味で、LZW は自作。表が満ちたら
//! すぐ clear（利用者の決定））：
//! - 設定は `alphaThreshold`（0〜255、既定 128）：アルファがこれより小さい画素を透明、ほかを不透明にする（GIF の透明は 2 値）。
//! - 色（不透明な画素の RGB）が、透明の分を除いた空きに収まればそのままのパレット、収まらなければ `imaging::quantize`（自作の
//!   メディアンカット）で減色し、いちばん近い色にする（ディザはかけない）。フレームの列は全フレームの色で 1 つの大域パレットを作る。
//! - フレームはキャンバスいっぱいで書く（透明がある列は dispose Background、ない列は Keep）。表示時間は 10 ms 単位に四捨五入。
//!   フレームの列は NETSCAPE の拡張で繰り返しを書く（1 回なら書かない）。1 フレームで 1 回の列は、読むと 1 枚の画像に戻ってしまうので書かない
//!   （`Unsupported`。先に 1 枚の画像にする）。
//! - 16 ビットの画像は書けない（先に 8 ビットへ変換する）。トリムしたフレームの列も書かない（先に広げる）。

use std::io::Write;
use std::sync::Arc;

use serde_json::json;

use crate::codec::gif_read::{self, Dispose, Repeat, Scan};
use crate::codec::gif_write::{FrameOut, GifOut};
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
const ENC_ID: &str = "gif-kura-enc/1";

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

struct Plan {
    scan: Scan,
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
    let scan = gif_read::scan(src, limits)?;
    let (w, h) = (scan.w, scan.h);
    if w == 0 || h == 0 {
        return Err(err("論理画面の大きさが 0 です"));
    }
    let Some(first) = scan.frames.first() else {
        return Err(err("フレームがありません"));
    };
    let plays = match scan.repeat {
        Repeat::None => 1,
        Repeat::Infinite => 0,
        Repeat::Finite(n) => u32::from(n) + 1,
    };
    let animated = scan.frames.len() > 1 || scan.repeat != Repeat::None;
    // 途中で切れた最初のフレームは、届かなかった画素が透明のまま見える
    let need_clear = !(first.complete
        && first.left == 0
        && first.top == 0
        && first.w >= w
        && first.h >= h
        && first.transparent.is_none()
        && scan
            .frames
            .iter()
            .all(|f| matches!(f.dispose, Dispose::Any | Dispose::Keep)));
    let indexed_palette = match &scan.global {
        Some(g) if !g.is_empty() && !scan.frames.iter().any(|f| f.local.is_some()) => {
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
        scan,
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

    /// 値のほかに、キャンバス（1 フレーム）と Previous で戻すための写し（1 フレーム）、1 行の置き場と LZW の表。フレーム
    /// 全体の置き場は持たない（1 行ずつ展開してキャンバスに描く。M16 段 8）。
    fn work_bytes(&self, _format: &str, shape: &Shape) -> u64 {
        let frame = match *shape {
            Shape::Image { w, h, fmt } | Shape::Frames { w, h, fmt, .. } => w
                .max()
                .saturating_mul(h.max())
                .saturating_mul(fmt.bytes_per_pixel() as u64),
            Shape::Audio { .. } => 0,
        };
        frame.saturating_mul(2).saturating_add(256 * 1024)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let p = plan(src, limits)?;
        let (w, h) = (
            Dim::Exact(u64::from(p.scan.w)),
            Dim::Exact(u64::from(p.scan.h)),
        );
        Ok(if !p.animated {
            Shape::Image { w, h, fmt: p.fmt }
        } else {
            Shape::Frames {
                count: Dim::Exact(p.scan.frames.len() as u64),
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
        let global: Vec<[u8; 4]> = p.scan.global.clone().unwrap_or_default();
        let mut meta = ImageMeta::default();
        if let Some(icc) = &p.scan.icc {
            meta.icc = Some(Arc::new(icc.clone()));
        }
        let (cw, ch) = (p.scan.w as usize, p.scan.h as usize);
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
        for info in &p.scan.frames {
            if u64::from(info.w) * u64::from(info.h) > limits.max_pixels {
                return Err(EngineError::Limit(format!(
                    "フレームの画素が上限 {} を超えます",
                    limits.max_pixels
                )));
            }
            // 局所パレットがあるのは Rgba8 のときだけ（計画でそう決めてある）
            let colors: &[[u8; 4]] = match &info.local {
                Some(local) if !indexed => local,
                _ => &global,
            };
            // 画面の中に切った範囲
            let x1 = (info.left + info.w).min(p.scan.w);
            let y1 = (info.top + info.h).min(p.scan.h);
            let saved = (info.dispose == Dispose::Previous).then(|| canvas.clone());
            // 1 行ずつ展開して描く（届いた画素だけ。途中で切れたフレームは最後の行が短い）
            gif_read::decode_frame(src, info, |fy, idx| {
                let y = info.top as usize + fy;
                if y >= y1 as usize {
                    return Ok(());
                }
                for (dx, &i) in idx.iter().enumerate() {
                    let x = info.left as usize + dx;
                    if x >= x1 as usize {
                        break;
                    }
                    if Some(i) == info.transparent {
                        continue;
                    }
                    if usize::from(i) >= colors.len() {
                        return Err(err("パレットにない番号の画素があります"));
                    }
                    let at = (y * cw + x) * bpp;
                    if indexed {
                        canvas[at] = i;
                    } else {
                        canvas[at..at + 4].copy_from_slice(&colors[usize::from(i)]);
                    }
                }
                Ok(())
            })?;
            let ms = u32::from(info.delay) * 10;
            out.push((
                Raster::from_rows(p.scan.w, p.scan.h, p.fmt, palette.clone(), &canvas)?,
                ms,
            ));
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            match (info.dispose, saved) {
                (Dispose::Previous, Some(prev)) => canvas = prev,
                (Dispose::Background, _) => {
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
        // 繰り返し：0 は無限、n は n − 1 回の繰り返し（1 回なら書かない）
        let repeat = plays.and_then(|n| match n {
            0 => Some(0),
            1 => None,
            n => Some(u16::try_from(n - 1).unwrap_or(u16::MAX)),
        });
        let mut gif = GifOut::new(w, h, &rgb, repeat)?;
        let dispose = if any_transparent { 2 } else { 1 };
        // 透明は透明の番号へ、ほかはいちばん近いパレットの色へ
        let opaque_palette: Vec<[u8; 4]> =
            palette.iter().copied().filter(|c| c[3] == 255).collect();
        for (rgba, (_, ms)) in rgba_frames.iter().zip(&rasters) {
            let fh = quantize::histogram(rgba, mon)?;
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
            gif.frame(&FrameOut {
                left: 0,
                top: 0,
                width: w,
                height: h,
                palette: None,
                transparent,
                delay: if plays.is_some() { delay } else { 0 },
                dispose: if plays.is_some() { dispose } else { 0 },
                interlaced: false,
                indices: &idx,
            })?;
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
        }
        gif.write_to(out)
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
