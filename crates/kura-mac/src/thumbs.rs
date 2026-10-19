//! M11 のサムネイルとフォントの情報（Swift の C 文字列と整数の口。ADR-37）。
//!
//! - `image_thumbnails_keep_alpha`：ImageIO の道でアルファを保つ（HEIC・HEIF・AVIF・TIFF・BMP・JPEG。決定 3）。透明な画素が
//!   あれば PNG、なければ JPEG（品質 0.82）を書き、書いた形式を返す（Swift の戻り値 0 = JPEG、4 = PNG）。
//! - `font_thumbnails`：CoreText で見本の字を、正方形の透明の地に字の形だけのアルファで描いた PNG を書く（色は表示の側が
//!   CSS の mask-image で付ける）。
//! - `probe_font`：書体名・スタイル・字の数・形式（`kura_probe` の種類 3）。
//!
//! 書き先は拡張子のない stem（kura-core の `ThumbTargets`）で、Swift が書いた形式（kura-core の `ThumbFormat`）の拡張子を足す。
//! macOS 以外はどれも `Unsupported`。

use std::path::Path;

pub use kura_core::media::FontInfo;
use kura_core::media::{MediaError, ThumbFormat, ThumbTargets};
use serde::{Deserialize, Serialize};

/// フォントの見本の字の候補。先頭から、その書体がすべての字を持つものを使う（どれも持たなければ、字の番号 1 から順に描く）。
pub const FONT_SAMPLES: [&str; 2] = ["永あAa", "Aa"];

/// フォントの probe の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FontProbe {
    /// 形式（`ttf`・`otf`・`ttc`・`woff`・`woff2`。先頭のバイトで決める）。
    pub codec: String,
    pub font: FontInfo,
}

/// 画像のサムネイル（ImageIO）。透明な画素があれば PNG、なければ JPEG を書き、書いた形式を返す。
pub fn image_thumbnails_keep_alpha(
    path: &Path,
    out: &ThumbTargets,
) -> Result<ThumbFormat, MediaError> {
    #[cfg(target_os = "macos")]
    {
        mac::image_thumbnails(path, out, true)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, out);
        Err(MediaError::Unsupported)
    }
}

/// フォントのサムネイル（CoreText）。正方形の透明の地に、字の形だけのアルファで描いた PNG を書く（いつも PNG）。
pub fn font_thumbnails(path: &Path, out: &ThumbTargets) -> Result<ThumbFormat, MediaError> {
    #[cfg(target_os = "macos")]
    {
        mac::font_thumbnails(path, out)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, out);
        Err(MediaError::Unsupported)
    }
}

