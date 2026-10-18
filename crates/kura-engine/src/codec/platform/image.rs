//! Platform の画像の受け口：backend（ImageIO）が 1 度だけ展開した画素を帯ごとに受け取り、値にする。
//!
//! 約束（adapter の側。識別子の版に入る）：
//! - backend は画素を kura の並び（グレー／RGB、＋α、8 ビットか 16 ビットのリトルエンディアン。パレットは RGB(A) に広げる）
//!   で、色空間を変えずに書く。アルファはストレートか、プレマルチプライドのまま（[`PlatformAlpha`]）。プレマルチプライドは
//!   adapter が [`unpremultiply_u8`]・[`unpremultiply_u16`] の整数の式でストレートに戻す。
//! - 向きは backend が当てずに報告し（EXIF の Orientation・HEIF の irot・imir）、adapter が `imaging::orient`（整数だけ）で
//!   当てる。形も向きを当てた大きさ。運ぶ EXIF の向きは 1 にする。
//! - 上限は backend が展開する前に確かめる（形 → `Limits::check` → ICC・EXIF の予算 → 展開 → 確保）。展開した実際の大きさ・
//!   形式が開いたときの形と違えば壊れたファイル（`Decode`）。中止は帯（256 行）・フレームの合間で見る。
//! - HDR・浮動小数の画像は `Unsupported`（「HDR の画像は編集できません」）。フレームの列はキャンバスいっぱい
//!   （HEICS・AVIS。合成は backend のもの）。形式の表でフレームの列を値にしない形式（TIFF の複数のページ・JPEG・PSD）は
//!   最初のフレームだけを読む。ICC は backend の色空間のもの（推定した sRGB なども）をそのまま運ぶ。

use std::sync::Arc;

use crate::codec::platform::BackendError;
use crate::codec::symph::read_all;
use crate::codec::{CodecEnv, Decoder, ReadSeek, Stage, Tier, exif, format_by_id, try_vec};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::imaging::orient::{orient, oriented_size};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::shape::{Dim, Fullness, Shape, ValueKind};
use crate::value::{Frames, Image, ImageMeta, PixelFormat, Raster, RasterBuilder, TILE, Value};

/// backend が書く画素のアルファの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformAlpha {
    /// アルファがない（形式もアルファを持たない）。
    None,
    /// ストレート（ImageIO の PNG・HEIC・AVIF・WebP など）。
    Straight,
    /// プレマルチプライド（関連付けたアルファの TIFF など）。adapter がストレートに戻す。
    Premultiplied,
}

/// 開いた画像の形（展開する前・向きを当てる前。ImageIO の属性と、展開しない `CGImage` の属性から）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo {
    /// 画素の幅（向きを当てる前）。
    pub width: u32,
    /// 画素の高さ（向きを当てる前）。
    pub height: u32,
    /// backend が書く画素の形式（`Indexed8` 以外）。
    pub format: PixelFormat,
    pub alpha: PlatformAlpha,
    /// EXIF の向き（1〜8。範囲の外は 1 とみなす）。
    pub orientation: u8,
    /// フレームの数（1 以上。2 以上ならキャンバスいっぱいのフレームの列）。
    pub frames: u32,
    /// 繰り返しの回数（0 は無限。フレームの列のときだけ使う）。
    pub loop_count: u32,
    /// ICC プロファイルの長さ（0 はなし）。
    pub icc_len: u64,
    /// EXIF（TIFF の形）の長さ（0 はなし）。
    pub exif_len: u64,
    /// HDR・浮動小数の画像（PQ・HLG・EXR・浮動小数の TIFF など）。adapter は `Unsupported` にする。
    pub hdr: bool,
}

/// 1 フレームを展開した結果（実際の大きさ・形式。開いたときの形と照らす）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub alpha: PlatformAlpha,
    /// 表示時間（ミリ秒。フレームの列のときだけ使う）。
    pub duration_ms: u32,
}

