//! WebP（`image-webp` クレート。版は `=` で固定。Pinned）。
//!
//! 読み取り（`webp-rs-dec/1`）：
//! - 非可逆（VP8＋ALPH）・可逆（VP8L）・アニメ。アルファがあれば Rgba8、なければ Rgb8。
//! - アニメは image-webp の合成（フレームの blend・dispose を当てたキャンバス。式は四則演算だけで決定的）を、
//!   背景を透明 [0,0,0,0] にして使う（ファイルの背景色のヒントは使わない。ブラウザと同じ）。どのフレームもキャンバスいっぱい。
//!   表示時間はファイルの ms のまま。繰り返しは、無限なら 0、n 回なら n。
//! - ICC・EXIF（向きを画素に当て、EXIF の向きは 1 にする）を運ぶ。2 つの合計が `max_meta_bytes` を超えれば断る。
//!
//! 書き出し（`webp-rs-enc/1`）：可逆（VP8L）の 1 枚の画像だけ。Gray8・GrayA8・Rgb8・Rgba8 と、色に広げた Indexed8。
//! ICC・EXIF を書く。16 ビット・フレームの列は書けない。

use std::io::{BufReader, Write};
use std::sync::Arc;

use crate::codec::{Decoder, Encoder, MetaCaps, ReadSeek, Tier, caps_of, exif, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::imaging::{convert, orient};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::op::ParamSchema;
use crate::shape::{Dim, Fullness, Shape};
use crate::value::{Frames, Image, ImageMeta, PixelFormat, Raster, Value};

/// WebP の読み書き。
pub struct WebpCodec;

const DEC_ID: &str = "webp-rs-dec/1";
const ENC_ID: &str = "webp-rs-enc/1";

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

type Reader<'a> = image_webp::WebPDecoder<BufReader<&'a mut dyn ReadSeek>>;

/// 読み手を作り、メタデータ（上限の中で）を読む。
fn open<'a>(
    src: &'a mut dyn ReadSeek,
    limits: &Limits,
) -> Result<(Reader<'a>, ImageMeta, u8), EngineError> {
    let mut d = image_webp::WebPDecoder::new(BufReader::new(src)).map_err(err)?;
    let meta_limit = usize::try_from(limits.max_meta_bytes).unwrap_or(usize::MAX);
    d.set_memory_limit(meta_limit);
    let too_big = |e: image_webp::DecodingError| match e {
        image_webp::DecodingError::MemoryLimitExceeded => {
            EngineError::Limit("メタデータが大きすぎます".into())
        }
        other => err(other),
    };
    let mut meta = ImageMeta::default();
    if let Some(icc) = d.icc_profile().map_err(too_big)? {
        // ICC と EXIF の合計で max_meta_bytes に収める（EXIF は ICC の残りで抑える）
        d.set_memory_limit(meta_limit.saturating_sub(icc.len()));
        meta.icc = Some(Arc::new(icc));
    }
    let mut orientation = 1;
    if let Some(e) = d.exif_metadata().map_err(too_big)? {
        let tiff = exif::strip_header(&e);
        orientation = exif::orientation(tiff);
        meta.exif = Some(Arc::new(exif::with_orientation_reset(tiff)));
    }
    d.set_memory_limit(usize::try_from(limits.max_value_bytes).unwrap_or(usize::MAX));
    if d.is_animated() {
        d.set_background_color([0, 0, 0, 0]).map_err(err)?;
    }
    Ok((d, meta, orientation))
}

fn fmt_of(d: &Reader<'_>) -> PixelFormat {
    if d.has_alpha() {
        PixelFormat::Rgba8
    } else {
        PixelFormat::Rgb8
    }
}

