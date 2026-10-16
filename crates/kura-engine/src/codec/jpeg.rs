//! JPEG（読む：zune-jpeg、書く：jpeg-encoder。どちらも版を `=` で固定し、SIMD の feature を外した整数の経路。Pinned）。
//!
//! 読み取り（`jpeg-zune-dec/1`）：
//! - ベースライン・プログレッシブ。グレーは Gray8、YCbCr・RGB は Rgb8。CMYK・YCCK は色の変換に ICC が要るので `Unsupported`
//!   （次の段へ回す）。
//! - ICC（APP2）・EXIF（APP1。向きを画素に当て、EXIF の向きは 1 にする）・解像度（JFIF の単位がインチかセンチのとき）を運ぶ。
//! - プログレッシブは、係数を全部持つので値の 2 倍ほどの作業領域が要る。値の 3 倍が `max_value_bytes` を超えるときは断る。
//!
//! 書き出し（`jpeg-rs-enc/1`）：
//! - 設定は `quality`（1〜100、既定 90）・`chroma`（`auto`・`444`・`422`・`420`。`auto` は品質 90 以上で 444、未満で 420）・
//!   `progressive`（既定 false）。
//! - Gray8 はグレー、Rgb8 と Indexed8（色に広げる）は YCbCr。GrayA8・Rgba8 は、透明な画素がないときだけ書く（あればエラー。
//!   先に背景を合成する）。16 ビットは書けない（先に 8 ビットへ変換する）。
//! - 解像度（JFIF）・ICC（APP2）・EXIF（APP1）を書く。

use std::io::{BufReader, Read, Write};
use std::sync::Arc;

use serde_json::json;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, caps_of, exif, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::imaging::{convert, orient};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::{ParamKind, ParamSchema, Unit, spec};
use crate::shape::{Dim, Shape};
use crate::value::{Image, ImageMeta, PixelFormat, Raster, Value};

/// JPEG の読み書き。
pub struct JpegCodec;

const DEC_ID: &str = "jpeg-zune-dec/1";
const ENC_ID: &str = "jpeg-rs-enc/1";

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

/// ヘッダを読んだ結果。
struct Header {
    w: u32,
    h: u32,
    fmt: PixelFormat,
    out: ColorSpace,
    orientation: u8,
}

fn decoder<'a>(
    src: &'a mut dyn ReadSeek,
    limits: &Limits,
) -> JpegDecoder<BufReader<&'a mut dyn ReadSeek>> {
    let max = usize::try_from(limits.max_dim.min(u64::from(u16::MAX))).unwrap_or(usize::MAX);
    let opts = DecoderOptions::new_safe()
        .set_max_width(max)
        .set_max_height(max);
    JpegDecoder::new_with_options(BufReader::new(src), opts)
}

fn header<T: zune_jpeg::zune_core::bytestream::ZByteReaderTrait>(
    d: &mut JpegDecoder<T>,
    limits: &Limits,
) -> Result<Header, EngineError> {
    d.decode_headers().map_err(err)?;
    let info = d.info().ok_or_else(|| err("ヘッダが読めません"))?;
    let (fmt, out) = match d.input_colorspace() {
        Some(ColorSpace::Luma) => (PixelFormat::Gray8, ColorSpace::Luma),
        Some(ColorSpace::YCbCr | ColorSpace::RGB) => (PixelFormat::Rgb8, ColorSpace::RGB),
        other => {
            return Err(EngineError::Unsupported(format!(
                "この JPEG の色の空間（{other:?}）は読めません"
            )));
        }
    };
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    let progressive = info.sof.is_progressive();
    let orientation = d
        .exif()
        .map_or(1, |e| exif::orientation(exif::strip_header(e)));
    let value_bytes = u64::from(w) * u64::from(h) * fmt.bytes_per_pixel() as u64;
    if progressive && value_bytes.saturating_mul(3) > limits.max_value_bytes {
        return Err(EngineError::Limit(
            "プログレッシブの JPEG を読む作業領域が上限を超えます".into(),
        ));
    }
    Ok(Header {
        w,
        h,
        fmt,
        out,
        orientation,
    })
}

