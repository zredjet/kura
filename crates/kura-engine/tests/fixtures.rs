//! 他のツールで作ったファイル（`fixtures/`）を読み、他の実装の読み取り（参照の PNG）と照らす（docs/editing.md §8.5）。
//!
//! - フィクスチャの作り方と道具の版は `fixtures/README.md`。ファイルは消さない・変えない（追記だけの検査の対象）。
//! - 自前のエンコーダでは作れない形（非可逆の WebP・libjpeg のプログレッシブ・Photoshop 互換の PSD など）の確かめに使う。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Cursor;
use std::path::PathBuf;

use kura_engine::codec::CodecSet;
use kura_engine::doc::DecodeOptions;
use kura_engine::imaging::convert::to_rgba8_rows;
use kura_engine::value::PixelFormat;
use kura_engine::{EngineError, Limits, NoMonitor, Value};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn decode(name: &str, format: &str) -> Result<Value, EngineError> {
    let bytes = std::fs::read(dir().join(name)).unwrap();
    CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .map(|(v, _)| v)
}

/// 画素を RGBA の 16 ビット（0〜65535）にそろえて並べる（8 ビットは 257 倍）。
fn rgba16(v: &Value) -> Vec<Vec<u16>> {
    let one = |px: &kura_engine::value::Raster| -> Vec<u16> {
        let rows = px.to_rows().unwrap();
        match px.format() {
            PixelFormat::Rgb16
            | PixelFormat::Rgba16
            | PixelFormat::Gray16
            | PixelFormat::GrayA16 => {
                let s: Vec<u16> = rows
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect();
                let ch = px.format().bytes_per_pixel() / 2;
                s.chunks_exact(ch)
                    .flat_map(|p| match ch {
                        1 => [p[0], p[0], p[0], 65_535],
                        2 => [p[0], p[0], p[0], p[1]],
                        3 => [p[0], p[1], p[2], 65_535],
                        _ => [p[0], p[1], p[2], p[3]],
                    })
                    .collect()
            }
            _ => to_rgba8_rows(px)
                .unwrap()
                .iter()
                .map(|b| u16::from(*b) * 257)
                .collect(),
        }
    };
    match v {
        Value::Image(i) => vec![one(&i.px)],
        Value::Frames(f) => f.frames().iter().map(|x| one(&x.px)).collect(),
        Value::Audio(_) => panic!(),
    }
}

/// 最大の差（アルファ 0 の画素の色は比べない）。
fn max_diff(a: &[Vec<u16>], b: &[Vec<u16>]) -> u16 {
    assert_eq!(a.len(), b.len(), "フレームの数");
    let mut m = 0;
    for (x, y) in a.iter().zip(b) {
        assert_eq!(x.len(), y.len(), "大きさ");
        for (p, q) in x.chunks_exact(4).zip(y.chunks_exact(4)) {
            let visible = p[3] != 0 || q[3] != 0;
            for c in 0..4 {
                if c == 3 || visible {
                    m = m.max(p[c].abs_diff(q[c]));
                }
            }
        }
    }
    m
}

/// PSD（ImageMagick が書いたもの）は、ImageIO（sips）か ImageMagick の読み取り、または元の画像とちょうど同じ。
/// 透明のある合成は ImageMagick が下塗りせずに書くので、元の画像と同じになる（白の下塗りの判定が「下塗りなし」と分かる）。
#[test]
fn psd_matches_other_readers() {
    for (file, reference) in [
        ("psd-rgb8.psd", "psd-rgb8.sips.png"),
        ("psd-rgb8-raw.psd", "psd-rgb8-raw.sips.png"),
        ("psd-rgba8.psd", "psd-rgba8.source.png"),
        ("psd-gray8.psd", "psd-gray8.sips.png"),
        ("psd-indexed.psd", "psd-indexed.sips.png"),
        ("psd-rgb16.psd", "psd-rgb16.magick.png"),
        ("psb-rgb8.psb", "psb-rgb8.sips.png"),
        ("psb-rgba8.psb", "psb-rgba8.source.png"),
    ] {
        let ours = rgba16(&decode(file, "psd").unwrap());
        let theirs = rgba16(&decode(reference, "png").unwrap());
        assert_eq!(max_diff(&ours, &theirs), 0, "{file}");
    }
    assert!(matches!(
        decode("psd-cmyk.psd", "psd"),
        Err(EngineError::Unsupported(_))
    ));
}

/// 非可逆の WebP（アルファ付きを含む）は libwebp の dwebp とちょうど同じ。
#[test]
fn lossy_webp_matches_libwebp() {
    for (file, reference) in [
        ("webp-lossy.webp", "webp-lossy.dwebp.png"),
        ("webp-lossy-alpha.webp", "webp-lossy-alpha.dwebp.png"),
    ] {
        let ours = rgba16(&decode(file, "webp").unwrap());
        let theirs = rgba16(&decode(reference, "png").unwrap());
        assert_eq!(max_diff(&ours, &theirs), 0, "{file}");
    }
}

/// アニメの WebP（可逆・非可逆の混在、dispose・blend あり）は、libwebp を使う ImageMagick の合成と同じ。
/// ただし ImageMagick は、アルファのない最初のフレームの外側を不透明な黒にする（仕様とブラウザは透明）ので、最初のフレームは描いた範囲だけ比べる。
#[test]
fn animated_webp_matches_libwebp_compositing() {
    let anim = decode("webp-anim.webp", "webp").unwrap();
    let Value::Frames(f) = &anim else { panic!() };
    assert_eq!(
        f.frames().iter().map(|x| x.duration_ms).collect::<Vec<_>>(),
        vec![100, 120, 80]
    );
    assert_eq!(f.loop_count, 3);
    let ours = rgba16(&anim);
    for (i, frame) in ours.iter().enumerate() {
        let theirs =
            rgba16(&decode(&format!("webp-anim.magick-{i}.png"), "png").unwrap()).remove(0);
        for (k, (p, q)) in frame
            .chunks_exact(4)
            .zip(theirs.chunks_exact(4))
            .enumerate()
        {
            let (x, y) = (k % 40, k / 40);
            if i == 0 && !((2..21).contains(&x) && (2..21).contains(&y)) {
                assert_eq!(p[3], 0, "最初のフレームの外側は透明");
                continue;
            }
            // 完全に透明な画素の色は見えないので比べない
            if p[3] == 0 && q[3] == 0 {
                continue;
            }
            assert_eq!(p, q, "frame {i} ({x}, {y})");
        }
    }
}

/// JPEG は libjpeg-turbo の djpeg と近い（IDCT と間引きの戻し方が違うので、ちょうどではない）。値そのものはデコーダのゴールデンで凍結する。
#[test]
fn jpeg_is_close_to_libjpeg_turbo() {
    for (file, tolerance) in [
        ("jpeg-progressive.jpg", 8),
        ("jpeg-gray.jpg", 2),
        ("jpeg-422.jpg", 8),
        ("jpeg-restart.jpg", 3),
    ] {
        let reference = file.replace(".jpg", ".djpeg.png");
        let ours = rgba16(&decode(file, "jpeg").unwrap());
        let theirs = rgba16(&decode(&reference, "png").unwrap());
        let d = max_diff(&ours, &theirs) / 257;
        assert!(d <= tolerance, "{file}: {d}");
    }
    assert!(matches!(
        decode("jpeg-cmyk.jpg", "jpeg"),
        Err(EngineError::Unsupported(_))
    ));
}
