//! PNG・APNG の書き手（M16 段 7。`png-kura-enc/1`）。前の png クレートの書き出し（`png-rs-enc/1`）と同じメタデータの
//! 意味で、フィルタの選び方と圧縮（`zlib::Deflate`）は自作の凍結した式。
//!
//! チャンクの並び：IHDR・pHYs（dpi があれば。1 メートルあたりに四捨五入）・iCCP（名前 `_`、圧縮は balanced）・eXIf・acTL
//! （フレームの列）・PLTE と tRNS（Indexed8。アルファの末尾の 255 は省き、残りがなければ tRNS を書かない）・テキスト
//! （キーと値が ASCII なら tEXt、ほかは圧縮しない iTXt。tEXt をキーの順に、続けて iTXt をキーの順に。キーは Latin-1 で
//! 1〜79 バイト）・画像（フレームの列では fcTL と
//! IDAT・fdAT。既定の画像が最初のフレーム、dispose NONE・blend SOURCE）・IEND。
//! 画像のデータは [`IDAT_MAX`] バイトずつのチャンクに分ける（zlib の流れを先頭から切るので、渡す切れ目によらない）。
//!
//! フィルタ（行ごと）：Indexed8 以外は None・Sub・Up・Avg・Paeth のうち、フィルタを当てたバイトを符号つきとみた絶対値の
//! 和が最も小さいもの（同じなら先のもの）。

use std::collections::BTreeMap;
use std::io::Write;

use crate::codec::zlib::{Deflate, Level, crc32_update};
use crate::error::EngineError;
use crate::geom::IRect;
use crate::monitor::Monitor;
use crate::value::{ImageMeta, PixelFormat, Raster, Value};

/// 書き出しの約束の版（書くバイト列が変わる変更は上げる。§9.1）。
pub(crate) const ENC_ID: &str = "png-kura-enc/1";

/// 画像のデータのチャンクの大きさ（fdAT は番号の 4 バイトを除いた分）。
pub(crate) const IDAT_MAX: usize = 1 << 20;
/// PNG の 4 バイトの整数の上限（2³¹ − 1）。
const PNG_INT_MAX: u32 = 0x7FFF_FFFF;

/// 一度に読む行の帯のバイト数の目安。
const BAND_BYTES: usize = 256 * 1024;

/// 1 フレーム（幅 `width`・高さ `height`・画素 `bpp` バイト）を書く作業領域の上限（バイト）：読む行の帯（[`BAND_BYTES`] か
/// 1 行。高さまで）、上の行と試す行・よい行（3 行）、圧縮器（`Deflate::held_bytes`）、チャンクに書くのを待つ圧縮した
/// バイト（[`IDAT_MAX`] 未満と、1 つの帯で閉じるブロック・帯の分か、フレームの全部。容量は倍々に伸びるので 2 倍）。
/// 値の大きさによらず、幅に比例する分と定まった分だけ。メタデータ（ICC を圧縮した写し・iCCP の圧縮器。`max_meta_bytes`
/// まで）は形からはわからないので数えない（前の png クレートの書き出しも同じ）。
pub(crate) fn work_bytes(level: Level, width: u64, height: u64, bpp: u64) -> u64 {
    let row = width.saturating_mul(bpp);
    let total = row.saturating_add(1).saturating_mul(height);
    let band = row.max(BAND_BYTES as u64).min(row.saturating_mul(height));
    let pending = (IDAT_MAX as u64 + (1 << 20))
        .saturating_add(band)
        .min(total.saturating_add(total / 64).saturating_add(1024));
    band.saturating_add(row.saturating_mul(3))
        .saturating_add(Deflate::held_bytes(level, total))
        .saturating_add(pending.saturating_mul(2))
}

fn enc_err(message: impl Into<String>) -> EngineError {
    EngineError::Encode {
        codec: ENC_ID.to_string(),
        message: message.into(),
    }
}

/// チャンクを書く（長さ・種類・中身・CRC）。書いたバイト数を数える。
struct Out<'a> {
    w: &'a mut dyn Write,
    count: u64,
}

impl Out<'_> {
    fn raw(&mut self, b: &[u8]) -> Result<(), EngineError> {
        self.w.write_all(b)?;
        self.count += b.len() as u64;
        Ok(())
    }

    /// 中身を `parts` の継ぎで書く。
    fn chunk(&mut self, kind: &[u8; 4], parts: &[&[u8]]) -> Result<(), EngineError> {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        let len = u32::try_from(len)
            .ok()
            .filter(|&l| l <= i32::MAX as u32)
            .ok_or_else(|| enc_err("チャンクが大きすぎます"))?;
        self.raw(&len.to_be_bytes())?;
        self.raw(kind)?;
        let mut crc = crc32_update(0, kind);
        for p in parts {
            self.raw(p)?;
            crc = crc32_update(crc, p);
        }
        self.raw(&crc.to_be_bytes())
    }
}