/// 画像の backend（kura-mac の ImageIO・テストの偽物）。
pub trait ImageBackend: Send + Sync {
    /// 開いたもの（Swift のハンドルの安全な包み）。`Drop` で 1 度だけ閉じる。
    type Handle: ImageHandle;
    /// コーデックの識別子（例 `imageio-dec/1`）。
    fn id(&self) -> &'static str;
    /// この形式を扱うか（形式の表の ID。固定の段が断った変種を受けるなら `jpeg`・`psd` も）。
    fn can_decode(&self, format: &str) -> bool;
    /// 環境の記録（OS・ImageIO の版。組み立てたときに調べた値）。
    fn env(&self) -> Option<CodecEnv> {
        None
    }
    /// 入力のバイト列を開いて、ヘッダだけ読む（展開しない）。`bytes` はハンドルが持ってよい（閉じるまで生かす）。
    /// 扱えない形式・変種は `Unsupported`。
    fn open(&self, bytes: Arc<Vec<u8>>, format: &str) -> Result<Self::Handle, BackendError>;
}

/// 開いた画像。
pub trait ImageHandle {
    /// 開いたときの形。
    fn info(&self) -> ImageInfo;
    /// フレーム `index` を全体で 1 度だけ展開する（前のフレームの展開は捨ててよい）。実際の大きさ・形式を返す。
    /// 1 回の展開の中では止められない（Swift の中でループを回すなら `mon` を中止のコールバックにする）。
    fn begin(&mut self, index: u32, mon: &dyn Monitor) -> Result<DecodedFrame, BackendError>;
    /// 展開したフレームの行 `y0..y0 + rows` を `out`（`rows × row_bytes` バイトちょうど）に、行を詰めて書く。
    /// backend の側でも範囲（`y0 + rows ≤ 高さ`）・長さ（`row_bytes` = 幅 × 画素の大きさ）を確かめ、違えば `Invalid`。
    fn read_rows(
        &mut self,
        y0: u32,
        rows: u32,
        row_bytes: usize,
        out: &mut [u8],
    ) -> Result<(), BackendError>;
    /// ICC プロファイルを `out`（`icc_len` バイトちょうど）に写す。
    fn copy_icc(&mut self, out: &mut [u8]) -> Result<(), BackendError>;
    /// EXIF（TIFF の形）を `out`（`exif_len` バイトちょうど）に写す。
    fn copy_exif(&mut self, out: &mut [u8]) -> Result<(), BackendError>;
}

/// 画像の backend を `Decoder` にする adapter（`Tier::Platform`・再現を約束しない）。
pub struct PlatformImageDecoder<B> {
    backend: B,
}

impl<B: ImageBackend> PlatformImageDecoder<B> {
    /// backend を包む。
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// 包んだ backend。
    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn open(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        limits: &Limits,
    ) -> Result<(B::Handle, ImageInfo), EngineError> {
        let id = self.backend.id();
        let bytes = Arc::new(read_all(src, limits)?);
        let h = self
            .backend
            .open(bytes, format)
            .map_err(|e| e.into_engine(id, Stage::Decode))?;
        let mut info = checked_info(id, h.info())?;
        // フレームの列を値にしない形式（形式の表の decodes_to に Frames がない：TIFF の複数のページ・JPEG・PSD など）は
        // 最初のフレームだけを読む
        if !format_by_id(format).is_some_and(|f| f.decodes_to.contains(&ValueKind::Frames)) {
            info.frames = 1;
        }
        Ok((h, info))
    }
}

/// 開いたときの形を確かめる（backend の約束が破れていれば内部の誤り、壊れていれば `Decode`、HDR は `Unsupported`）。
fn checked_info(id: &str, mut info: ImageInfo) -> Result<ImageInfo, EngineError> {
    if info.hdr {
        return Err(EngineError::Unsupported(format!(
            "{id}: HDR・浮動小数の画像は編集できません"
        )));
    }
    if info.width == 0 || info.height == 0 || info.frames == 0 {
        return Err(EngineError::Decode {
            codec: id.to_string(),
            message: format!(
                "画像の大きさが 0 です（{}×{}・{} フレーム）",
                info.width, info.height, info.frames
            ),
        });
    }
    if info.format == PixelFormat::Indexed8 {
        return Err(EngineError::Internal(format!(
            "{id}: Platform の画像はパレットを RGB(A) に広げて書きます"
        )));
    }
    if info.format.has_alpha() != (info.alpha != PlatformAlpha::None) {
        return Err(EngineError::Internal(format!(
            "{id}: 画素の形式とアルファの扱いが合いません（{:?}・{:?}）",
            info.format, info.alpha
        )));
    }
    if !(1..=8).contains(&info.orientation) {
        info.orientation = 1;
    }
    Ok(info)
}

