//! ImageIO の読み取りの backend（`imageio-dec/1`）。エンジンの `PlatformImageDecoder` で包んで `Decoder` にする。
//! 約束は Swift の `ImageDecode.swift` とエンジンの `codec::platform::image` に書いた。
//!
//! 展開（`kura_img_begin`）は、壊れたファイルで ImageIO の中から戻らなくなることがある（HEIC の VideoToolbox。Apple 側の
//! 不具合）ので、専用のスレッドで行う（[`crate::codec::watchdog`]）。[`begin_timeout`] までに戻らなければ、スレッドと
//! ハンドルを見捨てて壊れたファイル（`Failed` → `Decode`）にし、中止を頼まれたら時間切れを待たずに見捨てて `Canceled`
//! にする。見捨てて戻っていないスレッドが [`MAX_STALLED_DECODES`] 本に達したら、Platform の画像の段を断る（`Unsupported`。
//! 次の段へ回る）。

use std::ffi::CString;
use std::sync::Arc;
use std::time::Duration;

use kura_engine::Monitor;
use kura_engine::codec::CodecEnv;
use kura_engine::codec::platform::{
    BackendError, DecodedFrame, ImageBackend, ImageHandle, ImageInfo,
};

use crate::bridge::SwiftImage;
use crate::codec::IMAGEIO_DEC;
use crate::codec::caps::{IMAGE_ROUTES, MacCaps};
use crate::codec::convert;
use crate::codec::watchdog::{Stop, Watchdog};

/// 展開の時間の上限の基本（どの大きさでも待つ時間）。8064×6048 の HEIC の展開は、この Mac で 0.2 秒（PNG は 0.4 秒）。
pub const BEGIN_TIMEOUT_BASE: Duration = Duration::from_secs(20);
/// 展開の時間の上限に、フレームの画素 100 万ごとに足す時間（上限の 2^28 画素で 20 + 67 秒）。
pub const BEGIN_TIMEOUT_PER_MEGAPIXEL: Duration = Duration::from_millis(250);
/// 見捨てて戻っていない展開のスレッドの数の上限（達したら Platform の画像の段を断る）。
pub const MAX_STALLED_DECODES: usize = 4;

/// プロセスで 1 つの展開の見張り。
static IMAGEIO_WATCHDOG: Watchdog = Watchdog::new(MAX_STALLED_DECODES);

/// フレーム（`pixels` 画素）の展開の時間の上限：[`BEGIN_TIMEOUT_BASE`] + [`BEGIN_TIMEOUT_PER_MEGAPIXEL`] × ⌈画素 / 10^6⌉。
pub fn begin_timeout(pixels: u64) -> Duration {
    let mpx = u32::try_from(pixels.div_ceil(1_000_000)).unwrap_or(u32::MAX);
    BEGIN_TIMEOUT_BASE.saturating_add(BEGIN_TIMEOUT_PER_MEGAPIXEL.saturating_mul(mpx))
}

/// 見捨てたスレッドが上限に達したときの文言。
fn stalled_message(n: usize) -> String {
    format!("ImageIO の展開が {n} 本戻っていないので、Platform の画像の段を使いません")
}

/// ImageIO の backend。
pub struct ImageIoBackend {
    env: CodecEnv,
    /// 読む形式と、その形式で受け入れる UTI（Swift に渡すカンマ区切りの文字列）。能力にあるものだけ。
    routes: Vec<(&'static str, CString)>,
    /// 展開の見張り（見捨てたスレッドの数）。
    watchdog: &'static Watchdog,
    /// 展開の時間の上限（フレームの画素の数から）。
    timeout: fn(u64) -> Duration,
}

impl ImageIoBackend {
    /// 能力から組む（ImageIO が読めない形式は扱わない）。
    pub fn new(caps: &MacCaps) -> Self {
        let routes = IMAGE_ROUTES
            .iter()
            .filter(|(format, _)| caps.image_readable(format))
            .filter_map(|(format, utis)| Some((*format, CString::new(utis.join(",")).ok()?)))
            .collect();
        Self {
            env: caps.image_env(),
            routes,
            watchdog: &IMAGEIO_WATCHDOG,
            timeout: begin_timeout,
        }
    }