/// 値の形式 → PNG の色の種類とビット深度。
fn color_of(f: PixelFormat) -> (u8, u8) {
    match f {
        PixelFormat::Gray8 => (0, 8),
        PixelFormat::GrayA8 => (4, 8),
        PixelFormat::Rgb8 => (2, 8),
        PixelFormat::Rgba8 => (6, 8),
        PixelFormat::Gray16 => (0, 16),
        PixelFormat::GrayA16 => (4, 16),
        PixelFormat::Rgb16 => (2, 16),
        PixelFormat::Rgba16 => (6, 16),
        PixelFormat::Indexed8 => (3, 8),
    }
}

/// キーを Latin-1 のバイトにする（1〜79 バイト。U+00FF を超える文字は書けない）。
fn keyword(k: &str) -> Result<Vec<u8>, EngineError> {
    let mut out = Vec::with_capacity(k.len());
    for c in k.chars() {
        let b = u8::try_from(u32::from(c)).map_err(|_| {
            enc_err(format!(
                "テキストのキーに Latin-1 にない文字があります：{k}"
            ))
        })?;
        out.push(b);
    }
    if out.is_empty() || out.len() > 79 {
        return Err(enc_err(format!(
            "テキストのキーは 1〜79 バイトにしてください：{k}"
        )));
    }
    Ok(out)
}

/// 1 メートルあたりの画素（1 インチあたりから四捨五入）。
fn ppm(dpi: u32) -> u32 {
    u32::try_from((u64::from(dpi) * 10_000 + 127) / 254).unwrap_or(u32::MAX)
}

/// 表示時間（ms）を fcTL の分数（秒）にする。65,535 ms までは ms のまま（読むと同じ値に戻る）、
/// それより長いと 10 ms、さらに長いと 1 秒の単位に四捨五入する。
pub(crate) fn delay_fraction(ms: u32) -> (u16, u16) {
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

/// 書く PNG の中身（[`encode`] と、アンカーの入れ物 `codec::anchor` が組む）。
pub(crate) struct Spec<'a> {
    /// フレームと表示時間（ms）。どれも同じ大きさ・形式。
    pub frames: Vec<(&'a Raster, u32)>,
    /// APNG の繰り返し（0 は無限）。`None` なら最初のフレームだけの画像。
    pub plays: Option<u32>,
    pub dpi: Option<(u32, u32)>,
    pub icc: Option<&'a [u8]>,
    pub exif: Option<&'a [u8]>,
    pub text: Option<&'a BTreeMap<String, String>>,
    /// テキストのあと・画像の前に足す私用のチャンク（種類と中身）。
    pub private: &'a [([u8; 4], &'a [u8])],
    /// 表示時間（ms）→ fcTL の分数。
    pub delay: fn(u32) -> (u16, u16),
    /// 進捗を送る（1 枚の画像は行、APNG はフレーム）。
    pub progress: bool,
}

/// `value`（画像か、キャンバスいっぱいのフレームの列）を PNG・APNG にして `w` へ書く。書いたバイト数を返す。
pub(crate) fn encode(
    value: &Value,
    level: Level,
    w: &mut dyn Write,
    mon: &dyn Monitor,
) -> Result<u64, EngineError> {
    let (frames, meta, plays): (Vec<(&Raster, u32)>, &ImageMeta, Option<u32>) = match value {
        Value::Image(img) => (vec![(&img.px, 0)], &img.meta, None),
        Value::Frames(fr) if fr.is_full() => (
            fr.frames().iter().map(|f| (&f.px, f.duration_ms)).collect(),
            &fr.meta,
            // acTL の繰り返しは PNG の整数（2³¹ − 1 まで。超える値は詰める。前の書き手はそのまま書き、読めないファイルに
            // なっていた）
            Some(fr.loop_count.min(PNG_INT_MAX)),
        ),
        _ => {
            return Err(EngineError::Unsupported(
                "PNG には画像か、キャンバスいっぱいのフレームの列だけを書けます".into(),
            ));
        }
    };
    let spec = Spec {
        frames,
        plays,
        dpi: meta.dpi,
        icc: meta.icc.as_deref().map(Vec::as_slice),
        exif: meta.exif.as_deref().map(Vec::as_slice),
        text: Some(&meta.text),
        private: &[],
        delay: delay_fraction,
        progress: false,
    };
    write(&spec, level, w, mon)
}