/// 向きを当てた形。
fn shape_of(info: &ImageInfo) -> Shape {
    let (w, h) = oriented_size(info.width, info.height, info.orientation);
    let (w, h) = (Dim::Exact(u64::from(w)), Dim::Exact(u64::from(h)));
    if info.frames == 1 {
        Shape::Image {
            w,
            h,
            fmt: info.format,
        }
    } else {
        Shape::Frames {
            count: Dim::Exact(u64::from(info.frames)),
            w,
            h,
            fmt: info.format,
            full: Fullness::Full,
        }
    }
}

/// ICC・EXIF の合計を `max_meta_bytes` で確かめる。
fn check_meta_budget(info: &ImageInfo, limits: &Limits) -> Result<(), EngineError> {
    match info.icc_len.checked_add(info.exif_len) {
        Some(n) if n <= limits.max_meta_bytes => Ok(()),
        _ => Err(EngineError::Limit(format!(
            "ICC・EXIF が大きすぎます（{} + {} バイト）",
            info.icc_len, info.exif_len
        ))),
    }
}

impl<B: ImageBackend> Decoder for PlatformImageDecoder<B> {
    fn id(&self) -> &'static str {
        self.backend.id()
    }

    fn tier(&self) -> Tier {
        Tier::Platform
    }

    fn reproducible(&self) -> bool {
        false
    }

    fn env(&self) -> Option<CodecEnv> {
        self.backend.env()
    }

    fn can_decode(&self, format: &str) -> bool {
        self.backend.can_decode(format)
    }

    fn shape(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
    ) -> Result<Shape, EngineError> {
        let (_h, info) = self.open(src, format, limits)?;
        check_meta_budget(&info, limits)?;
        Ok(shape_of(&info))
    }

    fn decode(
        &self,
        src: &mut dyn ReadSeek,
        format: &str,
        _opts: &DecodeOptions,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        let id = self.backend.id();
        let (mut h, info) = self.open(src, format, limits)?;
        // 展開・確保の前に上限で判断する
        limits.check(&shape_of(&info)).map_err(EngineError::Limit)?;
        check_meta_budget(&info, limits)?;
        let mut frames: Vec<(Raster, u32)> = Vec::new();
        frames
            .try_reserve_exact(info.frames as usize)
            .map_err(|_| EngineError::Limit("フレームの表を確保できません".into()))?;
        let mut progress = Progress {
            done: 0,
            total: u64::from(info.height).saturating_mul(u64::from(info.frames)),
        };
        for index in 0..info.frames {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let got = h
                .begin(index, mon)
                .map_err(|e| e.into_engine(id, Stage::Decode))?;
            if (got.width, got.height, got.format, got.alpha)
                != (info.width, info.height, info.format, info.alpha)
            {
                return Err(EngineError::Decode {
                    codec: id.to_string(),
                    message: format!(
                        "ヘッダと中身の大きさが合いません（{}×{} {:?} を {}×{} {:?} として読みました）",
                        info.width, info.height, info.format, got.width, got.height, got.format
                    ),
                });
            }
            let px = read_frame(id, &mut h, &info, mon, &mut progress)?;
            frames.push((orient(&px, info.orientation, mon)?, got.duration_ms));
        }
        let icc = copy_blob(id, info.icc_len, |out| h.copy_icc(out))?;
        let exif = copy_blob(id, info.exif_len, |out| h.copy_exif(out))?
            .map(|e| exif::with_orientation_reset(exif::strip_header(&e)));
        drop(h);
        let meta = ImageMeta {
            icc: icc.map(Arc::new),
            exif: exif.map(Arc::new),
            ..ImageMeta::default()
        };
        if info.frames == 1 {
            let Some((px, _)) = frames.pop() else {
                return Err(EngineError::Internal("フレームがありません".into()));
            };
            Ok(Value::Image(Arc::new(Image { px, meta })))
        } else {
            Ok(Value::Frames(Arc::new(Frames::full(
                frames,
                meta,
                info.loop_count,
            )?)))
        }
    }
}

/// 進捗（行の数）。
struct Progress {
    done: u64,
    total: u64,
}