impl Decoder for WebpCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "webp"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let (d, _, orientation) = open(src, limits)?;
        let (w, h) = d.dimensions();
        let (w, h) = orient::oriented_size(w, h, orientation);
        let (w, h) = (Dim::Exact(u64::from(w)), Dim::Exact(u64::from(h)));
        let fmt = fmt_of(&d);
        Ok(if d.is_animated() {
            Shape::Frames {
                count: Dim::Exact(u64::from(d.num_frames())),
                w,
                h,
                fmt,
                full: Fullness::Full,
            }
        } else {
            Shape::Image { w, h, fmt }
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
        let (mut d, meta, orientation) = open(src, limits)?;
        let (w, h) = d.dimensions();
        let fmt = fmt_of(&d);
        let len = d
            .output_buffer_size()
            .ok_or_else(|| err("画像が大きすぎます"))?;
        let mut buf = try_vec(len, 0u8)?;
        if !d.is_animated() {
            d.read_image(&mut buf).map_err(err)?;
            let px = Raster::from_rows(w, h, fmt, None, &buf)?;
            drop(buf);
            let px = orient::orient(&px, orientation, mon)?;
            return Ok(Value::Image(Arc::new(Image { px, meta })));
        }
        let count = d.num_frames();
        if u64::from(count) > limits.max_frames || count == 0 {
            return Err(EngineError::Limit(format!(
                "フレームの数が上限の外です（{count}）"
            )));
        }
        let plays = match d.loop_count() {
            image_webp::LoopCount::Forever => 0,
            image_webp::LoopCount::Times(n) => u32::from(n.get()),
        };
        let mut frames = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let ms = d.read_frame(&mut buf).map_err(err)?;
            let px = Raster::from_rows(w, h, fmt, None, &buf)?;
            frames.push((orient::orient(&px, orientation, mon)?, ms));
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
        }
        Ok(Value::Frames(Arc::new(Frames::full(frames, meta, plays)?)))
    }
}