/// `spec` を PNG・APNG にして `w` へ書く。書いたバイト数を返す。
pub(crate) fn write(
    spec: &Spec<'_>,
    level: Level,
    w: &mut dyn Write,
    mon: &dyn Monitor,
) -> Result<u64, EngineError> {
    let frames = &spec.frames;
    let plays = spec.plays;
    let Some((px, _)) = frames.first().copied() else {
        return Err(EngineError::Unsupported("フレームがありません".into()));
    };
    let (color, depth) = color_of(px.format());
    let (width, height) = (px.width(), px.height());
    if width == 0 || height == 0 {
        return Err(enc_err("幅と高さは 1 以上にしてください"));
    }
    for (f, _) in frames {
        if f.width() != width || f.height() != height || f.format() != px.format() {
            return Err(enc_err("フレームの大きさと形式はそろえてください"));
        }
    }
    // テキストのチャンクを先に組む（書けないキーなら何も書かずに断る）。tEXt をキーの順に、続けて iTXt をキーの順に
    // 中身は写さず、チャンクを書くときにキーの前置きと値を継ぐ
    let mut texts: Vec<(&[u8; 4], Vec<u8>, &str)> = Vec::new();
    let mut itxts: Vec<(&[u8; 4], Vec<u8>, &str)> = Vec::new();
    for (k, v) in spec.text.into_iter().flatten() {
        let mut head = keyword(k)?;
        head.push(0);
        if k.is_ascii() && v.is_ascii() {
            texts.push((b"tEXt", head, v.as_str()));
        } else {
            // 圧縮なし・言語と訳したキーは空
            head.extend_from_slice(&[0, 0, 0, 0]);
            itxts.push((b"iTXt", head, v.as_str()));
        }
    }
    texts.append(&mut itxts);
    let mut out = Out { w, count: 0 };
    out.raw(&[137, 80, 78, 71, 13, 10, 26, 10])?;
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    ihdr[8] = depth;
    ihdr[9] = color;
    out.chunk(b"IHDR", &[&ihdr])?;
    if let Some((x, y)) = spec.dpi {
        let mut d = [0u8; 9];
        d[..4].copy_from_slice(&ppm(x).to_be_bytes());
        d[4..8].copy_from_slice(&ppm(y).to_be_bytes());
        d[8] = 1;
        out.chunk(b"pHYs", &[&d])?;
    }
    if let Some(icc) = spec.icc {
        // 大きさがわかっているので領域を先に取る（倍々に伸ばさない。値には効かない）
        let mut d = Deflate::with_hint(Level::Balanced, icc.len() as u64);
        d.write(icc);
        d.finish();
        out.chunk(b"iCCP", &[b"_\0\0", d.output()])?;
    }
    if let Some(e) = spec.exif {
        out.chunk(b"eXIf", &[e])?;
    }
    if let Some(plays) = plays {
        let n = u32::try_from(frames.len()).map_err(|_| enc_err("フレームが多すぎます"))?;
        let mut d = [0u8; 8];
        d[..4].copy_from_slice(&n.to_be_bytes());
        d[4..].copy_from_slice(&plays.to_be_bytes());
        out.chunk(b"acTL", &[&d])?;
    }
    if let Some(p) = px.palette() {
        let rgb: Vec<u8> = p.colors.iter().flat_map(|c| [c[0], c[1], c[2]]).collect();
        let mut alphas: Vec<u8> = p.colors.iter().map(|c| c[3]).collect();
        while alphas.last() == Some(&255) {
            alphas.pop();
        }
        out.chunk(b"PLTE", &[&rgb])?;
        if !alphas.is_empty() {
            out.chunk(b"tRNS", &[&alphas])?;
        }
    } else if px.format() == PixelFormat::Indexed8 {
        return Err(enc_err("パレットのない Indexed8 は書けません"));
    }
    for (kind, head, value) in &texts {
        out.chunk(kind, &[head, value.as_bytes()])?;
    }
    for (kind, data) in spec.private {
        out.chunk(kind, &[data])?;
    }
    if mon.canceled() {
        return Err(EngineError::Canceled);
    }
    let mut seq: u32 = 0;
    let shown: &[(&Raster, u32)] = if plays.is_some() {
        frames
    } else {
        &frames[..1]
    };
    let total = shown.len() as u64;
    for (i, (frame, ms)) in shown.iter().enumerate() {
        if plays.is_some() {
            let (num, den) = (spec.delay)(*ms);
            let mut d = [0u8; 26];
            d[..4].copy_from_slice(&seq.to_be_bytes());
            d[4..8].copy_from_slice(&width.to_be_bytes());
            d[8..12].copy_from_slice(&height.to_be_bytes());
            d[20..22].copy_from_slice(&num.to_be_bytes());
            d[22..24].copy_from_slice(&den.to_be_bytes());
            out.chunk(b"fcTL", &[&d])?;
            seq = seq.wrapping_add(1);
        }
        let rows_progress = spec.progress && plays.is_none();
        write_image(
            frame,
            depth,
            level,
            &mut out,
            i > 0,
            &mut seq,
            rows_progress,
            mon,
        )?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        if spec.progress && plays.is_some() {
            mon.progress(i as u64 + 1, total);
        }
    }
    out.chunk(b"IEND", &[])?;
    Ok(out.count)
}