    /// 経路を指定して組む（テストで PNG などを ImageIO に読ませて固定の段と照らす）。
    #[cfg(test)]
    pub(crate) fn for_formats(caps: &MacCaps, routes: &[(&'static str, &str)]) -> Self {
        Self {
            env: caps.image_env(),
            routes: routes
                .iter()
                .map(|(f, u)| (*f, CString::new(*u).unwrap()))
                .collect(),
            watchdog: &IMAGEIO_WATCHDOG,
            timeout: begin_timeout,
        }
    }

    /// 見張りと時間の上限を差し替える（テストで、プロセスの見張りを汚さずに時間切れを確かめる）。
    #[cfg(test)]
    pub(crate) fn with_watchdog(
        self,
        watchdog: &'static Watchdog,
        timeout: fn(u64) -> Duration,
    ) -> Self {
        Self {
            watchdog,
            timeout,
            ..self
        }
    }

    fn utis(&self, format: &str) -> Option<&CString> {
        self.routes
            .iter()
            .find(|(f, _)| *f == format)
            .map(|(_, u)| u)
    }
}

impl ImageBackend for ImageIoBackend {
    type Handle = ImageIoHandle;

    fn id(&self) -> &'static str {
        IMAGEIO_DEC
    }

    fn can_decode(&self, format: &str) -> bool {
        self.utis(format).is_some()
    }

    fn env(&self) -> Option<CodecEnv> {
        Some(self.env.clone())
    }

    fn open(&self, bytes: Arc<Vec<u8>>, format: &str) -> Result<ImageIoHandle, BackendError> {
        let utis = self.utis(format).ok_or_else(|| {
            BackendError::Unsupported(format!("{format} は ImageIO で読みません"))
        })?;
        if let Some(n) = self.watchdog.exhausted() {
            return Err(BackendError::Unsupported(stalled_message(n)));
        }
        // Swift は入力を自分の CFData に写す（ImageIO に Rust の領域を貸さない）ので、bytes はここで落としてよい
        let image = SwiftImage::open(&bytes, utis)?;
        let info = convert::image_info(&image.info())?;
        Ok(ImageIoHandle {
            image: Some(image),
            info,
            watchdog: self.watchdog,
            timeout: (self.timeout)(u64::from(info.width) * u64::from(info.height)),
        })
    }
}

/// 開いた画像。
pub struct ImageIoHandle {
    /// 開いた画像。展開のスレッドを見捨てたら `None`（閉じない。スレッドが戻ればスレッドの中で閉じる）。
    image: Option<SwiftImage>,
    info: ImageInfo,
    watchdog: &'static Watchdog,
    /// 1 フレームの展開の時間の上限。
    timeout: Duration,
}

/// 見捨てたあとのハンドルを使おうとした（adapter は展開の失敗のあとに続けないので、kura の内部の誤り）。
fn abandoned() -> BackendError {
    BackendError::Invalid("展開のスレッドとともに見捨てたハンドルです".into())
}

impl ImageIoHandle {
    fn image(&mut self) -> Result<&mut SwiftImage, BackendError> {
        self.image.as_mut().ok_or_else(abandoned)
    }
}

impl ImageHandle for ImageIoHandle {
    fn info(&self) -> ImageInfo {
        self.info
    }

    /// 専用のスレッドで展開する（時間切れ・中止ではスレッドとハンドルを見捨てる）。
    fn begin(&mut self, index: u32, mon: &dyn Monitor) -> Result<DecodedFrame, BackendError> {
        let mut image = self.image.take().ok_or_else(abandoned)?;
        let limit = self.timeout;
        let got = self
            .watchdog
            .run("kura-imageio-begin", limit, mon, move |flag| {
                let frame = image.begin(index, flag);
                (image, frame)
            });
        match got {
            Ok((image, frame)) => {
                self.image = Some(image);
                convert::decoded_frame(&frame?)
            }
            Err(Stop::Canceled) => Err(BackendError::Canceled),
            Err(Stop::TimedOut(d)) => Err(BackendError::Failed(format!(
                "画像を展開できません（ImageIO の展開が {:.1} 秒で終わりません。壊れたファイル。展開しているスレッドを見捨てました）",
                d.as_secs_f64()
            ))),
            Err(Stop::Exhausted(n)) => Err(BackendError::Unsupported(stalled_message(n))),
            Err(Stop::Spawn(e)) => Err(BackendError::Failed(format!(
                "展開のスレッドを作れません: {e}"
            ))),
            Err(Stop::Panicked) => Err(BackendError::Invalid(
                "展開のスレッドが panic しました".into(),
            )),
        }
    }

    fn read_rows(
        &mut self,
        y0: u32,
        rows: u32,
        row_bytes: usize,
        out: &mut [u8],
    ) -> Result<(), BackendError> {
        self.image()?.read_rows(y0, rows, row_bytes, out)
    }

    fn copy_icc(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        self.image()?.copy_icc(out)
    }