/// 展開したフレームを帯ごとに受け取り、プレマルチプライドならストレートに戻して、タイルに組む。
fn read_frame<H: ImageHandle>(
    id: &str,
    h: &mut H,
    info: &ImageInfo,
    mon: &dyn Monitor,
    progress: &mut Progress,
) -> Result<Raster, EngineError> {
    let mut b = RasterBuilder::new(info.width, info.height, info.format, None)?;
    let row_len = b.row_len();
    let band_rows = TILE.min(info.height);
    let band_len = row_len
        .checked_mul(band_rows as usize)
        .ok_or_else(|| EngineError::Limit("画像が大きすぎます".into()))?;
    let mut band = try_vec(band_len, 0u8)?;
    let mut y0 = 0u32;
    while y0 < info.height {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let rows = band_rows.min(info.height - y0);
        let buf = &mut band[..row_len * rows as usize];
        h.read_rows(y0, rows, row_len, buf)
            .map_err(|e| e.into_engine(id, Stage::Decode))?;
        if info.alpha == PlatformAlpha::Premultiplied {
            unpremultiply_rows(info.format, buf);
        }
        for row in buf.chunks_exact(row_len) {
            b.push_row(row)?;
        }
        y0 += rows;
        progress.done = progress.done.saturating_add(u64::from(rows));
        mon.progress(progress.done, progress.total);
    }
    b.finish()
}

/// 長さを確かめて確保し、backend に写させる（長さ 0 は `None`）。
fn copy_blob(
    id: &str,
    len: u64,
    copy: impl FnOnce(&mut [u8]) -> Result<(), BackendError>,
) -> Result<Option<Vec<u8>>, EngineError> {
    if len == 0 {
        return Ok(None);
    }
    let n =
        usize::try_from(len).map_err(|_| EngineError::Limit("ICC・EXIF が大きすぎます".into()))?;
    let mut buf = try_vec(n, 0u8)?;
    copy(&mut buf).map_err(|e| e.into_engine(id, Stage::Decode))?;
    Ok(Some(buf))
}

/// 8 ビットのプレマルチプライドの色 `c`・アルファ `a` をストレートに戻す：`a = 0` は 0、ほかは
/// `⌊(2·min(c, a)·255 + a) / (2a)⌋`（vImage の `(min(c, a)·255 + ⌊a/2⌋) / a` とすべての組で同じ値。凍結する）。
pub fn unpremultiply_u8(c: u8, a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    let (c, a) = (u32::from(c.min(a)), u32::from(a));
    // c ≤ a なので結果は 255 以下
    ((c * 510 + a) / (2 * a)) as u8
}

/// 16 ビットのプレマルチプライドの色をストレートに戻す：`a = 0` は 0、ほかは `⌊(2·min(c, a)·65535 + a) / (2a)⌋`
/// （8 ビットと同じ形の式。凍結する）。
pub fn unpremultiply_u16(c: u16, a: u16) -> u16 {
    if a == 0 {
        return 0;
    }
    let (c, a) = (u64::from(c.min(a)), u64::from(a));
    // c ≤ a なので結果は 65535 以下
    ((c * 131_070 + a) / (2 * a)) as u16
}