/// 1 枚の画素を、フィルタを当てて圧縮し、IDAT（`fdat` なら番号つきの fdAT）のチャンクに書く。
#[allow(clippy::too_many_arguments)]
fn write_image(
    px: &Raster,
    depth: u8,
    level: Level,
    out: &mut Out<'_>,
    fdat: bool,
    seq: &mut u32,
    progress: bool,
    mon: &dyn Monitor,
) -> Result<(), EngineError> {
    let bpp = px.bpp();
    let row_len = px.width() as usize * bpp;
    let height = px.height();
    let band_rows = (BAND_BYTES / row_len.max(1)).clamp(1, height as usize) as u32;
    let mut band = try_zeroed(row_len * band_rows as usize)?;
    let mut prev = try_zeroed(row_len)?;
    let mut filt = Filters::new(row_len)?;
    // パレットの画像はフィルタを当てない（番号は値の近さを表さない）
    let indexed = px.format() == PixelFormat::Indexed8;
    let mut z = Deflate::with_hint(level, (row_len as u64 + 1) * u64::from(height));
    let mut emit = |z: &mut Deflate, out: &mut Out<'_>, all: bool| -> Result<(), EngineError> {
        let o = z.output();
        let mut at = 0;
        while o.len() - at >= IDAT_MAX || (all && at < o.len()) {
            let n = (o.len() - at).min(IDAT_MAX);
            let piece = &o[at..at + n];
            if fdat {
                out.chunk(b"fdAT", &[&seq.to_be_bytes(), piece])?;
                *seq = seq.wrapping_add(1);
            } else {
                out.chunk(b"IDAT", &[piece])?;
            }
            at += n;
        }
        o.drain(..at);
        Ok(())
    };
    let mut y = 0u32;
    while y < height {
        let n = band_rows.min(height - y);
        let rows = &mut band[..row_len * n as usize];
        px.read_rect(IRect::new(0, i64::from(y), px.width(), n), rows)?;
        if depth == 16 {
            // 値はリトルエンディアン、PNG はビッグエンディアン
            for pair in rows.as_chunks_mut::<2>().0 {
                pair.swap(0, 1);
            }
        }
        for (r, row) in rows.chunks_exact(row_len).enumerate() {
            let up = if r == 0 {
                &prev[..]
            } else {
                &rows[(r - 1) * row_len..r * row_len]
            };
            let (kind, data) = if indexed {
                (0u8, row)
            } else {
                filt.choose(row, up, bpp)
            };
            z.write(&[kind]);
            z.write(data);
        }
        prev.copy_from_slice(&rows[(n as usize - 1) * row_len..]);
        emit(&mut z, out, false)?;
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        y += n;
        if progress {
            mon.progress(u64::from(y), u64::from(height));
        }
    }
    z.finish();
    emit(&mut z, out, true)
}

fn try_zeroed(n: usize) -> Result<Vec<u8>, EngineError> {
    let mut v = Vec::new();
    v.try_reserve_exact(n)
        .map_err(|_| EngineError::Limit(format!("{n} バイトの領域を取れません")))?;
    v.resize(n, 0);
    Ok(v)
}

/// フィルタを当てる行の領域（試す行と、今までで最もよい行）。
struct Filters {
    trial: Vec<u8>,
    best: Vec<u8>,
}

impl Filters {
    fn new(row_len: usize) -> Result<Self, EngineError> {
        Ok(Filters {
            trial: try_zeroed(row_len)?,
            best: try_zeroed(row_len)?,
        })
    }

    /// 行 `cur`（上の行 `prev`）に最もよいフィルタを選び、その番号と当てた行を返す。
    fn choose<'a>(&'a mut self, cur: &'a [u8], prev: &[u8], bpp: usize) -> (u8, &'a [u8]) {
        let mut best_kind = 0u8;
        let mut best_cost = cost(cur);
        if best_cost == 0 {
            return (0, cur);
        }
        for kind in 1..=4u8 {
            let c = apply(kind, cur, prev, bpp, &mut self.trial);
            if c < best_cost {
                best_cost = c;
                best_kind = kind;
                std::mem::swap(&mut self.trial, &mut self.best);
                if c == 0 {
                    break;
                }
            }
        }
        if best_kind == 0 {
            (0, cur)
        } else {
            (best_kind, &self.best)
        }
    }
}

