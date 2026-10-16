//! PSD・PSB の合成済みの画像の読み取り（自作。`psd-kura-dec/1`。Pinned）。書き出しはしない。層は読まない（層の値は将来）。
//!
//! 仕様は Adobe の Photoshop File Formats Specification。
//! - 色のモード：ビットマップ（1 ビット。1 が黒 → Gray8）・グレー（8/16 → Gray8/Gray16）・インデックス（8 → Indexed8）・
//!   RGB（8/16 → Rgb8/Rgb16）。透明があれば GrayA*・Rgba*。CMYK・Lab・マルチチャンネル・ダブルトーン・32 ビットは
//!   色の変換が要るので `Unsupported`（次の段へ回す）。
//! - 圧縮：なし・RLE（PackBits。行の大きさちょうどに展開）。合成済みの画像の ZIP・予測付きの ZIP は、ツールによって
//!   流れの分け方が違い（全チャンネルで 1 つ・チャンネルごと）確かめる相手もないので `Unsupported`（次の段へ）。
//! - 透明：層の数が負、または全体の追加の情報に Mtrn・Mt16・Mt32 があるときだけ、色の次のチャンネルを合成のアルファにする
//!   （それ以外の余分なチャンネルは保存した選択範囲なので使わない）。
//! - Photoshop は、透明があるとき合成済みの色を白で下塗りして保存する（`c = s·a/M + (M − a)`）。ほかのツール（ImageMagick など）は
//!   下塗りしないことがある。下塗りした色なら、どの画素・どの色でも `c ≥ M − a` が成り立つので、それが全部の画素で成り立つときだけ
//!   `s = (c − (M − a))·M / a` を四捨五入して戻す（a = 0 なら 0。M はチャンネルの最大）。ほかはそのまま使う。
//! - 層があって合成済みの画像が本物でない（版の情報のリソース 1057 の hasRealMergedData が偽）ときは `Unsupported`。
//! - 解像度（1005）・ICC（1039）・EXIF（1058）を運ぶ。画素は表示のとおりに保存されているので向きは当てず、EXIF の向きは 1 にする。
//!   インデックスの透明の番号（1047）はパレットのアルファを 0 にする。

use std::io::SeekFrom;
use std::sync::Arc;

use crate::codec::{Decoder, ReadSeek, Tier, exif, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};
use crate::value::{Image, ImageMeta, Palette, PixelFormat, RasterBuilder, Value};

/// PSD・PSB の読み取り。
pub struct PsdCodec;

const DEC_ID: &str = "psd-kura-dec/1";

fn err(e: impl std::fmt::Display) -> EngineError {
    EngineError::Decode {
        codec: DEC_ID.to_string(),
        message: e.to_string(),
    }
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn read_exact(src: &mut dyn ReadSeek, n: usize) -> Result<Vec<u8>, EngineError> {
    let mut b = try_vec(n, 0u8)?;
    src.read_exact(&mut b)
        .map_err(|_| err("ファイルが途中で切れています"))?;
    Ok(b)
}

/// 色のモード。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Bitmap,
    Gray,
    Indexed,
    Rgb,
}

/// 画素を読む前に分かること。
struct Header {
    psb: bool,
    w: u32,
    h: u32,
    depth: u16,
    mode: Mode,
    channels: u16,
    /// 色のチャンネルの数（1 か 3）。
    color_channels: u16,
    alpha: bool,
    fmt: PixelFormat,
    palette: Option<Vec<[u8; 4]>>,
    meta: ImageMeta,
    /// 画像のデータの始まり。
    data_at: u64,
    /// ファイルの長さ（確保の前に、残りの長さと照らす）。
    file_len: u64,
}