/// 行を詰めた画素（`fmt` の並び。16 ビットはリトルエンディアン）を、その場でストレートに戻す。アルファのない形式は
/// そのまま。アルファは変えない。
pub fn unpremultiply_rows(fmt: PixelFormat, buf: &mut [u8]) {
    match fmt {
        PixelFormat::GrayA8 | PixelFormat::Rgba8 => {
            let n = fmt.bytes_per_pixel();
            for px in buf.chunks_exact_mut(n) {
                let a = px[n - 1];
                for c in &mut px[..n - 1] {
                    *c = unpremultiply_u8(*c, a);
                }
            }
        }
        PixelFormat::GrayA16 | PixelFormat::Rgba16 => {
            let n = fmt.bytes_per_pixel();
            for px in buf.chunks_exact_mut(n) {
                let a = u16::from_le_bytes([px[n - 2], px[n - 1]]);
                for c in px[..n - 2].chunks_exact_mut(2) {
                    let v = unpremultiply_u16(u16::from_le_bytes([c[0], c[1]]), a);
                    c.copy_from_slice(&v.to_le_bytes());
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::codec::CodecSet;
    use crate::monitor::{CancelAfter, NoMonitor};
    use crate::testgen::InputGen;
    use crate::testing::platform::{FakeImage, FakeImageBackend, ImageFault, premultiply_rows};

    fn decode(
        b: FakeImageBackend,
        limits: &Limits,
        mon: &dyn Monitor,
    ) -> Result<Value, EngineError> {
        PlatformImageDecoder::new(b).decode(
            &mut Cursor::new(vec![1u8, 2, 3]),
            "heic",
            &DecodeOptions::default(),
            limits,
            mon,
        )
    }

    fn noise(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Raster {
        match (InputGen::Noise { w, h, fmt, seed }).generate() {
            Ok(Value::Image(img)) => img.px.clone(),
            other => panic!("{other:?}"),
        }
    }

    /// 8 ビットの戻しは、すべての組で vImage の文書の式（`(min(c, a)·255 + ⌊a/2⌋) / a`）と同じで、c ≤ a なら
    /// `⌊(2·c·255 + a) / (2a)⌋` そのもの。
    #[test]
    fn unpremultiply_u8_matches_vimage_for_all_pairs() {
        for a in 0..=255u32 {
            for c in 0..=255u32 {
                let got = u32::from(unpremultiply_u8(c as u8, a as u8));
                let vimage = if a == 0 {
                    0
                } else {
                    (c.min(a) * 255 + a / 2) / a
                };
                assert_eq!(got, vimage, "c={c} a={a}");
                if a > 0 && c <= a {
                    assert_eq!(got, (2 * c * 255 + a) / (2 * a), "c={c} a={a}");
                }
            }
        }
        assert_eq!(unpremultiply_u8(200, 255), 200);
        assert_eq!(unpremultiply_u8(9, 0), 0);
        assert_eq!(unpremultiply_u8(255, 1), 255);
    }

    /// 16 ビットの戻し：端の値と、ChaCha8 の組で、同じ形の式（u128 で計算）と一致し、範囲に収まり、c について単調。
    /// ストレートの値を丸めてプレマルチプライドにしたものを戻すと、元の値の近く（誤差は 65535 / (2a) + 1 以下）。
    #[test]
    fn unpremultiply_u16_properties() {
        use rand_chacha::rand_core::Rng;
        let reference = |c: u16, a: u16| -> u16 {
            if a == 0 {
                0
            } else {
                let (c, a) = (u128::from(c.min(a)), u128::from(a));
                ((2 * c * 65_535 + a) / (2 * a)) as u16
            }
        };
        let mut rng = crate::determinism::rng(0x5eed_u64);
        let mut pairs: Vec<(u16, u16)> = vec![
            (0, 0),
            (1, 0),
            (65_535, 65_535),
            (1, 1),
            (65_535, 1),
            (32_767, 65_535),
            (1, 65_535),
        ];
        for _ in 0..200_000 {
            let w = rng.next_u32();
            pairs.push(((w & 0xFFFF) as u16, (w >> 16) as u16));
        }
        for (c, a) in pairs {
            let got = unpremultiply_u16(c, a);
            assert_eq!(got, reference(c, a), "c={c} a={a}");
            if a > 0 && c < a {
                assert!(unpremultiply_u16(c + 1, a) >= got);
            }
            if a == 65_535 {
                assert_eq!(got, c);
            }
            if a > 0 {
                let straight = c;
                let pre = ((u32::from(straight) * u32::from(a) + 32_767) / 65_535) as u16;
                let back = unpremultiply_u16(pre, a);
                let tol = 65_535 / (2 * u32::from(a)) + 1;
                assert!(
                    u32::from(back.abs_diff(straight)) <= tol,
                    "s={straight} a={a} back={back}"
                );
            }
        }
    }

    /// 帯の戻しは画素ごとの式と同じ（アルファは変えず、アルファのない形式はそのまま）。
    #[test]
    fn unpremultiply_rows_by_format() {
        let mut ga8 = vec![100, 200, 7, 0, 255, 255];
        unpremultiply_rows(PixelFormat::GrayA8, &mut ga8);
        assert_eq!(ga8, [unpremultiply_u8(100, 200), 200, 0, 0, 255, 255]);
        let mut rgba16 = Vec::new();
        for v in [1000u16, 2000, 3000, 4000] {
            rgba16.extend_from_slice(&v.to_le_bytes());
        }
        unpremultiply_rows(PixelFormat::Rgba16, &mut rgba16);
        let got: Vec<u16> = rgba16
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(
            got,
            [
                unpremultiply_u16(1000, 4000),
                unpremultiply_u16(2000, 4000),
                unpremultiply_u16(3000, 4000),
                4000
            ]
        );
        let mut rgb = vec![1, 2, 3];
        unpremultiply_rows(PixelFormat::Rgb8, &mut rgb);
        assert_eq!(rgb, [1, 2, 3]);
    }

    /// ストレートの画素はそのまま値になり、記録は Platform・再現を約束しない・backend の環境（CodecSet を通して）。
    #[test]
    fn straight_pixels_become_the_value() {
        for fmt in PixelFormat::ALL
            .into_iter()
            .filter(|f| *f != PixelFormat::Indexed8)
        {
            let px = noise(300, 270, fmt, 7);
            let alpha = if fmt.has_alpha() {
                PlatformAlpha::Straight
            } else {
                PlatformAlpha::None
            };
            let b = FakeImageBackend::new(FakeImage::still(&px, alpha));
            let counters = b.counters();
            let mut set = CodecSet::pinned();
            set.add_decoder(Arc::new(PlatformImageDecoder::new(b)))
                .unwrap();
            let (v, stamp) = set
                .decode(
                    &mut Cursor::new(vec![0u8; 8]),
                    "heic",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .unwrap();
            let Value::Image(img) = &v else {
                panic!("{v:?}")
            };
            assert_eq!(img.px, px, "{fmt:?}");
            assert_eq!(
                (stamp.codec.as_str(), stamp.tier, stamp.reproducible),
                ("fake-imageio-dec/1", Tier::Platform, false)
            );
            assert_eq!(stamp.env, Some(FakeImageBackend::default_env()));
            // 開いたハンドルはすべて閉じる（shape と decode で 2 回）
            assert_eq!(counters.opens.load(Ordering::Relaxed), 2);
            assert_eq!(counters.closes.load(Ordering::Relaxed), 2);
            // 帯は 256 行ずつ
            assert_eq!(counters.bands.load(Ordering::Relaxed), 2);
        }
    }

    /// プレマルチプライドは adapter がストレートに戻す（8・16 ビット、グレーと RGB）。
    #[test]
    fn premultiplied_pixels_are_unpremultiplied() {
        for fmt in [
            PixelFormat::GrayA8,
            PixelFormat::Rgba8,
            PixelFormat::GrayA16,
            PixelFormat::Rgba16,
        ] {
            let straight = noise(70, 40, fmt, 3);
            let mut pre = straight.to_rows().unwrap();
            premultiply_rows(fmt, &mut pre);
            let mut want = pre.clone();
            unpremultiply_rows(fmt, &mut want);
            let mut img = FakeImage::still(&straight, PlatformAlpha::Premultiplied);
            img.frames[0].0 = pre;
            let v = decode(FakeImageBackend::new(img), &Limits::default(), &NoMonitor).unwrap();
            let Value::Image(got) = v else { panic!() };
            assert_eq!(got.px.to_rows().unwrap(), want, "{fmt:?}");
        }
    }

    /// 向きは adapter が `imaging::orient` で当て、形も向きを当てた大きさ。EXIF の向きは 1 にして運ぶ。ICC はそのまま。
    #[test]
    fn orientation_icc_and_exif() {
        let px = noise(5, 3, PixelFormat::Rgb8, 9);
        for o in 1..=8u8 {
            let mut img = FakeImage::still(&px, PlatformAlpha::None);
            img.orientation = o;
            img.icc = Some(vec![1, 2, 3, 4]);
            img.exif = Some(exif::minimal(u16::from(o), true));
            let dec = PlatformImageDecoder::new(FakeImageBackend::new(img));
            let shape = dec
                .shape(
                    &mut Cursor::new(vec![0u8]),
                    "heic",
                    &DecodeOptions::default(),
                    &Limits::default(),
                )
                .unwrap();
            let v = dec
                .decode(
                    &mut Cursor::new(vec![0u8]),
                    "heic",
                    &DecodeOptions::default(),
                    &Limits::default(),
                    &NoMonitor,
                )
                .unwrap();
            assert!(shape.admits(&v.shape()), "{o}");
            let Value::Image(got) = v else { panic!() };
            assert_eq!(got.px, orient(&px, o, &NoMonitor).unwrap(), "{o}");
            assert_eq!(got.meta.icc.as_deref(), Some(&vec![1, 2, 3, 4]));
            assert_eq!(exif::orientation(got.meta.exif.as_deref().unwrap()), 1);
        }
        // 範囲の外の向きは 1
        let mut img = FakeImage::still(&px, PlatformAlpha::None);
        img.orientation = 9;
        let v = decode(FakeImageBackend::new(img), &Limits::default(), &NoMonitor).unwrap();
        let Value::Image(got) = v else { panic!() };
        assert_eq!(got.px, px);
    }

    /// フレームの列（HEICS・AVIS）：キャンバスいっぱいのフレームと表示時間・繰り返しの回数、各フレームにも向きを当てる。
    #[test]
    fn sequences_become_full_frames() {
        let a = noise(4, 6, PixelFormat::Rgba8, 1);
        let b = noise(4, 6, PixelFormat::Rgba8, 2);
        let mut img = FakeImage::still(&a, PlatformAlpha::Straight);
        img.frames = vec![(a.to_rows().unwrap(), 40), (b.to_rows().unwrap(), 70_001)];
        img.loop_count = 3;
        img.orientation = 6;
        let v = decode(FakeImageBackend::new(img), &Limits::default(), &NoMonitor).unwrap();
        let Value::Frames(fr) = v else { panic!() };
        assert!(fr.is_full());
        assert_eq!(fr.loop_count, 3);
        assert_eq!(
            fr.frames()
                .iter()
                .map(|f| (f.px.clone(), f.duration_ms))
                .collect::<Vec<_>>(),
            [
                (orient(&a, 6, &NoMonitor).unwrap(), 40),
                (orient(&b, 6, &NoMonitor).unwrap(), 70_001)
            ]
        );
    }

    /// フレームの列を値にしない形式（TIFF の複数のページなど）は、最初のフレームだけを 1 枚の画像として読む。
    #[test]
    fn multi_page_formats_read_the_first_frame() {
        let a = noise(4, 6, PixelFormat::Rgb8, 1);
        let b = noise(4, 6, PixelFormat::Rgb8, 2);
        let mut img = FakeImage::still(&a, PlatformAlpha::None);
        img.frames = vec![(a.to_rows().unwrap(), 0), (b.to_rows().unwrap(), 0)];
        let mut backend = FakeImageBackend::new(img);
        backend.formats = vec!["tiff", "heic"];
        let counters = backend.counters();
        let dec = PlatformImageDecoder::new(backend);
        let read = |format: &str| {
            dec.decode(
                &mut Cursor::new(vec![0u8]),
                format,
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
            .unwrap()
        };
        let Value::Image(got) = read("tiff") else {
            panic!()
        };
        assert_eq!(got.px, a);
        assert_eq!(counters.begins.load(Ordering::Relaxed), 1);
        assert!(matches!(read("heic"), Value::Frames(_)));
    }

    /// 上限を超える形は、展開（begin）も確保もせずに `Limit`。ICC・EXIF の予算も同じ。
    #[test]
    fn limits_are_checked_before_decoding() {
        let px = noise(64, 64, PixelFormat::Rgba8, 1);
        let small = Limits {
            max_pixels: 1_000,
            ..Limits::default()
        };
        let b = FakeImageBackend::new(FakeImage::still(&px, PlatformAlpha::Straight));
        let counters = b.counters();
        assert!(matches!(
            decode(b, &small, &NoMonitor),
            Err(EngineError::Limit(_))
        ));
        assert_eq!(counters.begins.load(Ordering::Relaxed), 0);
        assert_eq!(counters.closes.load(Ordering::Relaxed), 1);
        let mut img = FakeImage::still(&px, PlatformAlpha::Straight);
        img.icc = Some(vec![0; 600]);
        img.exif = Some(vec![0; 600]);
        let tight = Limits {
            max_meta_bytes: 1_000,
            ..Limits::default()
        };
        let b = FakeImageBackend::new(img);
        let counters = b.counters();
        assert!(matches!(
            decode(b, &tight, &NoMonitor),
            Err(EngineError::Limit(_))
        ));
        assert_eq!(counters.begins.load(Ordering::Relaxed), 0);
        // 入力のバイト列も max_value_bytes まで
        let b = FakeImageBackend::new(FakeImage::still(&px, PlatformAlpha::Straight));
        let tiny = Limits {
            max_value_bytes: 2,
            ..Limits::default()
        };
        assert!(matches!(
            decode(b, &tiny, &NoMonitor),
            Err(EngineError::Limit(_))
        ));
    }

    /// 中止：帯・フレームの合間で、1〜4 回目の確認のどこでも止まる（E24 と同じ形）。
    #[test]
    fn cancels_between_bands() {
        let px = noise(20, 1_000, PixelFormat::Gray8, 1);
        for after in 1..=4 {
            let b = FakeImageBackend::new(FakeImage::still(&px, PlatformAlpha::None));
            let mon = CancelAfter::new(after);
            assert!(
                matches!(
                    decode(b, &Limits::default(), &mon),
                    Err(EngineError::Canceled)
                ),
                "{after}"
            );
        }
        // backend の中の中止（Swift のループが中止のコールバックを見た）も Canceled
        let mut b = FakeImageBackend::new(FakeImage::still(&px, PlatformAlpha::None));
        b.faults.push(ImageFault::Begin(BackendError::Canceled));
        assert!(matches!(
            decode(b, &Limits::default(), &NoMonitor),
            Err(EngineError::Canceled)
        ));
    }

    /// 壊れた形：展開した大きさ・形式が開いたときと違う、k 番目の帯で失敗、HDR、大きさ 0、backend の約束違反。
    #[test]
    fn broken_shapes_are_errors() {
        let px = noise(30, 600, PixelFormat::Rgb8, 1);
        let run = |fault: ImageFault| {
            let mut b = FakeImageBackend::new(FakeImage::still(&px, PlatformAlpha::None));
            b.faults.push(fault);
            decode(b, &Limits::default(), &NoMonitor)
        };
        assert!(matches!(
            run(ImageFault::DecodedSize(30, 599)),
            Err(EngineError::Decode { .. })
        ));
        assert!(matches!(
            run(ImageFault::DecodedFormat(PixelFormat::Rgba8)),
            Err(EngineError::Decode { .. })
        ));
        assert!(matches!(
            run(ImageFault::Band(
                2,
                BackendError::Failed("切れたファイル".into())
            )),
            Err(EngineError::Decode { .. })
        ));
        assert!(matches!(
            run(ImageFault::Open(BackendError::Unsupported("PSD".into()))),
            Err(EngineError::Unsupported(_))
        ));
        assert!(matches!(
            run(ImageFault::IccLength(3)),
            Err(EngineError::Internal(_))
        ));
        let mut img = FakeImage::still(&px, PlatformAlpha::None);
        img.hdr = true;
        assert!(matches!(
            decode(FakeImageBackend::new(img), &Limits::default(), &NoMonitor),
            Err(EngineError::Unsupported(_))
        ));
        let mut img = FakeImage::still(&px, PlatformAlpha::None);
        img.width = 0;
        assert!(matches!(
            decode(FakeImageBackend::new(img), &Limits::default(), &NoMonitor),
            Err(EngineError::Decode { .. })
        ));
        // アルファのない形式をストレートと言う・パレットのまま返すのは、backend の約束違反
        let img = FakeImage::still(&px, PlatformAlpha::Straight);
        assert!(matches!(
            decode(FakeImageBackend::new(img), &Limits::default(), &NoMonitor),
            Err(EngineError::Internal(_))
        ));
        let mut img = FakeImage::still(&px, PlatformAlpha::None);
        img.format = PixelFormat::Indexed8;
        assert!(matches!(
            decode(FakeImageBackend::new(img), &Limits::default(), &NoMonitor),
            Err(EngineError::Internal(_))
        ));
    }

    /// 固定の段が `Unsupported` を返した変種（CMYK の JPEG）は Platform へ回り、ふつうの JPEG は固定の段が読む。
    #[test]
    fn falls_through_from_the_pinned_tier() {
        let cmyk = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/jpeg-cmyk.jpg"
        ))
        .unwrap();
        let px = noise(8, 8, PixelFormat::Rgb8, 1);
        let mut b = FakeImageBackend::new(FakeImage::still(&px, PlatformAlpha::None));
        b.formats = vec!["heic", "jpeg"];
        let mut set = CodecSet::pinned();
        set.add_decoder(Arc::new(PlatformImageDecoder::new(b)))
            .unwrap();
        let read = |bytes: Vec<u8>| {
            set.decode(
                &mut Cursor::new(bytes),
                "jpeg",
                &DecodeOptions::default(),
                &Limits::default(),
                &NoMonitor,
            )
        };
        let (_, stamp) = read(cmyk).unwrap();
        assert_eq!(stamp.tier, Tier::Platform);
        let (rgb, _) = set
            .encode_bytes(
                &Value::Image(Arc::new(Image {
                    px,
                    meta: ImageMeta::default(),
                })),
                "jpeg",
                &serde_json::json!({}),
                &NoMonitor,
            )
            .unwrap();
        assert_eq!(read(rgb).unwrap().1.tier, Tier::Pinned);
    }
}