impl Encoder for WebpCodec {
    fn id(&self) -> &'static str {
        ENC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_encode(&self, format: &str, shape: &Shape) -> bool {
        format == "webp"
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
                } if w.max() <= 16_384 && h.max() <= 16_384
            )
    }

    fn options(&self, _format: &str) -> ParamSchema {
        ParamSchema { params: vec![] }
    }

    fn caps(&self, format: &str, _opts: &serde_json::Value) -> MetaCaps {
        // 可逆の 1 枚だけ書ける（アニメは書けない）
        MetaCaps {
            animation: false,
            ..caps_of(format)
        }
    }

    fn encode(
        &self,
        value: &Value,
        _format: &str,
        _opts: &serde_json::Value,
        out: &mut dyn Write,
        mon: &dyn Monitor,
    ) -> Result<u64, EngineError> {
        let Value::Image(img) = value else {
            return Err(EngineError::Unsupported(
                "WebP には 1 枚の画像だけを書けます（アニメは書けません）".into(),
            ));
        };
        let px = &img.px;
        let (data, color) = match px.format() {
            PixelFormat::Gray8 => (px.to_rows()?, image_webp::ColorType::L8),
            PixelFormat::GrayA8 => (px.to_rows()?, image_webp::ColorType::La8),
            PixelFormat::Rgb8 => (px.to_rows()?, image_webp::ColorType::Rgb8),
            PixelFormat::Rgba8 | PixelFormat::Indexed8 => {
                (convert::to_rgba8_rows(px)?, image_webp::ColorType::Rgba8)
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
            let mut enc = image_webp::WebPEncoder::new(&mut bytes);
            if let Some(icc) = &img.meta.icc {
                enc.set_icc_profile(icc.as_ref().clone());
            }
            if let Some(e) = &img.meta.exif {
                enc.set_exif_metadata(e.as_ref().clone());
            }
            enc.encode(&data, px.width(), px.height(), color)
                .map_err(enc_err)?;
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
    use serde_json::json;
    use std::io::Cursor;

    fn decode(bytes: Vec<u8>) -> Value {
        let shape = WebpCodec
            .shape(
                &mut Cursor::new(bytes.clone()),
                "webp",
                &DecodeOptions::default(),
                &Limits::default(),
            )
            .unwrap();
        let v = WebpCodec
            .decode(
                &mut Cursor::new(bytes),
                "webp",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap();
        assert!(shape.admits(&v.shape()));
        v
    }

    /// 可逆で書いて読むと、RGBA に広げた画素が同じ（グレーは RGB で戻る）。ICC と EXIF も戻る。
    #[test]
    fn lossless_round_trip() {
        let set = CodecSet::pinned();
        for fmt in [
            PixelFormat::Gray8,
            PixelFormat::GrayA8,
            PixelFormat::Rgb8,
            PixelFormat::Rgba8,
            PixelFormat::Indexed8,
        ] {
            let Value::Image(img) = (InputGen::Noise {
                w: 33,
                h: 17,
                fmt,
                seed: 8,
            })
            .generate()
            .unwrap() else {
                panic!()
            };
            let mut img = Arc::unwrap_or_clone(img);
            img.meta.icc = Some(Arc::new(vec![5; 300]));
            img.meta.exif = Some(Arc::new(exif::minimal(1, true)));
            let v = Value::Image(Arc::new(img));
            let (bytes, _) = set
                .encode_bytes(&v, "webp", &json!({}), &NoMonitor)
                .unwrap();
            let Value::Image(back) = decode(bytes) else {
                panic!()
            };
            let Value::Image(src) = &v else { panic!() };
            assert_eq!(
                convert::to_rgba8_rows(&back.px).unwrap(),
                convert::to_rgba8_rows(&src.px).unwrap(),
                "{fmt:?}"
            );
            assert_eq!(back.meta.icc, src.meta.icc);
            assert_eq!(back.meta.exif, src.meta.exif);
        }
    }

    /// EXIF の向きを画素に当てる。メタデータが上限を超えれば断る。
    #[test]
    fn orientation_and_metadata_limits() {
        let set = CodecSet::pinned();
        let Value::Image(img) = (InputGen::Noise {
            w: 6,
            h: 4,
            fmt: PixelFormat::Rgb8,
            seed: 2,
        })
        .generate()
        .unwrap() else {
            panic!()
        };
        let mut turned = (*img).clone();
        turned.meta.exif = Some(Arc::new(exif::minimal(8, false)));
        let (bytes, _) = set
            .encode_bytes(
                &Value::Image(Arc::new(turned)),
                "webp",
                &json!({}),
                &NoMonitor,
            )
            .unwrap();
        let Value::Image(back) = decode(bytes.clone()) else {
            panic!()
        };
        assert_eq!(back.px, orient::orient(&img.px, 8, &NoMonitor).unwrap());
        let small = Limits {
            max_meta_bytes: 4,
            ..Limits::default()
        };
        assert!(matches!(
            WebpCodec.shape(
                &mut Cursor::new(bytes),
                "webp",
                &DecodeOptions::default(),
                &small
            ),
            Err(EngineError::Limit(_))
        ));
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

    /// 壊れたファイルは落ちずにエラー。自前のコードの panic はテストの失敗にする。
    #[test]
    fn broken_files_are_errors() {
        let set = CodecSet::pinned();
        let v = InputGen::Noise {
            w: 30,
            h: 20,
            fmt: PixelFormat::Rgba8,
            seed: 3,
        }
        .generate()
        .unwrap();
        let mut img = match v {
            Value::Image(i) => (*i).clone(),
            _ => panic!(),
        };
        img.meta.exif = Some(Arc::new(exif::minimal(3, false)));
        let (good, _) = set
            .encode_bytes(&Value::Image(Arc::new(img)), "webp", &json!({}), &NoMonitor)
            .unwrap();
        let tight = Limits {
            max_pixels: 1 << 14,
            max_value_bytes: 1 << 20,
            ..Limits::default()
        };
        let (ok, failed) = mutate_and_read(&WebpCodec, "webp", &good, 300, 4, &tight);
        assert!(ok > 10 && failed > 10, "ok {ok} failed {failed}");
        let lossy = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/webp-anim.webp"),
        )
        .unwrap();
        let (ok, failed) = mutate_and_read(&WebpCodec, "webp", &lossy, 300, 5, &tight);
        assert!(ok > 10 && failed > 10, "anim ok {ok} failed {failed}");
    }
}