fn header(src: &mut dyn ReadSeek, limits: &Limits) -> Result<Header, EngineError> {
    let file_len = src.seek(SeekFrom::End(0))?;
    src.seek(SeekFrom::Start(0))?;
    let h = read_exact(src, 26)?;
    if &h[0..4] != b"8BPS" {
        return Err(err("PSD ではありません"));
    }
    let psb = match be16(&h[4..]) {
        1 => false,
        2 => true,
        v => return Err(err(format!("版 {v} は読めません"))),
    };
    let channels = be16(&h[12..]);
    let (height, width) = (be32(&h[14..]), be32(&h[18..]));
    let depth = be16(&h[22..]);
    let mode = match be16(&h[24..]) {
        0 => Mode::Bitmap,
        1 => Mode::Gray,
        2 => Mode::Indexed,
        3 => Mode::Rgb,
        m => {
            return Err(EngineError::Unsupported(format!(
                "PSD の色のモード {m} は読めません"
            )));
        }
    };
    if width == 0 || height == 0 || channels == 0 || channels > 56 {
        return Err(err("大きさかチャンネルの数が正しくありません"));
    }
    if u64::from(width) > limits.max_dim || u64::from(height) > limits.max_dim {
        return Err(EngineError::Limit("画像が大きすぎます".into()));
    }
    let ok_depth = match mode {
        Mode::Bitmap => depth == 1,
        Mode::Indexed => depth == 8,
        Mode::Gray | Mode::Rgb => depth == 8 || depth == 16,
    };
    if !ok_depth {
        return Err(EngineError::Unsupported(format!(
            "この色のモードの {depth} ビットは読めません"
        )));
    }
    let color_channels: u16 = if mode == Mode::Rgb { 3 } else { 1 };
    if channels < color_channels {
        return Err(err("色のチャンネルが足りません"));
    }
    // 色のモードのデータ（インデックスのパレット）
    let cm_len = u64::from(be32(&read_exact(src, 4)?));
    let mut palette = None;
    if mode == Mode::Indexed {
        if cm_len < 768 {
            return Err(err("パレットがありません"));
        }
        let p = read_exact(src, 768)?;
        palette = Some(
            (0..256)
                .map(|i| [p[i], p[256 + i], p[512 + i], 255])
                .collect::<Vec<_>>(),
        );
        src.seek(SeekFrom::Current(
            i64::try_from(cm_len - 768).map_err(|_| err("長さがあふれます"))?,
        ))?;
    } else {
        src.seek(SeekFrom::Current(
            i64::try_from(cm_len).map_err(|_| err("長さがあふれます"))?,
        ))?;
    }
    // 画像のリソース
    let res_len = u64::from(be32(&read_exact(src, 4)?));
    let res_start = src.stream_position()?;
    let res_end = res_start
        .checked_add(res_len)
        .filter(|e| *e <= file_len)
        .ok_or_else(|| err("リソースの長さが正しくありません"))?;
    let mut meta = ImageMeta::default();
    let mut real_merged = true;
    let mut transparent_index: Option<u16> = None;
    let mut pos = res_start;
    let mut meta_bytes = 0u64;
    while pos + 12 <= res_end {
        src.seek(SeekFrom::Start(pos))?;
        let sig = read_exact(src, 6)?;
        if &sig[0..4] != b"8BIM" {
            break;
        }
        let id = be16(&sig[4..]);
        let name_len = u64::from(read_exact(src, 1)?[0]);
        // 名前（長さの 1 バイトを含めて偶数に詰める）
        let name_padded = (name_len + 1).next_multiple_of(2) - 1;
        src.seek(SeekFrom::Current(name_padded as i64))?;
        let size = u64::from(be32(&read_exact(src, 4)?));
        let data_at = src.stream_position()?;
        if data_at.saturating_add(size) > res_end {
            return Err(err("リソースが途中で切れています"));
        }
        let want = matches!(id, 1005 | 1039 | 1047 | 1057 | 1058);
        if want {
            meta_bytes = meta_bytes.saturating_add(size + 32);
            if meta_bytes > limits.max_meta_bytes {
                return Err(EngineError::Limit("メタデータが大きすぎます".into()));
            }
            let d = read_exact(src, size as usize)?;
            match id {
                1005 if d.len() >= 16 => {
                    // 16.16 の固定小数点。単位 2（1 センチあたり）はインチに直す
                    let dpi = |fixed: u32, unit: u16| -> u32 {
                        let v = u64::from(fixed);
                        let per_inch = if unit == 2 { v * 254 / 100 } else { v };
                        u32::try_from((per_inch + 0x8000) >> 16).unwrap_or(u32::MAX)
                    };
                    let (x, y) = (
                        dpi(be32(&d[0..]), be16(&d[4..])),
                        dpi(be32(&d[8..]), be16(&d[12..])),
                    );
                    if x > 0 && y > 0 {
                        meta.dpi = Some((x, y));
                    }
                }
                1039 => meta.icc = Some(Arc::new(d)),
                1047 if d.len() >= 2 => transparent_index = Some(be16(&d)),
                1057 if d.len() >= 5 => real_merged = d[4] != 0,
                1058 => {
                    meta.exif = Some(Arc::new(exif::with_orientation_reset(exif::strip_header(
                        &d,
                    ))))
                }
                _ => {}
            }
        }
        pos = data_at + size.next_multiple_of(2);
    }
    // 層とマスクの情報：層の数（負なら合成の透明あり）と、全体の追加の情報の Mtrn・Mt16・Mt32 を見る
    src.seek(SeekFrom::Start(res_end))?;
    let lm_len = if psb {
        u64::from_be_bytes(
            read_exact(src, 8)?
                .try_into()
                .map_err(|_| err("長さが読めません"))?,
        )
    } else {
        u64::from(be32(&read_exact(src, 4)?))
    };
    let lm_start = src.stream_position()?;
    let lm_end = lm_start
        .checked_add(lm_len)
        .filter(|e| *e <= file_len)
        .ok_or_else(|| err("層の情報の長さが正しくありません"))?;
    let mut layer_count: i16 = 0;
    let mut merged_transparency = false;
    if lm_len >= 4 {
        let li_len = if psb {
            u64::from_be_bytes(
                read_exact(src, 8)?
                    .try_into()
                    .map_err(|_| err("長さが読めません"))?,
            )
        } else {
            u64::from(be32(&read_exact(src, 4)?))
        };
        let li_start = src.stream_position()?;
        // 長さは、使う前に残りの長さと照らす（あふれない）
        if li_len > lm_end.saturating_sub(li_start) {
            return Err(err("層の情報の長さが正しくありません"));
        }
        if li_len >= 2 {
            layer_count = be16(&read_exact(src, 2)?) as i16;
        }
        // 全体のマスクの情報を飛ばして、追加の情報を見る
        let gm_at = li_start
            .checked_add(
                li_len
                    .checked_next_multiple_of(2)
                    .ok_or_else(|| err("長さがあふれます"))?,
            )
            .ok_or_else(|| err("長さがあふれます"))?;
        if gm_at.checked_add(4).is_some_and(|e| e <= lm_end) {
            src.seek(SeekFrom::Start(gm_at))?;
            let gm_len = u64::from(be32(&read_exact(src, 4)?));
            let mut p = gm_at.saturating_add(4).saturating_add(gm_len);
            while p.checked_add(12).is_some_and(|e| e <= lm_end) {
                src.seek(SeekFrom::Start(p))?;
                let b = read_exact(src, 8)?;
                if &b[0..4] != b"8BIM" && &b[0..4] != b"8B64" {
                    break;
                }
                let key = [b[4], b[5], b[6], b[7]];
                if matches!(&key, b"Mtrn" | b"Mt16" | b"Mt32") {
                    merged_transparency = true;
                }
                // PSB で長さが 8 バイトの鍵
                let wide = psb
                    && matches!(
                        &key,
                        b"LMsk"
                            | b"Lr16"
                            | b"Lr32"
                            | b"Layr"
                            | b"Mt16"
                            | b"Mt32"
                            | b"Mtrn"
                            | b"Alph"
                            | b"FMsk"
                            | b"lnk2"
                            | b"FEid"
                            | b"FXid"
                            | b"PxSD"
                    );
                let len = if wide {
                    u64::from_be_bytes(
                        read_exact(src, 8)?
                            .try_into()
                            .map_err(|_| err("長さが読めません"))?,
                    )
                } else {
                    u64::from(be32(&read_exact(src, 4)?))
                };
                let header = if wide { 16 } else { 12 };
                let Some(padded) = len.checked_next_multiple_of(4) else {
                    break;
                };
                p = p.saturating_add(header).saturating_add(padded);
            }
        }
    }
    // 画像のデータの圧縮の方式（ZIP は読まない）
    src.seek(SeekFrom::Start(lm_end))?;
    let compression = be16(&read_exact(src, 2)?);
    if matches!(compression, 2 | 3) {
        return Err(EngineError::Unsupported(
            "ZIP で圧縮した合成済みの画像は読めません".into(),
        ));
    }
    if compression > 1 {
        return Err(err(format!("圧縮の方式 {compression} は読めません")));
    }
    if layer_count != 0 && !real_merged {
        return Err(EngineError::Unsupported(
            "合成済みの画像が保存されていない PSD です（互換性を最大にして保存すると読めます）"
                .into(),
        ));
    }
    let alpha = (layer_count < 0 || merged_transparency) && channels > color_channels;
    if let (Some(p), Some(t)) = (&mut palette, transparent_index)
        && let Some(c) = p.get_mut(usize::from(t))
    {
        c[3] = 0;
    }
    let sixteen = depth == 16;
    let fmt = match (mode, alpha, sixteen) {
        (Mode::Bitmap, _, _) => PixelFormat::Gray8,
        (Mode::Indexed, _, _) => PixelFormat::Indexed8,
        (Mode::Gray, false, false) => PixelFormat::Gray8,
        (Mode::Gray, true, false) => PixelFormat::GrayA8,
        (Mode::Gray, false, true) => PixelFormat::Gray16,
        (Mode::Gray, true, true) => PixelFormat::GrayA16,
        (Mode::Rgb, false, false) => PixelFormat::Rgb8,
        (Mode::Rgb, true, false) => PixelFormat::Rgba8,
        (Mode::Rgb, false, true) => PixelFormat::Rgb16,
        (Mode::Rgb, true, true) => PixelFormat::Rgba16,
    };
    Ok(Header {
        psb,
        w: width,
        h: height,
        depth,
        mode,
        channels,
        color_channels,
        alpha: alpha && !matches!(mode, Mode::Bitmap | Mode::Indexed),
        fmt,
        palette,
        meta,
        data_at: lm_end,
        file_len,
    })
}