/// APP0 の JFIF の解像度（単位がインチかセンチで、値が 0 でないとき。センチは 1 インチあたりに四捨五入）。
/// zune-jpeg は読まないので、マーカーを先頭から SOS まで、seek せずに前から順に読む（BufReader の読み置きを捨てない）。
fn jfif_density(src: &mut dyn ReadSeek) -> Result<Option<(u32, u32)>, EngineError> {
    src.rewind()?;
    let mut byte = [0u8; 1];
    let mut next =
        |src: &mut dyn ReadSeek| -> Option<u8> { src.read_exact(&mut byte).ok().map(|_| byte[0]) };
    if (next(src), next(src)) != (Some(0xFF), Some(0xD8)) {
        return Ok(None);
    }
    loop {
        if next(src) != Some(0xFF) {
            return Ok(None);
        }
        // 詰め物の 0xFF を読み飛ばす
        let mut m = 0xFF;
        while m == 0xFF {
            match next(src) {
                Some(v) => m = v,
                None => return Ok(None),
            }
        }
        // 長さを持たないマーカー（RSTn・TEM）
        if matches!(m, 0xD0..=0xD7 | 0x01) {
            continue;
        }
        if matches!(m, 0xDA | 0xD9) {
            return Ok(None);
        }
        let (Some(hi), Some(lo)) = (next(src), next(src)) else {
            return Ok(None);
        };
        let len = u64::from(u16::from_be_bytes([hi, lo]));
        if len < 2 {
            return Ok(None);
        }
        let mut body_len = len - 2;
        if m == 0xE0 && body_len >= 12 {
            let mut b = [0u8; 12];
            if src.read_exact(&mut b).is_err() {
                return Ok(None);
            }
            body_len -= 12;
            if &b[0..5] == b"JFIF\0" {
                let unit = b[7];
                let x = u64::from(u16::from_be_bytes([b[8], b[9]]));
                let y = u64::from(u16::from_be_bytes([b[10], b[11]]));
                return Ok(match unit {
                    1 if x > 0 && y > 0 => Some((x as u32, y as u32)),
                    2 if x > 0 && y > 0 => {
                        Some((((x * 254 + 50) / 100) as u32, ((y * 254 + 50) / 100) as u32))
                    }
                    _ => None,
                });
            }
        }
        let skipped = std::io::copy(&mut src.take(body_len), &mut std::io::sink())?;
        if skipped != body_len {
            return Ok(None);
        }
    }
}

impl Decoder for JpegCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "jpeg"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let mut d = decoder(src, limits);
        let hd = header(&mut d, limits)?;
        let (w, h) = orient::oriented_size(hd.w, hd.h, hd.orientation);
        Ok(Shape::Image {
            w: Dim::Exact(u64::from(w)),
            h: Dim::Exact(u64::from(h)),
            fmt: hd.fmt,
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
        let dpi = jfif_density(src)?;
        src.rewind()?;
        let mut d = decoder(src, limits);
        let hd = header(&mut d, limits)?;
        d.set_options(d.options().jpeg_set_out_colorspace(hd.out));
        let len = d
            .output_buffer_size()
            .ok_or_else(|| err("画像が大きすぎます"))?;
        let mut buf = try_vec(len, 0u8)?;
        d.decode_into(&mut buf).map_err(err)?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let mut meta = ImageMeta {
            dpi,
            ..ImageMeta::default()
        };
        if let Some(icc) = d.icc_profile() {
            if icc.len() as u64 > limits.max_meta_bytes {
                return Err(EngineError::Limit("ICC プロファイルが大きすぎます".into()));
            }
            meta.icc = Some(Arc::new(icc));
        }
        if let Some(e) = d.exif() {
            meta.exif = Some(Arc::new(exif::with_orientation_reset(exif::strip_header(
                e,
            ))));
        }
        let px = Raster::from_rows(hd.w, hd.h, hd.fmt, None, &buf)?;
        drop(buf);
        let px = orient::orient(&px, hd.orientation, mon)?;
        Ok(Value::Image(Arc::new(Image { px, meta })))
    }
}