    /// ImageIO は生の EXIF を返さない（exif_len は常に 0）。
    fn copy_exif(&mut self, out: &mut [u8]) -> Result<(), BackendError> {
        if out.is_empty() {
            Ok(())
        } else {
            Err(BackendError::Invalid("EXIF はありません".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;

    use kura_engine::codec::platform::{PlatformImageDecoder, unpremultiply_rows};
    use kura_engine::codec::{CodecSet, Decoder, Tier, exif};
    use kura_engine::doc::DecodeOptions;
    use kura_engine::monitor::CancelAfter;
    use kura_engine::testfiles::FileGen;
    use kura_engine::value::{Image, ImageMeta, PixelFormat, Raster};
    use kura_engine::{EngineError, Limits, NoMonitor, Value};
    use rand_chacha::rand_core::Rng;

    use super::*;
    use crate::codec::codec_caps;
    use crate::codec::testutil::*;

    /// PNG を ImageIO に読ませる decoder（固定の段と照らすため）。
    fn png_decoder() -> PlatformImageDecoder<ImageIoBackend> {
        let caps = codec_caps().unwrap();
        PlatformImageDecoder::new(ImageIoBackend::for_formats(&caps, &[("png", "public.png")]))
    }

    fn read(dec: &dyn Decoder, bytes: &[u8], format: &str) -> Result<Value, EngineError> {
        dec.decode(
            &mut Cursor::new(bytes.to_vec()),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
    }

    fn img(v: &Value) -> &Image {
        match v {
            Value::Image(i) => i,
            other => panic!("画像ではありません: {:?}", other.shape()),
        }
    }

    /// なめらかな RGB の画像（非可逆の HEIC でも元に近く読める）。
    fn gradient(w: u32, h: u32) -> Value {
        let mut rows = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rows.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 128]);
            }
        }
        image(
            Raster::from_rows(w, h, PixelFormat::Rgb8, None, &rows).unwrap(),
            ImageMeta::default(),
        )
    }

    /// HEIC のフィクスチャ（`gradient(300, 200)` を ImageIO が品質 0.9 で書いたもの）。
    fn heic() -> Vec<u8> {
        fixture("heic-gradient-300x200.heic")
    }

    /// 8 ビットの画素の PSNR（dB）。
    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let se: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum();
        let mse = se / a.len() as f64;
        if mse == 0.0 {
            f64::INFINITY
        } else {
            10.0 * (255.0 * 255.0 / mse).log10()
        }
    }

    /// 完了条件：ImageIO（imageio-dec/1 の約束：1 度だけ展開・色空間を変えずに帯ずつ変換）と png クレート（固定の段）の
    /// 画素がビット一致する。9 つの画素の形式（パレットは RGB(A) に広げて）× 帯の境目をまたぐ大きさ。ICC は ImageIO の
    /// 色空間のもの（ICC のない PNG でも推定した sRGB・Gray）を運ぶ。
    #[test]
    fn imageio_matches_png_rs_bit_for_bit() {
        let _g = shared();
        let dec = png_decoder();
        let pinned = CodecSet::pinned();
        let mut checked = 0;
        for fmt in PixelFormat::ALL {
            for (i, (w, h)) in [(300, 270), (1, 1), (257, 3), (3, 513)]
                .into_iter()
                .enumerate()
            {
                let png = png_bytes(&noise(w, h, fmt, 11 + i as u64));
                let (want, _) = decode(&pinned, &png, "png").unwrap();
                let got = read(&dec, &png, "png").unwrap();
                let (gf, gr) = rows(&got, false);
                let (wf, wr) = rows(&want, gf.has_alpha());
                assert_eq!(gf, wf, "{fmt:?} {w}×{h}");
                assert!(gr == wr, "{fmt:?} {w}×{h}: 画素が違います");
                assert!(img(&got).meta.icc.is_some(), "{fmt:?}");
                checked += 1;
            }
        }
        assert_eq!(checked, 36);
    }

    /// 段 9 の調査のコーパス（fixtures の `png-*.png`。README に作り方と中身）。
    const PNG_CORPUS: &[&str] = &[
        "gray1",
        "gray2",
        "gray4",
        "gray2-interlaced",
        "gray2-trns",
        "gray8-trns",
        "gray16-trns",
        "rgb8-trns",
        "rgb16-trns",
        "pal1",
        "pal2",
        "pal8",
        "pal8-trns",
        "pal4-trns",
        "pal4-interlaced",
        "rgba8-interlaced",
        "gray8-gama1",
        "rgb8-gama045",
        "rgb8-gama045-chrm",
        "rgba8-gama1",
        "rgb16-gama08",
        "rgba8-srgb",
        "rgb8-sbit565",
        "rgb8-icc-bogus",
        "rgba8-icc-p3",
        "rgba16-icc-p3",
        "gray8-icc-gray22",
        "gray8-icc-p3",
        "pal8-icc-p3",
    ];

    /// 埋め込みの有効な ICC（色空間に合うもの）を持つ PNG。ImageIO はこのバイト列をそのまま返す（ほかは推定した色空間の
    /// もの：グレーの画像の RGB のプロファイル `gray8-icc-p3`・壊れたもの `rgb8-icc-bogus`・`fg-meta-chunks`）。
    const VALID_ICC: &[&str] = &[
        "rgba8-icc-p3",
        "rgba16-icc-p3",
        "gray8-icc-gray22",
        "pal8-icc-p3",
    ];