/// PackBits を `out` の長さちょうどに展開する。
fn unpack_bits(src: &[u8], out: &mut [u8]) -> Result<(), EngineError> {
    let (mut i, mut o) = (0usize, 0usize);
    while o < out.len() {
        let n = *src.get(i).ok_or_else(|| err("RLE が途中で切れています"))? as i8;
        i += 1;
        if n >= 0 {
            let len = n as usize + 1;
            let s = src
                .get(i..i + len)
                .ok_or_else(|| err("RLE が途中で切れています"))?;
            out.get_mut(o..o + len)
                .ok_or_else(|| err("RLE が行より長いです"))?
                .copy_from_slice(s);
            i += len;
            o += len;
        } else if n != -128 {
            let len = (1 - i32::from(n)) as usize;
            let v = *src.get(i).ok_or_else(|| err("RLE が途中で切れています"))?;
            out.get_mut(o..o + len)
                .ok_or_else(|| err("RLE が行より長いです"))?
                .fill(v);
            i += 1;
            o += len;
        }
    }
    Ok(())
}

/// 全チャンネルの平面（チャンネルの順、各チャンネルは行を詰めたもの）を読む。使うチャンネルだけ残す。
fn read_planes(
    src: &mut dyn ReadSeek,
    hd: &Header,
    used: usize,
    limits: &Limits,
    mon: &dyn Monitor,
) -> Result<Vec<Vec<u8>>, EngineError> {
    let row = (hd.w as usize * hd.depth as usize).div_ceil(8);
    let plane = row
        .checked_mul(hd.h as usize)
        .filter(|n| (*n as u64).saturating_mul(used as u64) <= limits.max_value_bytes)
        .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?;
    src.seek(SeekFrom::Start(hd.data_at))?;
    let compression = be16(&read_exact(src, 2)?);
    let remaining = hd.file_len.saturating_sub(hd.data_at + 2);
    let mut planes = Vec::with_capacity(used);
    match compression {
        0 => {
            if (plane as u64).saturating_mul(used as u64) > remaining {
                return Err(err("画素のデータが途中で切れています"));
            }
            for _ in 0..used {
                planes.push(read_exact(src, plane)?);
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
            }
        }
        1 => {
            let rows = hd.h as usize * usize::from(hd.channels);
            let width = if hd.psb { 4 } else { 2 };
            let counts_len = rows
                .checked_mul(width)
                .ok_or_else(|| err("長さがあふれます"))?;
            if counts_len as u64 > remaining {
                return Err(err("RLE の行の長さの表が途中で切れています"));
            }
            let counts_raw = read_exact(src, counts_len)?;
            let counts: Vec<usize> = counts_raw
                .chunks_exact(width)
                .map(|c| {
                    if width == 4 {
                        be32(c) as usize
                    } else {
                        usize::from(be16(c))
                    }
                })
                .collect();
            for c in 0..used {
                let mut out = try_vec(plane, 0u8)?;
                for y in 0..hd.h as usize {
                    let n = counts[c * hd.h as usize + y];
                    // 1 行の圧縮は、最悪でも元の行の 2 倍より少し大きいだけ
                    if n > row * 2 + 16 {
                        return Err(err("RLE の行が長すぎます"));
                    }
                    let data = read_exact(src, n)?;
                    unpack_bits(&data, &mut out[y * row..(y + 1) * row])?;
                }
                planes.push(out);
                if mon.canceled() {
                    return Err(EngineError::Canceled);
                }
            }
        }
        _ => return Err(err(format!("圧縮の方式 {compression} は読めません"))),
    }
    Ok(planes)
}