impl Encoder for JpegCodec {
    fn id(&self) -> &'static str {
        ENC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "jpeg"
            && matches!(
                shape,
                Shape::Image {
                    fmt: PixelFormat::Gray8
                        | PixelFormat::GrayA8
                        | PixelFormat::Rgb8
                        | PixelFormat::Rgba8
                        | PixelFormat::Indexed8,
                    w,
                    h,
                } if w.max() <= u64::from(u16::MAX) && h.max() <= u64::from(u16::MAX)
            )
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema {
            params: vec![
                spec(
                    "quality",
                    ParamKind::Int {
                        min: 1,
                        max: 100,
                        step: 1,
                    },
                    Unit::None,
                    json!(90),
                ),
                spec(
                    "chroma",
                    ParamKind::Enum {
                        values: &["auto", "444", "422", "420"],
                    },
                    Unit::None,
                    json!("auto"),
                ),
                spec("progressive", ParamKind::Bool, Unit::None, json!(false)),
            ],
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
        let Value::Image(img) = value else {
            return Err(EngineError::Unsupported(
                "JPEG には 1 枚の画像だけを書けます".into(),
            ));
        };
        let px = &img.px;
        let (w, h) = (
            u16::try_from(px.width()).map_err(|_| enc_err("JPEG の幅は 65,535 まで"))?,
            u16::try_from(px.height()).map_err(|_| enc_err("JPEG の高さは 65,535 まで"))?,
        );
        let quality = opts
            .get("quality")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(90)
            .clamp(1, 100) as u8;
        let chroma = opts
            .get("chroma")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("auto");
        let progressive = opts
            .get("progressive")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let (data, color) = match px.format() {
            PixelFormat::Gray8 => (px.to_rows()?, jpeg_encoder::ColorType::Luma),
            PixelFormat::Rgb8 => (px.to_rows()?, jpeg_encoder::ColorType::Rgb),
            PixelFormat::GrayA8 | PixelFormat::Rgba8 | PixelFormat::Indexed8 => {
                let rgba = convert::to_rgba8_rows(px)?;
                if rgba.chunks_exact(4).any(|p| p[3] != 255) {
                    return Err(EngineError::Unsupported(
                        "JPEG は透明を持てません。先に背景を合成してください".into(),
                    ));
                }
                (rgba, jpeg_encoder::ColorType::Rgba)
            }
            _ => {
                return Err(EngineError::Unsupported(
                    "16 ビットの画像は、先に 8 ビットへ変換してください".into(),
                ));
            }
        };
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let mut bytes = Vec::new();
        {
            let mut enc = jpeg_encoder::Encoder::new(&mut bytes, quality);
            let sampling = match chroma {
                "444" => Some(jpeg_encoder::SamplingFactor::F_1_1),
                "422" => Some(jpeg_encoder::SamplingFactor::F_2_1),
                "420" => Some(jpeg_encoder::SamplingFactor::F_2_2),
                _ => None,
            };
            if let Some(s) = sampling {
                enc.set_sampling_factor(s);
            }
            enc.set_progressive(progressive);
            if let Some((x, y)) = img.meta.dpi {
                enc.set_density(jpeg_encoder::PixelDensity {
                    density: (
                        u16::try_from(x).unwrap_or(u16::MAX),
                        u16::try_from(y).unwrap_or(u16::MAX),
                    ),
                    unit: jpeg_encoder::PixelDensityUnit::Inches,
                });
            }
            if let Some(icc) = &img.meta.icc {
                enc.add_icc_profile(icc).map_err(enc_err)?;
            }
            if let Some(e) = &img.meta.exif {
                enc.add_exif_metadata(e).map_err(enc_err)?;
            }
            enc.encode(&data, w, h, color).map_err(enc_err)?;
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
    use crate::testgen::InputGen;
    use std::io::Cursor;

    fn decode(bytes: Vec<u8>) -> Result<Value, EngineError> {
        CodecSet::pinned()
            .decode(
                &mut Cursor::new(bytes),
                "jpeg",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .map(|(v, _)| v)
    }

    fn encode(v: &Value, opts: serde_json::Value) -> Vec<u8> {
        CodecSet::pinned()
            .encode_bytes(v, "jpeg", &opts, &NoMonitor)
            .unwrap()
            .0
    }

    /// 品質 100・444 で書いて読むと、元の画素に近い（非可逆なので差は小さい）。グレーはグレーのまま。
    #[test]
    fn round_trip_is_close() {
        for fmt in [PixelFormat::Rgb8, PixelFormat::Gray8] {
            let v = InputGen::Sheet {
                cols: 4,
                rows: 3,
                cell: 16,
            }
            .generate()
            .unwrap();
            let Value::Image(img) = &v else { panic!() };
            let rgba = convert::to_rgba8_rows(&img.px).unwrap();
            let rows: Vec<u8> = match fmt {
                PixelFormat::Gray8 => rgba.chunks_exact(4).map(|p| p[1]).collect(),
                _ => rgba
                    .chunks_exact(4)
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect(),
            };
            let src = Value::Image(Arc::new(Image {
                px: Raster::from_rows(img.px.width(), img.px.height(), fmt, None, &rows).unwrap(),
                meta: ImageMeta::default(),
            }));
            let back = decode(encode(&src, json!({ "quality": 100, "chroma": "444" }))).unwrap();
            let Value::Image(b) = back else { panic!() };
            assert_eq!(b.px.format(), fmt);
            let got = b.px.to_rows().unwrap();
            let max_diff = got
                .iter()
                .zip(&rows)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            assert!(max_diff <= 8, "{fmt:?} {max_diff}");
        }
    }

    /// 同じ値と設定からは同じバイト列（R2）。設定の違い（品質・間引き・プログレッシブ）は違うバイト列になり、どれも読める。
    #[test]
    fn options_are_deterministic_and_readable() {
        let v = InputGen::Noise {
            w: 37,
            h: 21,
            fmt: PixelFormat::Rgb8,
            seed: 3,
        }
        .generate()
        .unwrap();
        let mut seen = Vec::new();
        for opts in [
            json!({}),
            json!({ "quality": 50 }),
            json!({ "chroma": "422" }),
            json!({ "chroma": "420", "quality": 95 }),
            json!({ "progressive": true }),
        ] {
            let a = encode(&v, opts.clone());
            assert_eq!(a, encode(&v, opts.clone()));
            assert!(!seen.contains(&a), "{opts}");
            let Value::Image(img) = decode(a.clone()).unwrap() else {
                panic!()
            };
            assert_eq!((img.px.width(), img.px.height()), (37, 21));
            seen.push(a);
        }
        assert!(
            CodecSet::pinned()
                .encode_bytes(&v, "jpeg", &json!({ "quality": 0 }), &NoMonitor)
                .is_err()
        );
    }

    /// 透明な画素がある画像・16 ビットの画像は書かない。不透明な Rgba8 とパレットは書ける。
    #[test]
    fn alpha_and_16_bit_are_refused() {
        let sprite = InputGen::Sprite {
            w: 8,
            h: 8,
            rect: crate::geom::IRect::new(2, 2, 3, 3),
        }
        .generate()
        .unwrap();
        assert!(matches!(
            CodecSet::pinned().encode_bytes(&sprite, "jpeg", &json!({}), &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
        let opaque = Value::Image(Arc::new(Image {
            px: Raster::filled(4, 4, PixelFormat::Rgba8, None, &[1, 2, 3, 255]).unwrap(),
            meta: ImageMeta::default(),
        }));
        assert!(decode(encode(&opaque, json!({}))).is_ok());
        let deep = Value::Image(Arc::new(Image {
            px: Raster::filled(4, 4, PixelFormat::Rgb16, None, &[0; 6]).unwrap(),
            meta: ImageMeta::default(),
        }));
        assert!(!JpegCodec.can_encode("jpeg", &deep.shape()));
    }

    /// ICC・EXIF・解像度を運ぶ。EXIF の向きは画素に当て、向きを 1 にして運ぶ。
    #[test]
    fn metadata_and_orientation() {
        let base = InputGen::Noise {
            w: 16,
            h: 8,
            fmt: PixelFormat::Rgb8,
            seed: 1,
        }
        .generate()
        .unwrap();
        let Value::Image(img) = &base else { panic!() };
        let plain = decode(encode(&base, json!({ "quality": 100 }))).unwrap();
        let mut with = (**img).clone();
        with.meta.icc = Some(Arc::new((0..70_000u32).map(|i| i as u8).collect()));
        with.meta.exif = Some(Arc::new(exif::minimal(6, false)));
        with.meta.dpi = Some((300, 150));
        let bytes = encode(&Value::Image(Arc::new(with)), json!({ "quality": 100 }));
        let shape = JpegCodec
            .shape(
                &mut Cursor::new(bytes.clone()),
                "jpeg",
                &DecodeOptions::default(),
                &Limits::default(),
            )
            .unwrap();
        assert_eq!(
            shape,
            Shape::Image {
                w: Dim::Exact(8),
                h: Dim::Exact(16),
                fmt: PixelFormat::Rgb8
            }
        );
        let Value::Image(back) = decode(bytes).unwrap() else {
            panic!()
        };
        let Value::Image(plain) = plain else { panic!() };
        assert_eq!(back.px, orient::orient(&plain.px, 6, &NoMonitor).unwrap());
        assert_eq!(back.meta.icc.as_ref().map(|i| i.len()), Some(70_000));
        assert_eq!(exif::orientation(back.meta.exif.as_deref().unwrap()), 1);
        assert_eq!(back.meta.dpi, Some((300, 150)));
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

    /// 壊れたファイルは落ちずにエラー。自前のコード（JFIF の読み取り・ヘッダ）の panic はテストの失敗にする。
    #[test]
    fn broken_files_are_errors() {
        let v = InputGen::Noise {
            w: 40,
            h: 30,
            fmt: PixelFormat::Rgb8,
            seed: 2,
        }
        .generate()
        .unwrap();
        let tight = Limits {
            max_pixels: 1 << 16,
            max_value_bytes: 1 << 20,
            ..Limits::default()
        };
        for (opts, seed) in [
            (json!({ "progressive": true }), 5),
            (json!({ "quality": 60 }), 6),
        ] {
            let good = encode(&v, opts);
            let (ok, failed) = mutate_and_read(&JpegCodec, "jpeg", &good, 300, seed, &tight);
            assert!(ok > 10 && failed > 10, "ok {ok} failed {failed}");
        }
    }
}