/// バイトを符号つきとみた絶対値。
#[inline(always)]
fn mag(b: u8) -> u32 {
    u32::from((b as i8).unsigned_abs())
}

/// フィルタを当てたバイトを符号つきとみた絶対値の和。
fn cost(row: &[u8]) -> u64 {
    let mut sum = 0u64;
    let (chunks, rest) = row.as_chunks::<64>();
    for c in chunks {
        let mut acc = 0u32;
        for &b in c {
            acc += mag(b);
        }
        sum += u64::from(acc);
    }
    for &b in rest {
        sum += u64::from(mag(b));
    }
    sum
}

/// Paeth の予測（PNG の仕様の式を、比べの結果で選ぶ形にしたもの。値は同じ）。
#[inline(always)]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
    let pa = (ib - ic).abs();
    let pb = (ia - ic).abs();
    let pc = (ia + ib - 2 * ic).abs();
    let (m, v) = if pa <= pb { (pa, a) } else { (pb, b) };
    if m <= pc { v } else { c }
}

/// 32 バイトずつ当てる（固定の長さの配列にして境界の確かめを外し、ベクトル化させる）。`f` は 1 バイトの式で、
/// 引数は今・左・上・左上のバイト。当てた値の絶対値の和を返す。
#[inline(always)]
fn apply_rows(
    out: &mut [u8],
    cur: &[u8],
    left: &[u8],
    up: &[u8],
    upleft: &[u8],
    f: impl Fn(u8, u8, u8, u8) -> u8,
) -> u64 {
    let n = out.len();
    let (o32, orest) = out.as_chunks_mut::<32>();
    let (x32, xrest) = cur[..n].as_chunks::<32>();
    let (a32, arest) = left[..n].as_chunks::<32>();
    let (b32, brest) = up[..n].as_chunks::<32>();
    let (c32, crest) = upleft[..n].as_chunks::<32>();
    let mut sum = 0u64;
    for ((((o, x), a), b), c) in o32.iter_mut().zip(x32).zip(a32).zip(b32).zip(c32) {
        let mut acc = 0u32;
        for i in 0..32 {
            let v = f(x[i], a[i], b[i], c[i]);
            o[i] = v;
            acc += mag(v);
        }
        sum += u64::from(acc);
    }
    for ((((o, &x), &a), &b), &c) in orest.iter_mut().zip(xrest).zip(arest).zip(brest).zip(crest) {
        let v = f(x, a, b, c);
        *o = v;
        sum += u64::from(mag(v));
    }
    sum
}

/// フィルタ `kind`（1〜4）を当てて `out` に書き、当てたバイトの絶対値の和を返す。
fn apply(kind: u8, cur: &[u8], prev: &[u8], bpp: usize, out: &mut [u8]) -> u64 {
    let n = cur.len();
    let bpp = bpp.min(n);
    let (cur, prev) = (&cur[..n], &prev[..n]);
    let out = &mut out[..n];
    // 最初の画素（左と左上は 0）
    let mut head = 0u64;
    for i in 0..bpp {
        let v = match kind {
            1 => cur[i],
            3 => cur[i].wrapping_sub(prev[i] >> 1),
            _ => cur[i].wrapping_sub(prev[i]),
        };
        out[i] = v;
        head += u64::from(mag(v));
    }
    if kind == 2 {
        return head
            + apply_rows(
                &mut out[bpp..],
                &cur[bpp..],
                &cur[bpp..],
                &prev[bpp..],
                &prev[bpp..],
                |x, _, b, _| x.wrapping_sub(b),
            );
    }
    let (o, x, a, b, c) = (
        &mut out[bpp..],
        &cur[bpp..],
        &cur[..n - bpp],
        &prev[bpp..],
        &prev[..n - bpp],
    );
    head + match kind {
        1 => apply_rows(o, x, a, b, c, |x, a, _, _| x.wrapping_sub(a)),
        3 => apply_rows(o, x, a, b, c, |x, a, b, _| {
            x.wrapping_sub((a & b) + ((a ^ b) >> 1))
        }),
        _ => apply_rows(o, x, a, b, c, |x, a, b, c| x.wrapping_sub(paeth(a, b, c))),
    }
}

#[cfg(test)]
mod tests;