/// フォントの情報を CoreText で読む。読めない（フォントでない・壊れている）なら `Unsupported`。
pub fn probe_font(path: &Path) -> Result<FontProbe, MediaError> {
    #[cfg(target_os = "macos")]
    {
        let json = mac::probe_json(path, mac::KIND_FONT)?;
        serde_json::from_str(&json).map_err(|e| MediaError::Failed(e.to_string()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(MediaError::Unsupported)
    }
}

#[cfg(target_os = "macos")]
pub(crate) mod mac {
    use std::ffi::{CStr, CString, c_char};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use kura_core::media::MediaError;

    use super::{FONT_SAMPLES, ThumbFormat, ThumbTargets};
    use crate::{LARGE_PX, SMALL_PX};

    /// Swift の戻り値（Common.swift）。
    pub(crate) const OK: i32 = 0;
    pub(crate) const UNSUPPORTED: i32 = 1;
    pub(crate) const NO_ARTWORK: i32 = 3;
    pub(crate) const PNG: i32 = 4;

    /// `kura_probe` の種類の番号。
    pub(crate) const KIND_FONT: i32 = 3;

    unsafe extern "C" {
        fn kura_image_thumbnails(
            path: *const c_char,
            out_large_stem: *const c_char,
            out_small_stem: *const c_char,
            large_px: i32,
            small_px: i32,
            keep_alpha: i32,
        ) -> i32;
        fn kura_font_thumbnails(
            path: *const c_char,
            samples: *const c_char,
            out_large_stem: *const c_char,
            out_small_stem: *const c_char,
            large_px: i32,
            small_px: i32,
        ) -> i32;
        fn kura_probe(path: *const c_char, kind: i32) -> *mut c_char;
        fn kura_free_string(pointer: *mut c_char);
    }

    /// パスを NUL 終端の文字列にする。
    pub(crate) fn c_path(path: &Path) -> Result<CString, MediaError> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| {
            MediaError::Failed(format!("パスに NUL 文字があります: {}", path.display()))
        })
    }

    /// 戻り値（0・1・2）を結果にする。
    pub(crate) fn check(code: i32) -> Result<(), MediaError> {
        match code {
            OK => Ok(()),
            UNSUPPORTED => Err(MediaError::Unsupported),
            _ => Err(MediaError::Failed("macOS での処理に失敗しました".into())),
        }
    }

    /// サムネイルの戻り値（0 = JPEG・4 = PNG・1・2）を、書いた形式にする。
    fn written(code: i32) -> Result<ThumbFormat, MediaError> {
        match code {
            PNG => Ok(ThumbFormat::Png),
            other => check(other).map(|()| ThumbFormat::Jpeg),
        }
    }

    /// ImageIO のサムネイル。`keep_alpha` が偽なら、今までどおりいつも JPEG を書く。
    pub(crate) fn image_thumbnails(
        path: &Path,
        out: &ThumbTargets,
        keep_alpha: bool,
    ) -> Result<ThumbFormat, MediaError> {
        let (src, large, small) = (c_path(path)?, c_path(&out.large)?, c_path(&out.small)?);
        // SAFETY: すべて NUL 終端の文字列で、呼び出しのあいだ生きている。Swift 側は読むだけ。
        let code = unsafe {
            kura_image_thumbnails(
                src.as_ptr(),
                large.as_ptr(),
                small.as_ptr(),
                LARGE_PX,
                SMALL_PX,
                i32::from(keep_alpha),
            )
        };
        written(code)
    }

    /// CoreText のフォントのサムネイル（いつも PNG）。
    pub(crate) fn font_thumbnails(
        path: &Path,
        out: &ThumbTargets,
    ) -> Result<ThumbFormat, MediaError> {
        let (src, large, small) = (c_path(path)?, c_path(&out.large)?, c_path(&out.small)?);
        let samples = CString::new(FONT_SAMPLES.join("\n"))
            .map_err(|_| MediaError::Failed("見本の字に NUL 文字があります".into()))?;
        // SAFETY: すべて NUL 終端の文字列で、呼び出しのあいだ生きている。Swift 側は読むだけ。
        let code = unsafe {
            kura_font_thumbnails(
                src.as_ptr(),
                samples.as_ptr(),
                large.as_ptr(),
                small.as_ptr(),
                LARGE_PX,
                SMALL_PX,
            )
        };
        match written(code)? {
            ThumbFormat::Png => Ok(ThumbFormat::Png),
            ThumbFormat::Jpeg => Err(MediaError::Failed(
                "フォントのサムネイルが PNG で書かれていません".into(),
            )),
        }
    }

    /// `kura_probe` の JSON。扱えなければ `Unsupported`。
    pub(crate) fn probe_json(path: &Path, kind: i32) -> Result<String, MediaError> {
        let path = c_path(path)?;
        // SAFETY: path は NUL 終端。戻り値は NULL か strdup した文字列で、kura_free_string で解放する。
        let pointer = unsafe { kura_probe(path.as_ptr(), kind) };
        if pointer.is_null() {
            return Err(MediaError::Unsupported);
        }
        // SAFETY: NULL でない戻り値は NUL 終端の UTF-8（JSON）。
        let json = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: kura_probe が返したポインタを一度だけ解放する。
        unsafe { kura_free_string(pointer) };
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn stems_get_the_extension_of_the_written_format() {
        let stems = ThumbTargets {
            large: PathBuf::from("/lib/thumbs/ab/abc.def_l"),
            small: PathBuf::from("/lib/thumbs/ab/abc.def_s"),
        };
        assert_eq!(
            stems.files(ThumbFormat::Png),
            (
                PathBuf::from("/lib/thumbs/ab/abc.def_l.png"),
                PathBuf::from("/lib/thumbs/ab/abc.def_s.png")
            )
        );
        assert_eq!(stems.files(ThumbFormat::Jpeg).1.extension().unwrap(), "jpg");
    }

    #[test]
    fn font_samples_have_no_separator_or_nul() {
        assert!(
            FONT_SAMPLES
                .iter()
                .all(|s| !s.is_empty() && !s.contains('\n') && !s.contains('\0'))
        );
    }

    #[test]
    fn font_probe_reads_the_swift_json() {
        let p: FontProbe = serde_json::from_str(
            r#"{"codec":"woff2","font":{"family":"Fira Sans","style":"Regular","fullName":"Fira Sans","postscriptName":"FiraSans-Regular","glyphs":1686,"faces":1,"variable":false}}"#,
        )
        .unwrap();
        assert_eq!(p.codec, "woff2");
        assert_eq!(p.font.family.as_deref(), Some("Fira Sans"));
        assert_eq!(
            (p.font.glyphs, p.font.faces, p.font.variable),
            (1686, 1, false)
        );
    }
}

/// macOS の実物のテスト（ImageIO・CoreText）。
#[cfg(test)]
#[cfg(target_os = "macos")]
mod mac_tests {
    use std::path::PathBuf;
    use std::process::Command;

    use kura_engine::Value;
    use kura_engine::codec::CodecSet;
    use kura_engine::imaging::convert::to_rgba8_rows;

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    fn stems(dir: &Path) -> ThumbTargets {
        ThumbTargets {
            large: dir.join("t/x_l"),
            small: dir.join("t/x_s"),
        }
    }

    /// PNG を読んで (幅, 高さ, RGBA8 の行)。
    fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
        let bytes = std::fs::read(path).unwrap();
        let (v, _) = crate::codec::testutil::decode(&CodecSet::pinned(), &bytes, "png").unwrap();
        let Value::Image(img) = v else {
            panic!("画像でない")
        };
        (
            img.px.width(),
            img.px.height(),
            to_rgba8_rows(&img.px).unwrap(),
        )
    }

    fn alpha_at(px: &(u32, u32, Vec<u8>), x: u32, y: u32) -> u8 {
        px.2[((y * px.0 + x) * 4 + 3) as usize]
    }

    /// 決定 3・4：アルファのある HEIC・TIFF は、ImageIO の道でアルファを保ち PNG を書く（戻り値 4）。角の α は 0、中は 255。
    #[test]
    fn image_thumbnails_keep_alpha_in_heic_and_tiff() {
        let _g = crate::codec::testutil::shared();
        for name in ["heic-alpha-48x32.heic", "tiff-alpha-48x32.tif"] {
            let dir = tempfile::tempdir().unwrap();
            let out = stems(dir.path());
            let format = image_thumbnails_keep_alpha(&fixture(name), &out).unwrap();
            assert_eq!(format, ThumbFormat::Png, "{name}");
            let (l, s) = out.files(ThumbFormat::Png);
            let (lj, sj) = out.files(ThumbFormat::Jpeg);
            assert!(!lj.exists() && !sj.exists(), "{name}: JPEG も書いた");
            for file in [&l, &s] {
                let px = read_png(file);
                assert_eq!((px.0, px.1), (48, 32), "{name}: 元より大きくしない");
                assert_eq!(alpha_at(&px, 0, 0), 0, "{name}: 角が透明でない");
                assert_eq!(alpha_at(&px, 47, 31), 0, "{name}: 角が透明でない");
                assert_eq!(alpha_at(&px, 24, 16), 255, "{name}: 中が不透明でない");
            }
        }
    }

    /// 透明のない画像（JPEG・アルファの列があってもすべて不透明な PNG）は JPEG のまま。
    #[test]
    fn opaque_images_stay_jpeg() {
        let _g = crate::codec::testutil::shared();
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("opaque.png");
        let rgba: Vec<u8> = (0..40 * 30).flat_map(|i| [i as u8, 50, 90, 255]).collect();
        let v = Value::Image(std::sync::Arc::new(kura_engine::value::Image {
            px: kura_engine::value::Raster::from_rows(
                40,
                30,
                kura_engine::value::PixelFormat::Rgba8,
                None,
                &rgba,
            )
            .unwrap(),
            meta: Default::default(),
        }));
        let bytes = CodecSet::pinned()
            .encode_bytes(&v, "png", &serde_json::json!({}), &kura_engine::NoMonitor)
            .unwrap()
            .0;
        std::fs::write(&png, bytes).unwrap();
        let jpeg = dir.path().join("photo.jpg");
        let status = Command::new("sips")
            .args(["-s", "format", "jpeg"])
            .arg(&png)
            .arg("--out")
            .arg(&jpeg)
            .output()
            .unwrap()
            .status;
        assert!(status.success());
        for src in [&png, &jpeg] {
            let out = stems(
                &dir.path()
                    .join(format!("out-{}", src.extension().unwrap().display())),
            );
            assert_eq!(
                image_thumbnails_keep_alpha(src, &out).unwrap(),
                ThumbFormat::Jpeg,
                "{}",
                src.display()
            );
            let (l, s) = out.files(ThumbFormat::Jpeg);
            assert!(l.is_file() && s.is_file());
            assert!(!out.files(ThumbFormat::Png).0.exists());
        }
    }

    /// アルファを保たない呼び方（今の MediaBackend の道）は、アルファのある元でも JPEG だけを書く。
    #[test]
    fn legacy_mode_always_writes_jpeg() {
        let _g = crate::codec::testutil::shared();
        let dir = tempfile::tempdir().unwrap();
        let out = stems(dir.path());
        let format = mac::image_thumbnails(&fixture("heic-alpha-48x32.heic"), &out, false).unwrap();
        assert_eq!(format, ThumbFormat::Jpeg);
        assert!(out.files(ThumbFormat::Jpeg).0.is_file());
        assert!(!out.files(ThumbFormat::Png).0.exists());
    }

    #[test]
    fn broken_images_fail_instead_of_crashing() {
        let _g = crate::codec::testutil::shared();
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.heic");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(image_thumbnails_keep_alpha(&bad, &stems(dir.path())).is_err());
    }

    /// システムのフォント（macOS に必ずあるもの）。
    fn system_font(name: &str) -> PathBuf {
        let p = PathBuf::from("/System/Library/Fonts").join(name);
        assert!(p.is_file(), "{} がない", p.display());
        p
    }

    /// TTF を、表を圧縮しない WOFF 1.0 に包み直す（WOFF のテストの見本。表の中身はそのまま）。
    fn ttf_to_woff(ttf: &[u8]) -> Vec<u8> {
        let be16 = |b: &[u8], at: usize| u16::from_be_bytes([b[at], b[at + 1]]);
        let be32 = |b: &[u8], at: usize| u32::from_be_bytes(b[at..at + 4].try_into().unwrap());
        let flavor = be32(ttf, 0);
        let n = usize::from(be16(ttf, 4));
        let tables: Vec<([u8; 4], u32, u32, u32)> = (0..n)
            .map(|i| {
                let r = 12 + i * 16;
                (
                    ttf[r..r + 4].try_into().unwrap(),
                    be32(ttf, r + 4),
                    be32(ttf, r + 8),
                    be32(ttf, r + 12),
                )
            })
            .collect();
        let pad = |len: u32| (len + 3) & !3;
        let sfnt_size = 12 + 16 * n as u32 + tables.iter().map(|t| pad(t.3)).sum::<u32>();
        let mut offset = 44 + 20 * n as u32;
        let mut dir = Vec::new();
        let mut data = Vec::new();
        for (tag, checksum, at, len) in &tables {
            dir.extend_from_slice(tag);
            dir.extend_from_slice(&offset.to_be_bytes());
            dir.extend_from_slice(&len.to_be_bytes());
            dir.extend_from_slice(&len.to_be_bytes());
            dir.extend_from_slice(&checksum.to_be_bytes());
            data.extend_from_slice(&ttf[*at as usize..(*at + *len) as usize]);
            data.resize(data.len() + (pad(*len) - len) as usize, 0);
            offset += pad(*len);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"wOFF");
        out.extend_from_slice(&flavor.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&(n as u16).to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&sfnt_size.to_be_bytes());
        out.extend_from_slice(&[0, 1, 0, 0]);
        out.extend_from_slice(&[0; 20]);
        out.extend_from_slice(&dir);
        out.extend_from_slice(&data);
        assert_eq!(out.len() as u32, offset);
        out
    }

    /// α > 0 の画素を囲む矩形の幅と高さ。
    fn ink_box(px: &(u32, u32, Vec<u8>)) -> (u32, u32) {
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for y in 0..px.1 {
            for x in 0..px.0 {
                if alpha_at(px, x, y) > 0 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
                }
            }
        }
        assert!(x0 <= x1, "字が描かれていない");
        (x1 - x0 + 1, y1 - y0 + 1)
    }

    /// 決定 9・plan defaults 21：フォントのサムネイルは、正方形の透明の地に字の形だけのアルファ（字は黒）の PNG。ttf・ttc・otf・
    /// woff を読む。見本の字は、書体が持つ候補（ヒラギノは「永あAa」、Geneva は「Aa」）。
    #[test]
    fn font_thumbnails_are_transparent_masks() {
        let _g = crate::codec::testutil::shared();
        let dir = tempfile::tempdir().unwrap();
        let woff = dir.path().join("geneva.woff");
        std::fs::write(
            &woff,
            ttf_to_woff(&std::fs::read(system_font("Geneva.ttf")).unwrap()),
        )
        .unwrap();
        let mut ratios = Vec::new();
        for (k, src) in [
            system_font("Geneva.ttf"),
            system_font("ヒラギノ角ゴシック W3.ttc"),
            system_font("LastResort.otf"),
            woff,
        ]
        .iter()
        .enumerate()
        {
            let out = stems(&dir.path().join(format!("out{k}")));
            font_thumbnails(src, &out).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
            let (l, s) = out.files(ThumbFormat::Png);
            for (file, side) in [(&l, 1600), (&s, 512)] {
                let px = read_png(file);
                assert_eq!((px.0, px.1), (side, side), "{}", src.display());
                for (x, y) in [(0, 0), (side - 1, 0), (0, side - 1), (side - 1, side - 1)] {
                    assert_eq!(alpha_at(&px, x, y), 0, "{}: 角が透明でない", src.display());
                }
                let mut opaque = 0usize;
                for p in px.2.chunks_exact(4) {
                    if p[3] > 0 {
                        assert_eq!(&p[..3], &[0, 0, 0], "{}: 字が黒でない", src.display());
                    }
                    if p[3] == 255 {
                        opaque += 1;
                    }
                }
                let share = opaque as f64 / f64::from(side * side);
                assert!(
                    (0.005..0.6).contains(&share),
                    "{}: 字の画素の割合 {share}",
                    src.display()
                );
                let (w, h) = ink_box(&px);
                assert!(w <= side * 81 / 100 + 2 && h <= side * 71 / 100 + 2);
                if side == 512 {
                    ratios.push(f64::from(w) / f64::from(h));
                }
            }
        }
        // 「永あAa」（4 字）は「Aa」（2 字）より横に長い
        assert!(ratios[1] > ratios[0] * 1.5, "見本の字の選び方 {ratios:?}");
    }

    #[test]
    fn probe_reads_font_family_and_glyph_count() {
        let _g = crate::codec::testutil::shared();
        let geneva = probe_font(&system_font("Geneva.ttf")).unwrap();
        assert_eq!(geneva.codec, "ttf");
        assert_eq!(geneva.font.family.as_deref(), Some("Geneva"));
        assert_eq!(geneva.font.style.as_deref(), Some("Regular"));
        assert_eq!(geneva.font.faces, 1);
        assert!(geneva.font.glyphs > 100);
        assert!(!geneva.font.variable);

        let hiragino = probe_font(&system_font("ヒラギノ角ゴシック W3.ttc")).unwrap();
        assert_eq!(hiragino.codec, "ttc");
        assert!(hiragino.font.faces >= 2, "{hiragino:?}");
        assert!(hiragino.font.glyphs > 10_000, "{hiragino:?}");

        assert_eq!(
            probe_font(&system_font("LastResort.otf")).unwrap().codec,
            "otf"
        );
        let dir = tempfile::tempdir().unwrap();
        let woff = dir.path().join("geneva.woff");
        std::fs::write(
            &woff,
            ttf_to_woff(&std::fs::read(system_font("Geneva.ttf")).unwrap()),
        )
        .unwrap();
        let w = probe_font(&woff).unwrap();
        assert_eq!((w.codec.as_str(), w.font), ("woff", geneva.font));
        // 可変フォント（SF）は軸がある
        assert!(probe_font(&system_font("SFNS.ttf")).unwrap().font.variable);
    }

    /// 知らない種類の番号・フォントでないファイルは NULL（`Unsupported`）。
    #[test]
    fn probe_unknown_kind_is_unsupported() {
        let _g = crate::codec::testutil::shared();
        let font = system_font("Geneva.ttf");
        assert!(matches!(
            mac::probe_json(&font, 9),
            Err(MediaError::Unsupported)
        ));
        assert!(matches!(
            mac::probe_json(&font, -1),
            Err(MediaError::Unsupported)
        ));
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake.ttf");
        std::fs::write(&fake, b"\0\x01\0\0 not really a font").unwrap();
        let png = fixture("png-rgba8-srgb.png");
        for bad in [&fake, &png] {
            assert!(matches!(probe_font(bad), Err(MediaError::Unsupported)));
            assert!(matches!(
                font_thumbnails(bad, &stems(dir.path())),
                Err(MediaError::Unsupported)
            ));
        }
    }
}