impl Decoder for PsdCodec {
    fn id(&self) -> &'static str {
        DEC_ID
    }

    fn tier(&self) -> Tier {
        Tier::Pinned
    }

    fn can_decode(&self, format: &str) -> bool {
        format == "psd"
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        _format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let hd = header(src, limits)?;
        Ok(Shape::Image {
            w: Dim::Exact(u64::from(hd.w)),
            h: Dim::Exact(u64::from(hd.h)),
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
        let hd = header(src, limits)?;
        let used = usize::from(hd.color_channels) + usize::from(hd.alpha);
        let planes = read_planes(src, &hd, used, limits, mon)?;
        let (w, h) = (hd.w as usize, hd.h as usize);
        let row = (w * hd.depth as usize).div_ceil(8);
        let palette = hd.palette.clone().map(|colors| Palette { colors });
        // 白の下塗りか（下塗りした色でしか成り立たない条件が、全部の画素で成り立つか）
        let matted = hd.alpha && {
            let sixteen = hd.depth == 16;
            let max: u32 = if sixteen { 65_535 } else { 255 };
            let sample = |c: usize, i: usize| -> u32 {
                if sixteen {
                    u32::from(be16(&planes[c][i * 2..]))
                } else {
                    u32::from(planes[c][i])
                }
            };
            (0..w * h).all(|i| {
                let a = sample(used - 1, i);
                (0..used - 1).all(|c| sample(c, i) >= max - a)
            })
        };
        let mut b = RasterBuilder::new(hd.w, hd.h, hd.fmt, palette)?;
        let bpp = hd.fmt.bytes_per_pixel();
        let mut out = vec![0u8; w * bpp];
        for y in 0..h {
            match hd.mode {
                Mode::Bitmap => {
                    for x in 0..w {
                        let bit = (planes[0][y * row + x / 8] >> (7 - x % 8)) & 1;
                        out[x] = if bit == 1 { 0 } else { 255 };
                    }
                }
                Mode::Indexed => out.copy_from_slice(&planes[0][y * row..(y + 1) * row]),
                Mode::Gray | Mode::Rgb => {
                    let sixteen = hd.depth == 16;
                    let max: u32 = if sixteen { 65_535 } else { 255 };
                    for x in 0..w {
                        let sample = |c: usize| -> u32 {
                            if sixteen {
                                u32::from(be16(&planes[c][y * row + x * 2..]))
                            } else {
                                u32::from(planes[c][y * row + x])
                            }
                        };
                        let a = hd.alpha.then(|| sample(used - 1));
                        for c in 0..used {
                            let mut v = sample(c);
                            if let Some(a) = a
                                && matted
                                && c < used - 1
                            {
                                // 白の下塗りを戻す
                                v = if a == 0 {
                                    0
                                } else {
                                    let base = v.saturating_sub(max - a);
                                    ((u64::from(base) * u64::from(max) + u64::from(a) / 2)
                                        / u64::from(a))
                                    .min(u64::from(max)) as u32
                                };
                            }
                            if sixteen {
                                out[(x * used + c) * 2..(x * used + c) * 2 + 2]
                                    .copy_from_slice(&(v as u16).to_le_bytes());
                            } else {
                                out[x * used + c] = v as u8;
                            }
                        }
                    }
                }
            }
            b.push_row(&out)?;
        }
        Ok(Value::Image(Arc::new(Image {
            px: b.finish()?,
            meta: hd.meta,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::testfiles::{FileGen, PsdGen};
    use std::io::Cursor;

    fn base() -> PsdGen {
        PsdGen {
            w: 13,
            h: 7,
            mode: 3,
            depth: 8,
            extra_channels: 0,
            psb: false,
            compression: 1,
            layer_count: 0,
            merged_key: false,
            matted: false,
            real_merged: None,
            transparent_index: None,
            resolution: None,
            exif: None,
            seed: 5,
        }
    }

    fn decode(g: &PsdGen) -> Result<Value, EngineError> {
        let bytes = FileGen::Psd(g.clone()).bytes().unwrap();
        let shape = PsdCodec.shape(
            &mut Cursor::new(bytes.clone()),
            "psd",
            &DecodeOptions::default(),
            &Limits::default(),
        )?;
        let v = PsdCodec.decode(
            &mut Cursor::new(bytes),
            "psd",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )?;
        assert!(shape.admits(&v.shape()), "{shape:?} {:?}", v.shape());
        Ok(v)
    }

    fn image(v: Value) -> Arc<Image> {
        match v {
            Value::Image(i) => i,
            _ => panic!(),
        }
    }

    /// 圧縮なしと RLE、PSD と PSB は同じ値。ZIP は読まない。
    #[test]
    fn compression_and_psb_give_the_same_value() {
        for (mode, depth) in [(3u16, 8u16), (3, 16), (1, 8), (1, 16), (2, 8), (0, 1)] {
            let g = PsdGen {
                mode,
                depth,
                ..base()
            };
            let raw = decode(&PsdGen {
                compression: 0,
                ..g.clone()
            })
            .unwrap();
            assert_eq!(decode(&g).unwrap(), raw, "{mode} {depth}");
            assert_eq!(
                decode(&PsdGen {
                    psb: true,
                    ..g.clone()
                })
                .unwrap(),
                raw,
                "{mode} {depth}"
            );
            assert!(matches!(
                decode(&PsdGen {
                    compression: 2,
                    ..g
                }),
                Err(EngineError::Unsupported(_))
            ));
        }
    }

    /// ビットマップは 1 が黒。インデックスはパレットと 1047 の透明の番号。16 ビットはビッグエンディアンから。
    #[test]
    fn modes_read_as_specified() {
        let bitmap = image(
            decode(&PsdGen {
                mode: 0,
                depth: 1,
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(bitmap.px.format(), PixelFormat::Gray8);
        assert!(
            bitmap
                .px
                .to_rows()
                .unwrap()
                .iter()
                .all(|v| *v == 0 || *v == 255)
        );
        let bytes = FileGen::Psd(PsdGen {
            mode: 0,
            depth: 1,
            compression: 0,
            ..base()
        })
        .bytes()
        .unwrap();
        let first_bit = (bytes[bytes.len() - 2 * 7] >> 7) & 1;
        assert_eq!(
            bitmap.px.pixel(0, 0).unwrap()[0],
            if first_bit == 1 { 0 } else { 255 }
        );
        let idx = image(
            decode(&PsdGen {
                mode: 2,
                transparent_index: Some(3),
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(idx.px.format(), PixelFormat::Indexed8);
        let pal = &idx.px.palette().unwrap().colors;
        assert_eq!(pal.len(), 256);
        assert_eq!(pal[3][3], 0);
        assert_eq!(pal[4][3], 255);
        let deep = image(
            decode(&PsdGen {
                depth: 16,
                compression: 0,
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(deep.px.format(), PixelFormat::Rgb16);
    }

    /// 余分なチャンネルは、層の数が負か Mtrn があるときだけ合成のアルファ。下塗りなら戻し、下塗りでなければそのまま。
    #[test]
    fn transparency_and_white_matte() {
        let spot = image(
            decode(&PsdGen {
                extra_channels: 1,
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(spot.px.format(), PixelFormat::Rgb8);
        let straight = image(
            decode(&PsdGen {
                extra_channels: 2,
                layer_count: -1,
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(straight.px.format(), PixelFormat::Rgba8);
        // 下塗りなしは、そのままの値（チャンネル 0〜3 の乱数）
        let plain = image(
            decode(&PsdGen {
                extra_channels: 0,
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(
            straight.px.pixel(5, 3).unwrap()[..3],
            plain.px.pixel(5, 3).unwrap()[..]
        );
        for depth in [8u16, 16] {
            let matted = image(
                decode(&PsdGen {
                    extra_channels: 1,
                    merged_key: true,
                    matted: true,
                    depth,
                    ..base()
                })
                .unwrap(),
            );
            let straight = image(
                decode(&PsdGen {
                    extra_channels: 1,
                    merged_key: true,
                    matted: false,
                    depth,
                    ..base()
                })
                .unwrap(),
            );
            // 戻した色は、下塗りの前の色と（四捨五入の分だけ）近い。アルファが 0 の画素は色も 0
            let rows = |i: &Image| -> Vec<u32> {
                let r = i.px.to_rows().unwrap();
                if depth == 16 {
                    r.chunks_exact(2)
                        .map(|b| u32::from(u16::from_le_bytes([b[0], b[1]])))
                        .collect()
                } else {
                    r.iter().map(|b| u32::from(*b)).collect()
                }
            };
            let (m, s) = (rows(&matted), rows(&straight));
            let tol = if depth == 16 { 2 } else { 1 };
            for (pm, ps) in m.chunks_exact(4).zip(s.chunks_exact(4)) {
                assert_eq!(pm[3], ps[3]);
                if ps[3] == 0 {
                    assert_eq!(&pm[..3], &[0, 0, 0]);
                    continue;
                }
                // アルファが小さいと、下塗りの丸めの誤差が戻すときに大きくなる
                let bound = tol * (if depth == 16 { 65_535 } else { 255 }) / ps[3] + tol;
                for c in 0..3 {
                    assert!(pm[c].abs_diff(ps[c]) <= bound, "{depth}: {pm:?} {ps:?}");
                }
            }
        }
    }

    /// 1057 が偽で層があれば読まない。1005 の単位・1058 の EXIF の向き（画素は回さず、向きを 1 に）。
    #[test]
    fn resources() {
        assert!(matches!(
            decode(&PsdGen {
                layer_count: 2,
                real_merged: Some(false),
                ..base()
            }),
            Err(EngineError::Unsupported(_))
        ));
        assert!(
            decode(&PsdGen {
                layer_count: 2,
                real_merged: Some(true),
                ..base()
            })
            .is_ok()
        );
        assert!(
            decode(&PsdGen {
                layer_count: 0,
                real_merged: Some(false),
                ..base()
            })
            .is_ok()
        );
        let inch = image(
            decode(&PsdGen {
                resolution: Some((72 << 16, 1)),
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(inch.meta.dpi, Some((72, 72)));
        let cm = image(
            decode(&PsdGen {
                resolution: Some((100 << 16, 2)),
                ..base()
            })
            .unwrap(),
        );
        assert_eq!(cm.meta.dpi, Some((254, 254)));
        let turned = image(
            decode(&PsdGen {
                exif: Some(crate::codec::exif::minimal(6, false)),
                ..base()
            })
            .unwrap(),
        );
        let plain = image(decode(&base()).unwrap());
        assert_eq!(turned.px, plain.px);
        assert_eq!(
            crate::codec::exif::orientation(turned.meta.exif.as_deref().unwrap()),
            1
        );
    }

    /// 壊れたファイルは、自前のコードで落ちずにエラー（直接呼ぶ）。読めたときは形と中身が一致する。
    #[test]
    fn broken_files_are_errors() {
        use rand_chacha::rand_core::Rng;
        let mut r = crate::determinism::rng(8);
        let tight = Limits {
            max_value_bytes: 1 << 20,
            max_dim: 4_096,
            max_meta_bytes: 1 << 16,
            ..Limits::default()
        };
        let (mut ok, mut failed) = (0, 0);
        for g in [
            base(),
            PsdGen {
                psb: true,
                extra_channels: 1,
                layer_count: -1,
                ..base()
            },
            PsdGen {
                mode: 2,
                compression: 0,
                transparent_index: Some(1),
                ..base()
            },
            PsdGen {
                depth: 16,
                merged_key: true,
                extra_channels: 1,
                matted: true,
                ..base()
            },
        ] {
            let good = FileGen::Psd(g).bytes().unwrap();
            for _ in 0..300 {
                let mut b = good.clone();
                for _ in 0..1 + r.next_u32() % 4 {
                    let i = r.next_u32() as usize % b.len();
                    b[i] = r.next_u32() as u8;
                }
                let shape = PsdCodec.shape(
                    &mut Cursor::new(b.clone()),
                    "psd",
                    &DecodeOptions::default(),
                    &tight,
                );
                match PsdCodec.decode(
                    &mut Cursor::new(b),
                    "psd",
                    &DecodeOptions::default(),
                    &tight,
                    &NoMonitor,
                ) {
                    Ok(v) => {
                        assert!(shape.unwrap().admits(&v.shape()));
                        ok += 1;
                    }
                    Err(_) => failed += 1,
                }
            }
        }
        assert!(ok > 100 && failed > 100, "ok {ok} failed {failed}");
    }
}