    /// 完了条件の続き：png-rs-dec/2 のゴールデンのコーパス（`goldens-codecs/png-rs-dec@2.jsonl` の FileGen。静止画 15 例）と、
    /// 調査のコーパス（[`PNG_CORPUS`] の 29 例：1・2・4 ビットのグレー、グレー・RGB・16 ビットの tRNS、tRNS のない
    /// パレット、インターレース、gAMA・cHRM・sRGB・sBIT、壊れた ICC・P3 の ICC）を ImageIO で読み、固定の段
    /// （png-rs-dec/2）と画素・大きさがビット一致する。tRNS のないパレット（`pal1`・`pal2`・`pal8`・`pal8-icc-p3`・
    /// `fg-palette1`）は、ImageIO の indexed の枝（基の色空間に広げる）を通る。ICC は ImageIO の色空間のもの（[`VALID_ICC`]
    /// は埋め込みのバイト列のまま）。APNG は照らさない（合成が違う）。
    #[test]
    fn imageio_matches_png_rs_on_the_corpus() {
        let _g = shared();
        let dec = png_decoder();
        let pinned = CodecSet::pinned();
        let golden = std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../kura-engine/goldens-codecs/png-rs-dec@2.jsonl"),
        )
        .unwrap();
        let mut inputs: Vec<(String, Vec<u8>)> = golden
            .lines()
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                let src: FileGen = serde_json::from_value(v["source"].clone()).unwrap();
                (
                    format!("fg-{}", v["name"].as_str().unwrap()),
                    src.bytes().unwrap(),
                )
            })
            .collect();
        inputs.extend(
            PNG_CORPUS
                .iter()
                .map(|n| (n.to_string(), fixture(&format!("png-{n}.png")))),
        );
        let mut checked = 0;
        for (name, png) in &inputs {
            let (want, stamp) = decode(&pinned, png, "png").unwrap();
            assert_eq!(stamp.codec, "png-rs-dec/2", "{name}");
            if !matches!(want, Value::Image(_)) {
                continue;
            }
            let got = read(&dec, png, "png").unwrap_or_else(|e| panic!("{name}: {e}"));
            let (gf, gr) = rows(&got, false);
            let (wf, wr) = rows(&want, gf.has_alpha());
            assert_eq!(gf, wf, "{name}");
            let size = |v: &Value| (img(v).px.width(), img(v).px.height());
            assert_eq!(size(&got), size(&want), "{name}");
            assert!(gr == wr, "{name}: 画素が違います");
            assert!(img(&got).meta.icc.is_some(), "{name}");
            if VALID_ICC.contains(&name.as_str()) {
                assert_eq!(img(&got).meta.icc, img(&want).meta.icc, "{name}");
            }
            checked += 1;
        }
        assert_eq!(checked, 15 + PNG_CORPUS.len());
    }

    /// パレットの BMP（BITMAPINFOHEADER・下から上・行は 4 バイトに詰める）。`idx` は上から行の順の番号。
    fn palette_bmp(w: u32, h: u32, bits: u16, palette: &[[u8; 3]], idx: &[u8]) -> Vec<u8> {
        let row = (w as usize * usize::from(bits)).div_ceil(32) * 4;
        let off = 14 + 40 + palette.len() * 4;
        let size = off + row * h as usize;
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&(size as u32).to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&(off as u32).to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&((row * h as usize) as u32).to_le_bytes());
        b.extend_from_slice(&2_835u32.to_le_bytes());
        b.extend_from_slice(&2_835u32.to_le_bytes());
        b.extend_from_slice(&(palette.len() as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        for [r, g, bl] in palette {
            b.extend_from_slice(&[*bl, *g, *r, 0]);
        }
        for y in (0..h).rev() {
            b.extend_from_slice(&pack_row(
                &idx[(y * w) as usize..((y + 1) * w) as usize],
                bits,
                row,
            ));
        }
        b
    }

    /// 番号の行を `bits` ビットずつ上位から詰める（`len` バイトに）。
    fn pack_row(idx: &[u8], bits: u16, len: usize) -> Vec<u8> {
        let bits = usize::from(bits);
        let mut r = vec![0u8; len];
        for (x, &i) in idx.iter().enumerate() {
            let at = x * bits;
            r[at / 8] |= i << (8 - bits - at % 8);
        }
        r
    }

    /// パレットの TIFF（リトルエンディアン・無圧縮・1 つの帯・Photometric 3）。ColorMap は 8 ビットの色 × 256（ImageIO は
    /// 16 ビットの色を ⌊v/256⌉ で 8 ビットにするので、× 257 だと 1 ずれる色がある）。
    fn palette_tiff(w: u32, h: u32, bits: u16, palette: &[[u8; 3]], idx: &[u8]) -> Vec<u8> {
        let row = (w as usize * usize::from(bits)).div_ceil(8);
        let colors = 1usize << bits;
        let entries: u16 = 10;
        let map_at = 8 + 2 + usize::from(entries) * 12 + 4;
        let data_at = map_at + colors * 3 * 2;
        let data_len = row * h as usize;
        let mut b = Vec::new();
        b.extend_from_slice(b"II*\0");
        b.extend_from_slice(&8u32.to_le_bytes());
        b.extend_from_slice(&entries.to_le_bytes());
        let mut entry = |tag: u16, kind: u16, count: u32, value: u32| {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&kind.to_le_bytes());
            b.extend_from_slice(&count.to_le_bytes());
            b.extend_from_slice(&value.to_le_bytes());
        };
        const SHORT: u16 = 3;
        const LONG: u16 = 4;
        entry(256, LONG, 1, w);
        entry(257, LONG, 1, h);
        entry(258, SHORT, 1, u32::from(bits));
        entry(259, SHORT, 1, 1);
        entry(262, SHORT, 1, 3);
        entry(273, LONG, 1, data_at as u32);
        entry(277, SHORT, 1, 1);
        entry(278, LONG, 1, h);
        entry(279, LONG, 1, data_len as u32);
        entry(320, SHORT, (colors * 3) as u32, map_at as u32);
        b.extend_from_slice(&0u32.to_le_bytes());
        for ch in 0..3 {
            for k in 0..colors {
                let v = palette.get(k).map_or(0, |c| u16::from(c[ch]) << 8);
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        for y in 0..h {
            b.extend_from_slice(&pack_row(
                &idx[(y * w) as usize..((y + 1) * w) as usize],
                bits,
                row,
            ));
        }
        b
    }

    /// パレットの BMP（1・4・8 ビット）と TIFF（4・8 ビット）は、ImageIO の indexed の枝で RGB に広げて読み、パレットの
    /// 色がそのまま出る（BMP には固定の段の読み手がない。本番でこの枝を通る経路）。
    #[test]
    fn palette_bmp_and_tiff_expand_the_palette() {
        let _g = shared();
        let set = mac_set();
        let mut rng = kura_engine::determinism::rng(0x7a1e);
        let (w, h) = (37, 9);
        let mut checked = 0;
        for (bits, colors) in [(1u16, 2usize), (4, 16), (8, 200)] {
            let palette: Vec<[u8; 3]> = (0..colors)
                .map(|_| {
                    [
                        rng.next_u32() as u8,
                        rng.next_u32() as u8,
                        rng.next_u32() as u8,
                    ]
                })
                .collect();
            let idx: Vec<u8> = (0..w * h)
                .map(|_| (rng.next_u32() as usize % colors) as u8)
                .collect();
            let want: Vec<u8> = idx.iter().flat_map(|&i| palette[usize::from(i)]).collect();
            let mut files = vec![("bmp", palette_bmp(w, h, bits, &palette, &idx))];
            if bits > 1 {
                files.push(("tiff", palette_tiff(w, h, bits, &palette, &idx)));
            }
            for (format, file) in files {
                let (got, stamp) =
                    decode(&set, &file, format).unwrap_or_else(|e| panic!("{format} {bits}: {e}"));
                assert_eq!(
                    (stamp.codec.as_str(), stamp.tier),
                    (IMAGEIO_DEC, Tier::Platform)
                );
                assert_eq!(
                    (img(&got).px.width(), img(&got).px.height()),
                    (w, h),
                    "{format} {bits}"
                );
                assert!(
                    rows(&got, false) == (PixelFormat::Rgb8, want.clone()),
                    "{format} {bits}: 画素が違います"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 5);
    }

    /// 埋め込みの有効な ICC は元のバイト列のまま運ぶ。向き（eXIf の 6）は adapter が当て、固定の段と同じ画素・大きさになる。
    #[test]
    fn embedded_icc_and_orientation_match_png_rs() {
        let _g = shared();
        let dec = png_decoder();
        let pinned = CodecSet::pinned();
        let base = read(
            &dec,
            &png_bytes(&noise(20, 10, PixelFormat::Rgb8, 3)),
            "png",
        )
        .unwrap();
        let icc = img(&base).meta.icc.clone().unwrap();
        let v = image(
            img(&noise(20, 10, PixelFormat::Rgba8, 4)).px.clone(),
            ImageMeta {
                icc: Some(icc.clone()),
                exif: Some(Arc::new(exif::minimal(6, false))),
                ..ImageMeta::default()
            },
        );
        let png = png_bytes(&v);
        let (want, _) = decode(&pinned, &png, "png").unwrap();
        let got = read(&dec, &png, "png").unwrap();
        assert_eq!(img(&got).meta.icc, Some(icc));
        assert_eq!((img(&got).px.width(), img(&got).px.height()), (10, 20));
        assert_eq!(img(&got).px, img(&want).px);
    }

    /// TIFF・BMP（固定の段のデコーダがない。ImageIO が書いた可逆のファイル）は Platform の段で読み、元の PNG の固定の段の
    /// 読み取りとビット一致する。
    #[test]
    fn tiff_and_bmp_match_the_source() {
        let _g = shared();
        let set = mac_set();
        let pinned = CodecSet::pinned();
        for (fmt, targets) in [
            (PixelFormat::Gray8, &["tiff"][..]),
            (PixelFormat::GrayA8, &["tiff"]),
            (PixelFormat::Rgb8, &["tiff", "bmp"]),
            (PixelFormat::Rgba8, &["tiff", "bmp"]),
            (PixelFormat::Gray16, &["tiff"]),
            (PixelFormat::Rgba16, &["tiff"]),
        ] {
            let png = png_bytes(&noise(70, 300, fmt, 5));
            let (want, _) = decode(&pinned, &png, "png").unwrap();
            for &t in targets {
                let file = sips(&png, "png", t, if t == "tiff" { "tif" } else { "bmp" });
                let (got, stamp) = decode(&set, &file, t).unwrap();
                assert_eq!(
                    (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
                    (IMAGEIO_DEC, Tier::Platform, false)
                );
                let (gf, gr) = rows(&got, false);
                let (wf, wr) = rows(&want, gf.has_alpha());
                assert_eq!(gf, wf, "{fmt:?} {t}");
                assert!(gr == wr, "{fmt:?} {t}: 画素が違います");
            }
        }
    }

    /// HEIC（ImageIO が書いた非可逆のもの）：Platform の段で読み、記録に環境が入り、元に近い。同じプロセスでは毎回同じ値。
    /// HEIC と HEIF は互いの UTI を受け、AVIF としては読まない。
    #[test]
    fn heic_reads_on_the_platform_tier() {
        let _g = shared();
        let set = mac_set();
        let src = gradient(300, 200);
        let heic = heic();
        let (v, stamp) = decode(&set, &heic, "heic").unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
            (IMAGEIO_DEC, Tier::Platform, false)
        );
        let env = stamp.env.clone().unwrap();
        assert_eq!(env.tool, "macOS");
        assert!(env.components.contains_key("imageio"), "{env:?}");
        let (fmt, got) = rows(&v, false);
        assert_eq!(fmt, PixelFormat::Rgb8);
        let db = psnr(&got, &rows(&src, false).1);
        assert!(db > 30.0, "{db} dB");
        let (_, again) = decode(&set, &heic, "heic").unwrap();
        assert_eq!(again.value_digest, stamp.value_digest);
        let (_, heif) = decode(&set, &heic, "heif").unwrap();
        assert_eq!(heif.value_digest, stamp.value_digest);
        assert!(matches!(
            decode(&set, &heic, "avif"),
            Err(EngineError::Unsupported(_))
        ));
    }

    /// HEIC のシーケンスはキャンバスいっぱいのフレームの列（表示時間・繰り返しの回数つき）。
    #[test]
    fn heic_sequences_become_full_frames() {
        let _g = shared();
        let (v, stamp) = decode(&mac_set(), &fixture("heics-3x24x16.heic"), "heic").unwrap();
        assert_eq!(stamp.tier, Tier::Platform);
        let Value::Frames(fr) = v else {
            panic!("フレームの列ではありません")
        };
        assert!(fr.is_full());
        assert_eq!(fr.loop_count, 1);
        assert_eq!(
            fr.frames()
                .iter()
                .map(|f| f.duration_ms)
                .collect::<Vec<_>>(),
            [250, 500, 500]
        );
        for f in fr.frames() {
            assert_eq!(
                (f.px.width(), f.px.height(), f.px.format()),
                (24, 16, PixelFormat::Rgb8)
            );
        }
        assert_ne!(fr.frames()[0].px, fr.frames()[1].px);
    }

    /// 関連付けたアルファの TIFF（8・16 ビット）は、プレマルチプライドのまま受け取り、エンジンの整数の式でストレートに
    /// 戻す。16 ビットのフィクスチャには、vImage の戻しが式と 1 違う組を入れてある（8 ビットでは全組が同じ）。
    #[test]
    fn premultiplied_tiff_is_unpremultiplied_by_the_engine() {
        let _g = shared();
        let set = mac_set();
        for (name, fmt) in [
            ("tiff-premultiplied", PixelFormat::Rgba8),
            ("tiff-premultiplied16", PixelFormat::Rgba16),
        ] {
            let (v, _) = decode(&set, &fixture(&format!("{name}.tif")), "tiff").unwrap();
            let mut want = fixture(&format!("{name}.rgba"));
            unpremultiply_rows(fmt, &mut want);
            assert!(rows(&v, false) == (fmt, want), "{name}: 画素が違います");
        }
    }

    /// HDR・浮動小数の画像は扱えない。
    #[test]
    fn float_images_are_unsupported() {
        let _g = shared();
        match decode(&mac_set(), &fixture("tiff-float.tif"), "tiff") {
            Err(EngineError::Unsupported(m)) => assert!(m.contains("HDR"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    /// 固定の段が断った変種は ImageIO に回る。CMYK の JPEG は sRGB に変換して読み（ICC は sRGB のもの）、ふつうの JPEG は
    /// 固定の段が読む。16 ビットの CMYK の PSD は ImageIO も展開できず（描いても 0 のまま。vImage は 0 で埋めて成功を返す）、
    /// 取り出したデータが NULL なので壊れたファイルになる（黙って黒い画像にしない）。
    #[test]
    fn cmyk_variants_fall_through_to_imageio() {
        let _g = shared();
        let set = mac_set();
        let (v, stamp) = decode(&set, &engine_fixture("jpeg-cmyk.jpg"), "jpeg").unwrap();
        assert_eq!(
            (stamp.codec.as_str(), stamp.tier),
            (IMAGEIO_DEC, Tier::Platform)
        );
        assert_eq!(img(&v).px.format(), PixelFormat::Rgb8);
        assert!(img(&v).meta.icc.is_some());
        let (_, stamp) = decode(&set, &engine_fixture("jpeg-422.jpg"), "jpeg").unwrap();
        assert_eq!(stamp.tier, Tier::Pinned);
        match decode(&set, &engine_fixture("psd-cmyk.psd"), "psd") {
            Err(EngineError::Decode { codec, message }) => {
                assert_eq!(codec, IMAGEIO_DEC);
                assert!(message.contains("展開できません"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// 上限は ImageIO が展開する前に効く（展開の回数が増えない）。ICC の予算・入力の長さも同じ。
    #[test]
    fn limits_are_checked_before_imageio_decodes() {
        let _g = exclusive();
        let set = mac_set();
        let heic = heic();
        let (live, decodes) = stats();
        for limits in [
            Limits {
                max_pixels: 1_000,
                ..Limits::default()
            },
            Limits {
                max_meta_bytes: 100,
                ..Limits::default()
            },
            Limits {
                max_value_bytes: 10,
                ..Limits::default()
            },
        ] {
            assert!(
                matches!(
                    decode_with(&set, &heic, "heic", &limits, &NoMonitor),
                    Err(EngineError::Limit(_))
                ),
                "{limits:?}"
            );
        }
        assert_eq!(stats(), (live, decodes));
        decode(&set, &heic, "heic").unwrap();
        assert_eq!(stats(), (live, decodes + 1));
    }

    /// 中止：帯・フレームの合間（と展開の前・展開を待つあいだ）の、1〜4 回目の確認のどこでも止まる。
    #[test]
    fn cancels_between_bands_and_frames() {
        let _g = shared();
        let set = mac_set();
        let tiff = sips(
            &png_bytes(&noise(20, 1_100, PixelFormat::Gray8, 2)),
            "png",
            "tiff",
            "tif",
        );
        let heics = fixture("heics-3x24x16.heic");
        for after in 1..=4 {
            for (bytes, format) in [(&tiff, "tiff"), (&heics, "heic")] {
                assert!(
                    matches!(
                        decode_with(
                            &set,
                            bytes,
                            format,
                            &Limits::default(),
                            &CancelAfter::new(after)
                        ),
                        Err(EngineError::Canceled)
                    ),
                    "{format} {after}"
                );
            }
        }
        // 展開を待つあいだの中止で見捨てたスレッドは、展開を終えるとハンドルを閉じる（生きているハンドルの数を照らす
        // テストと重ならないよう、戻るまで待つ）
        wait_for_abandoned(&IMAGEIO_WATCHDOG);
    }

    /// 見捨てたスレッドが（止まっていなければ）戻るまで待つ。
    fn wait_for_abandoned(w: &Watchdog) {
        let start = std::time::Instant::now();
        while w.stalled() > 0 {
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "見捨てたスレッドが戻りません"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 壊れた入力（途中で切った・変異させた HEIC・TIFF、ゴミ）でプロセスが落ちず、ハンドルが漏れない。切った HEIC は、
    /// 短いもの（10% まで）は開く段で「画像がありません」になり、20% 以上のものは開けても展開に失敗する（取り出した
    /// データが NULL。黙って黒い画像にしない）。
    #[test]
    fn broken_inputs_fail_without_crashing() {
        let _g = exclusive();
        let (live, _) = stats();
        let set = mac_set();
        let heic = heic();
        let tiff = sips(
            &png_bytes(&noise(40, 30, PixelFormat::Rgba8, 1)),
            "png",
            "tiff",
            "tif",
        );
        for pct in [1, 5, 10, 25, 50, 75, 90, 99] {
            let _ = decode(&set, &heic[..heic.len() * pct / 100], "heic");
            let _ = decode(&set, &tiff[..tiff.len() * pct / 100], "tiff");
        }
        let broken = |bytes: &[u8], want: &str| match decode(&set, bytes, "heic") {
            Err(EngineError::Decode { codec, message }) => {
                assert_eq!(codec, IMAGEIO_DEC);
                assert!(message.contains(want), "{} バイト: {message}", bytes.len());
            }
            other => panic!(
                "{} バイト: {:?}",
                bytes.len(),
                other.map(|(v, s)| (v.shape(), s.codec))
            ),
        };
        broken(&heic[..heic.len() / 10], "画像がありません");
        for pct in [20, 25, 50, 75, 90, 99] {
            broken(
                &heic[..heic.len() * pct / 100],
                "画像を展開できません（壊れたファイル）",
            );
        }
        assert!(decode(&set, b"not an image at all", "heic").is_err());
        assert!(decode(&set, &[], "tiff").is_err());
        let mut rng = kura_engine::determinism::rng(0x9e1f);
        for (seed, format) in [(&heic, "heic"), (&tiff, "tiff")] {
            for _ in 0..150 {
                let mut b = seed.clone();
                for _ in 0..1 + rng.next_u32() % 8 {
                    let i = rng.next_u32() as usize % b.len();
                    b[i] = rng.next_u32() as u8;
                }
                if rng.next_u32().is_multiple_of(4) {
                    b.truncate(rng.next_u32() as usize % b.len());
                }
                let _ = decode(&set, &b, format);
            }
        }
        assert_eq!(stats().0, live);
    }

    /// 8 スレッドから同時に読んでも値は同じで、ハンドルは漏れない。
    #[test]
    fn eight_threads_decode_concurrently() {
        let _g = exclusive();
        let (live, _) = stats();
        let set = Arc::new(mac_set());
        let heic = Arc::new(heic());
        let (_, want) = decode(&set, &heic, "heic").unwrap();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (set, heic, digest) = (set.clone(), heic.clone(), want.value_digest);
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        let (_, s) = decode(&set, &heic, "heic").unwrap();
                        assert_eq!(s.value_digest, digest);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(stats().0, live);
    }

    /// 展開が止まる HEIC（`heic-decode-hangs.heic`）を、テストの見張り（上限 `max`）と時間の上限 1 秒で読む decoder。
    fn stalling_decoder(
        max: usize,
        timeout: fn(u64) -> Duration,
    ) -> (PlatformImageDecoder<ImageIoBackend>, &'static Watchdog) {
        let w: &'static Watchdog = Box::leak(Box::new(Watchdog::new(max)));
        let backend = ImageIoBackend::new(&codec_caps().unwrap()).with_watchdog(w, timeout);
        (PlatformImageDecoder::new(backend), w)
    }

    /// 展開の時間の上限は、基本の 20 秒にフレームの画素 100 万ごとに 0.25 秒を足す（定数は docs の材料）。
    #[test]
    fn begin_timeout_grows_with_the_frame() {
        assert_eq!(begin_timeout(0), Duration::from_secs(20));
        assert_eq!(begin_timeout(1), Duration::from_millis(20_250));
        assert_eq!(begin_timeout(48_771_072), Duration::from_millis(32_250));
        assert_eq!(begin_timeout(1 << 28), Duration::from_millis(87_250));
        // あふれない（画素 100 万の数は u32 で飽和）
        assert_eq!(
            begin_timeout(u64::MAX),
            Duration::from_secs(20) + Duration::from_millis(250) * u32::MAX
        );
    }

    /// 展開が戻らない HEIC（Apple 側の不具合。sips も止まる）：時間切れで壊れたファイル（`Decode`）になり、展開のスレッドと
    /// ハンドルを見捨てる（閉じない）。そのあとも別の画像は読め、見捨てたスレッドが上限に達したら Platform の画像の段を
    /// 断る（`Unsupported`。開く前に）。止まらない環境（不具合の直った OS・別のデコーダの仮想マシン）では、時間切れは
    /// 確かめずに失敗することだけを見る。
    #[test]
    fn stalled_decode_times_out_and_is_abandoned() {
        let _g = exclusive();
        let (dec, w) = stalling_decoder(2, |_| Duration::from_secs(1));
        let hang = fixture("heic-decode-hangs.heic");
        // 形（開くだけ）は読める
        let shape = dec
            .shape(
                &mut Cursor::new(hang.clone()),
                "heic",
                &DecodeOptions::default(),
                &Limits::default(),
            )
            .unwrap();
        assert!(
            matches!(shape, kura_engine::Shape::Image { .. }),
            "{shape:?}"
        );
        let (live, _) = stats();
        for stalled in 1..=2 {
            let start = std::time::Instant::now();
            match read(&dec, &hang, "heic") {
                Err(EngineError::Decode { codec, message }) if message.contains("終わりません") =>
                {
                    assert_eq!(codec, IMAGEIO_DEC);
                    assert!(message.contains("1.0 秒"), "{message}");
                }
                other => {
                    // この環境では止まらない
                    assert!(other.is_err(), "{other:?}");
                    eprintln!("止まらない環境です（時間切れは確かめません）: {other:?}");
                    return;
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{:?}",
                start.elapsed()
            );
            assert_eq!(w.stalled(), stalled);
            // 見捨てたハンドルは閉じない
            assert_eq!(stats().0, live + stalled as i64);
            if stalled == 1 {
                let v = read(&dec, &heic(), "heic").unwrap();
                assert_eq!(rows(&v, false).0, PixelFormat::Rgb8);
            }
        }
        assert_eq!(w.exhausted(), Some(2));
        let before = stats();
        for bytes in [heic(), hang] {
            match read(&dec, &bytes, "heic") {
                Err(EngineError::Unsupported(m)) => assert!(m.contains("2 本戻っていない"), "{m}"),
                other => panic!("{other:?}"),
            }
        }
        // 断るのは開く前（Swift を呼ばない）
        assert_eq!(stats(), before);
    }

    /// 展開が戻らないあいだに中止を頼まれたら、時間切れ（ここでは 10 分）を待たずにスレッドを見捨てて `Canceled` を返す。
    #[test]
    fn cancel_abandons_a_stalled_decode() {
        let _g = exclusive();
        let (dec, w) = stalling_decoder(2, |_| Duration::from_secs(600));
        struct CancelAt(std::time::Instant);
        impl Monitor for CancelAt {
            fn canceled(&self) -> bool {
                std::time::Instant::now() >= self.0
            }
        }
        let start = std::time::Instant::now();
        let mon = CancelAt(start + Duration::from_millis(500));
        let got = dec.decode(
            &mut Cursor::new(fixture("heic-decode-hangs.heic")),
            "heic",
            &DecodeOptions::default(),
            &Limits::default(),
            &mon,
        );
        if start.elapsed() < Duration::from_millis(500) {
            // この環境では止まらない（中止の前に失敗した）
            assert!(got.is_err(), "{:?}", got.map(|v| v.shape()));
            eprintln!("止まらない環境です（中止は確かめません）");
            return;
        }
        assert!(
            matches!(got, Err(EngineError::Canceled)),
            "{:?}",
            got.map(|v| v.shape())
        );
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(w.stalled(), 1);
    }
}
