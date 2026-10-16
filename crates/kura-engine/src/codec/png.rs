//! PNG（`png` クレート。版は `=` で固定。Pinned）。
//!
//! - 読む：8/16 ビット、グレー・グレー＋アルファ・RGB・RGBA・パレット（1/2/4/8 ビット）。
//!   1/2/4 ビットのグレーは 8 ビットへ伸ばし（255/(2^n−1) 倍）、パレットは番号のまま Indexed8 にする。
//!   グレー・RGB の tRNS（透明にする 1 色）は、アルファ付きの形式にして、その色の画素のアルファを 0 にする。
//!   ICC・解像度（pHYs）・テキスト（tEXt と圧縮していない iTXt）を運ぶ。圧縮したテキストは展開爆弾を避けて読まない。
//!   APNG はいまは既定の画像だけ（フレームは段 4）。
//! - 書く：値の形式のまま（16 ビットはビッグエンディアンに戻す）。Indexed8 は PLTE と tRNS。時刻（tIME）は書かない。

use std::borrow::Cow;
use std::io::{BufReader, Write};
use std::sync::Arc;

use serde_json::json;

use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, format_by_id, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Dim, Shape};
use crate::value::{Image, ImageMeta, Palette, PixelFormat, Raster, Value};

/// PNG の読み書き。
pub struct PngCodec;

const ID: &str = "png-rs/1";

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Decode {
        codec: ID.to_string(),
        message: e.to_string(),
    }
}

fn enc_err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Encode {
        codec: ID.to_string(),
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

impl Decoder for PngCodec {
    fn id(&self) -> &'static str {
        ID
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
        let (w, h, color, depth) = {
            let info = r.info();
            (info.width, info.height, info.color_type, info.bit_depth)
        };
        let trns: Option<Vec<u8>> = r.info().trns.as_ref().map(|t| t.to_vec());
        let palette_rgb: Option<Vec<u8>> = r.info().palette.as_ref().map(|p| p.to_vec());
        let meta = read_meta(r.info());
        let size = r
            .output_buffer_size()
            .ok_or_else(|| err("画像が大きすぎます"))?;
        let mut buf = try_vec(size, 0u8)?;
        let out = r.next_frame(&mut buf).map_err(err)?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let line = out.line_size;
        let indexed_or_gray_trns = color != png::ColorType::Indexed && trns.is_some();
        let fmt = target_format(color, depth, indexed_or_gray_trns)?;
        let bits = depth as u8;
        let channels = color.samples();
        let bpp = fmt.bytes_per_pixel();
        let mut data = try_vec(w as usize * h as usize * bpp, 0u8)?;
        for y in 0..h as usize {
            let row = buf
                .get(y * line..(y + 1) * line)
                .ok_or_else(|| err("行が足りません"))?;
            let dst = &mut data[y * w as usize * bpp..(y + 1) * w as usize * bpp];
            unpack_row(row, dst, w as usize, color, bits, channels, trns.as_deref())?;
        }
        let palette = match (color, palette_rgb) {
            (png::ColorType::Indexed, Some(rgb)) => {
                let alphas = trns.unwrap_or_default();
                let colors = rgb
                    .chunks_exact(3)
                    .enumerate()
                    .map(|(i, c)| [c[0], c[1], c[2], alphas.get(i).copied().unwrap_or(255)])
                    .collect::<Vec<_>>();
                // 番号がパレットの外を指す画像は壊れたものとして扱う
                if data.iter().any(|i| usize::from(*i) >= colors.len()) {
                    return Err(err("パレットにない番号の画素があります"));
                }
                Some(Palette { colors })
            }
            (png::ColorType::Indexed, None) => return Err(err("パレットがありません")),
            _ => None,
        };
        let px = Raster::from_rows(w, h, fmt, palette, &data)?;
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
        ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "png" && matches!(shape, Shape::Image { .. })
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

    fn caps(&self, format: &str) -> MetaCaps {
        format_by_id(format).map(|f| f.caps).unwrap_or(
            format_by_id("png").map(|f| f.caps).unwrap_or(MetaCaps {
                lossless: true,
                alpha: true,
                palette: true,
                high_bit_depth: true,
                icc: true,
                dpi: true,
                text: true,
                animation: false,
                loop_points: false,
                markers: false,
                tags: false,
            }),
        )
    }

    fn encode(
        &self,
        value: &Value,
        _format: &str,
        opts: &serde_json::Value,
        out: &mut dyn Write,
        mon: &dyn Monitor,
    ) -> Result<u64, EngineError> {
        let Value::Image(img) = value else {
            return Err(EngineError::Unsupported(
                "PNG には 1 枚の画像だけを書けます".into(),
            ));
        };
        let px = &img.px;
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
        let mut rows = px.to_rows()?;
        if depth == png::BitDepth::Sixteen {
            // 値はリトルエンディアン、PNG はビッグエンディアン
            for pair in rows.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
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
        if let Some((x, y)) = img.meta.dpi {
            // 1 インチあたり → 1 メートルあたり（四捨五入）
            let ppm =
                |dpi: u32| u32::try_from((u64::from(dpi) * 10_000 + 127) / 254).unwrap_or(u32::MAX);
            info.pixel_dims = Some(png::PixelDimensions {
                xppu: ppm(x),
                yppu: ppm(y),
                unit: png::Unit::Meter,
            });
        }
        if let Some(icc) = &img.meta.icc {
            info.icc_profile = Some(Cow::Owned(icc.as_ref().clone()));
        }
        let mut counter = CountingWriter {
            inner: out,
            count: 0,
        };
        {
            let mut enc = png::Encoder::with_info(&mut counter, info).map_err(enc_err)?;
            enc.set_compression(compression);
            for (k, v) in &img.meta.text {
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
            writer.write_image_data(&rows).map_err(enc_err)?;
            writer.finish().map_err(enc_err)?;
        }
        Ok(counter.count)
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
        assert_eq!(dstamp.codec, ID);
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
}
