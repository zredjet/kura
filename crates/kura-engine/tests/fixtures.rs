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

/// 音声を読み、使ったデコーダと planar の標本を返す。
fn audio(
    name: &str,
    format: &str,
) -> (
    String,
    bool,
    kura_engine::value::AudioMeta,
    u32,
    Vec<Vec<f32>>,
) {
    let bytes = std::fs::read(dir().join(name)).unwrap();
    let (v, stamp) = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    let Value::Audio(a) = v else { panic!("{name}") };
    let planes = a.planes().iter().map(|p| p.to_vec()).collect();
    (
        stamp.codec,
        stamp.reproducible,
        a.meta.clone(),
        a.rate(),
        planes,
    )
}

/// 先頭の `n` フレームだけにする。
fn head(p: &[Vec<f32>], n: usize) -> Vec<Vec<f32>> {
    p.iter().map(|c| c[..n].to_vec()).collect()
}

/// 参照に対する誤差のエネルギーの比（信号 / 誤差）。長さが違えば短いほうまで比べる。参照が 16 ビットの端に
/// 張り付いた標本（丸めで切れた値）は比べない。
fn signal_to_error(ours: &[Vec<f32>], theirs: &[Vec<f32>]) -> f64 {
    assert_eq!(ours.len(), theirs.len(), "チャンネルの数");
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (a, b) in ours.iter().zip(theirs) {
        for (x, y) in a.iter().zip(b) {
            if *y <= -1.0 || *y >= 32_767.0 / 32_768.0 {
                continue;
            }
            sig += f64::from(*y) * f64::from(*y);
            let d = f64::from(*x) - f64::from(*y);
            err += d * d;
        }
    }
    sig / kura_engine::math::max(err, 1e-30)
}

/// 可逆の形式（flac・ALAC・AIFF・AIFF-C の f32）は、元の WAV とちょうど同じ値を読む。
#[test]
fn lossless_audio_matches_the_source_wav() {
    let (_, _, _, r16, src16) = audio("audio-src16.wav", "wav");
    let (_, _, _, r24, src24) = audio("audio-src24.wav", "wav");
    assert_eq!((r16, src16[0].len()), (44_100, 44_100));
    assert_eq!((r24, src24[0].len()), (48_000, 24_000));
    for (file, format, id, src, frames, bits) in [
        (
            "flac-16.flac",
            "flac",
            "flac-symphonia-dec/1",
            &src16,
            44_100,
            16,
        ),
        (
            "flac-24.flac",
            "flac",
            "flac-symphonia-dec/1",
            &src24,
            24_000,
            24,
        ),
        (
            "alac-16.m4a",
            "m4a",
            "m4a-alac-symphonia-dec/1",
            &src16,
            44_100,
            16,
        ),
        (
            "alac-24.m4a",
            "m4a",
            "m4a-alac-symphonia-dec/1",
            &src24,
            24_000,
            24,
        ),
        ("aiff-24.aiff", "aiff", "aiff-kura-dec/1", &src24, 4_800, 24),
        ("aifc-f32.aif", "aiff", "aiff-kura-dec/1", &src16, 4_410, 32),
    ] {
        let (codec, reproducible, meta, _, ours) = audio(file, format);
        assert_eq!(codec, id, "{file}");
        assert!(reproducible, "{file}");
        assert_eq!(ours, head(src, frames), "{file}");
        assert_eq!(meta.source.map(|s| s.bits), Some(bits), "{file}");
    }
}

/// 非可逆の形式は、他のデコーダ（ffmpeg・afconvert・lewton）の読み取りと、長さ（gapless の切り取り）がちょうど同じで、
/// 値は 60 dB 以上の比で近い（参照は 16 ビットに丸めてある）。
#[test]
fn lossy_audio_matches_other_decoders() {
    let mut bad = Vec::new();
    for (file, format, id, reference, frames, reproducible) in [
        (
            "mp3-lame.mp3",
            "mp3",
            "mp3-nanomp3-dec/1",
            "mp3-lame.ffmpeg.flac",
            44_100,
            true,
        ),
        (
            "mp3-notag.mp3",
            "mp3",
            "mp3-nanomp3-dec/1",
            "mp3-notag.ffmpeg.flac",
            46_080,
            true,
        ),
        (
            "mp3-mpeg2-mono.mp3",
            "mp3",
            "mp3-nanomp3-dec/1",
            "mp3-mpeg2-mono.ffmpeg.flac",
            22_050,
            true,
        ),
        // Vorbis・AAC：段 9 から値を再現できる（symphonia の写しで表の超越関数を libm クレートにした）
        (
            "ogg-vorbis-loop.ogg",
            "ogg",
            "ogg-vorbis-symphonia-dec/3",
            "ogg-vorbis-loop.lewton.flac",
            44_100,
            true,
        ),
        (
            "ogg-vorbis-ffmpeg.ogg",
            "ogg",
            "ogg-vorbis-symphonia-dec/3",
            "ogg-vorbis-ffmpeg.lewton.flac",
            44_160,
            true,
        ),
        (
            "aac-lc-afconvert.m4a",
            "m4a",
            "m4a-aac-symphonia-dec/2",
            "aac-lc-afconvert.afconvert.flac",
            44_100,
            true,
        ),
        (
            "aac-lc-ffmpeg.m4a",
            "m4a",
            "m4a-aac-symphonia-dec/2",
            "aac-lc-ffmpeg.ffmpeg.flac",
            44_100,
            true,
        ),
        (
            "aac-lc.aac",
            "aac",
            "aac-adts-symphonia-dec/2",
            "aac-lc.ffmpeg.flac",
            46_080,
            true,
        ),
        // free format（ffmpeg は読めないので LAME のデコーダと照らす。最後の 2 フレームも読む）
        (
            "mp3-free.mp3",
            "mp3",
            "mp3-nanomp3-dec/1",
            "mp3-free.lame.flac",
            16_000,
            true,
        ),
        // Info のフレームの数が 0（途中で止まった ffmpeg の出力の形）：長さは分からないものとして全部読む
        (
            "mp3-info-frames0.mp3",
            "mp3",
            "mp3-nanomp3-dec/1",
            "mp3-info-frames0.ffmpeg.flac",
            44_975,
            true,
        ),
    ] {
        let (codec, rep, _, rate, ours) = audio(file, format);
        let (_, _, _, ref_rate, theirs) = audio(reference, "flac");
        assert_eq!(codec, id, "{file}");
        assert_eq!(rep, reproducible, "{file}");
        assert_eq!(rate, ref_rate, "{file}");
        let ratio = signal_to_error(&ours, &theirs);
        eprintln!(
            "{file}: {} / {} フレーム, 信号/誤差 {ratio:.3e}",
            ours[0].len(),
            theirs[0].len()
        );
        if ours[0].len() != frames || ratio < 1e6 {
            bad.push(file);
        }
    }
    assert!(bad.is_empty(), "{bad:?}");
}

/// 3・5 チャンネルの Ogg Vorbis（libvorbis の aoTuV が書いたもの。2 チャンネル以外は coupling なしで、residue 1 を全チャンネルで
/// 使う）は、lewton の読み取りと長さがちょうど同じで、値はチャンネルごとに 60 dB 以上の比で近い（無音のチャンネルは無音）。
/// 参照は lewton の出力をそのままの並び（Vorbis の並び）で保存したので、kura の WAV の並びに直して比べる。チャンネルごとに
/// 違う音にしてあるので、並びの誤りも見つかる。symphonia-codec-vorbis 0.6.1 の不具合（residue 0/1 を 2 チャンネル以上で使うと、
/// 次のチャンネルの分類を上書きする。third_party/symphonia-codec-vorbis/KURA.md）の回帰の試験。
#[test]
fn multichannel_vorbis_matches_lewton() {
    // Vorbis のチャンネル i は kura の planes[ORDER[n - 1][i]]（Vorbis I §4.3.9。codec::vorbis::ORDER と同じ表を、ここでは独立に書く）
    const ORDER: [&[usize]; 5] = [&[0], &[0, 1], &[0, 2, 1], &[0, 1, 2, 3], &[0, 2, 1, 3, 4]];
    for (file, reference, channels) in [
        ("ogg-vorbis-3ch.ogg", "ogg-vorbis-3ch.lewton.flac", 3usize),
        ("ogg-vorbis-5ch.ogg", "ogg-vorbis-5ch.lewton.flac", 5),
    ] {
        let (codec, rep, meta, rate, ours) = audio(file, "ogg");
        let (_, _, _, ref_rate, raw) = audio(reference, "flac");
        assert_eq!(codec, "ogg-vorbis-symphonia-dec/3", "{file}");
        assert!(rep, "{file}");
        assert_eq!((rate, ref_rate), (44_100, 44_100), "{file}");
        assert_eq!(meta.tags.get("title").map(String::as_str), Some("Channels"));
        assert_eq!((ours.len(), raw.len()), (channels, channels), "{file}");
        let mut theirs = vec![Vec::new(); channels];
        for (i, p) in raw.into_iter().enumerate() {
            theirs[ORDER[channels - 1][i]] = p;
        }
        for ch in 0..channels {
            let (a, b) = (&ours[ch], &theirs[ch]);
            assert_eq!((a.len(), b.len()), (22_050, 22_050), "{file} の長さ");
            if b.iter().all(|s| *s == 0.0) {
                assert!(
                    a.iter().all(|s| s.abs() < 1.0 / 65_536.0),
                    "{file}：チャンネル {ch} は無音"
                );
                continue;
            }
            let ratio = signal_to_error(&ours[ch..=ch], &theirs[ch..=ch]);
            eprintln!("{file}：チャンネル {ch} の信号/誤差 {ratio:.3e}");
            assert!(
                ratio >= 1e6,
                "{file}：チャンネル {ch} の信号/誤差 {ratio:.3e}"
            );
        }
    }
}

/// 扱わない変種（HE-AAC・Ogg の中の Opus・MP2）は `Unsupported`（次の段のデコーダへ回る）。HE-AAC は、ASC の明示の印
/// （`aac-he.m4a`）でも、帯域の中だけの印（ADTS の `aac-he.aac`、ADTS から写した m4a の `aac-he-implicit.m4a`）でも断る
/// （tests/symphonia_copies.rs）。
#[test]
fn unsupported_audio_variants_fall_through() {
    for (file, format) in [
        ("aac-he.m4a", "m4a"),
        ("aac-he.aac", "aac"),
        ("aac-he-implicit.m4a", "m4a"),
    ] {
        assert!(
            matches!(decode(file, format), Err(EngineError::Unsupported(_))),
            "{file}"
        );
    }
    assert!(matches!(
        decode("ogg-opus.ogg", "ogg"),
        Err(EngineError::Unsupported(_))
    ));
    // 3 チャンネル以上の AAC-LC（symphonia の AAC は 1・2 チャンネルだけ）は、チャンネルの構成があっても、形の段階で
    // 断って後の段（AudioToolbox・ffmpeg）へ回す（形と読みを同じ段が答えるように）
    for (file, format) in [("aac-lc-6ch.aac", "aac"), ("aac-lc-5ch.m4a", "m4a")] {
        let bytes = std::fs::read(dir().join(file)).unwrap();
        let shape = CodecSet::pinned().shape(
            &mut Cursor::new(bytes),
            format,
            &DecodeOptions::default(),
            &Limits::default(),
        );
        assert!(
            matches!(shape, Err(EngineError::Unsupported(_))),
            "{file}: {shape:?}"
        );
        assert!(
            matches!(decode(file, format), Err(EngineError::Unsupported(_))),
            "{file}"
        );
    }
    // MPEG layer II（.mp3 の拡張子の MP2）は空の音声にせず、次の段へ
    assert!(matches!(
        decode("mp2-layer2.mp3", "mp3"),
        Err(EngineError::Unsupported(_))
    ));
}

/// 途中で切れた MP3（LAME のタグは元の長さのまま）は、エラーにせず短く読む。読めた分は元の先頭と同じ値。
#[test]
fn truncated_mp3_reads_shorter() {
    let (_, _, _, _, full) = audio("mp3-lame.mp3", "mp3");
    let bytes = std::fs::read(dir().join("mp3-lame.mp3")).unwrap();
    let (v, _) = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes[..9_000].to_vec()),
            "mp3",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    let Value::Audio(a) = v else { panic!() };
    let n = a.frames();
    assert!(n > 10_000 && n < 44_100, "{n}");
    for (p, f) in a.planes().iter().zip(&full) {
        assert_eq!(&p[..], &f[..n]);
    }
}

/// m4a の編集リスト（elst）が中身の後ろを大きく切っていても、断らずに切って読む（ffmpeg と同じ 11,025 フレーム）。
#[test]
fn short_edit_list_is_applied() {
    let (_, _, _, _, full) = audio("aac-lc-ffmpeg.m4a", "m4a");
    let mut bytes = std::fs::read(dir().join("aac-lc-ffmpeg.m4a")).unwrap();
    let at = bytes.windows(4).position(|w| w == b"elst").unwrap();
    // 版 0：版と印（4）・数（4）のあとに、長さ（4）・始まり（4）
    bytes[at + 12..at + 16].copy_from_slice(&11_025u32.to_be_bytes());
    let (v, _) = CodecSet::pinned()
        .decode(
            &mut Cursor::new(bytes),
            "m4a",
            &DecodeOptions::default(),
            &Limits::default(),
            &NoMonitor,
        )
        .unwrap();
    let Value::Audio(a) = v else { panic!() };
    assert_eq!(a.frames(), 11_025);
    for (p, f) in a.planes().iter().zip(&full) {
        assert_eq!(&p[..], &f[..11_025]);
    }
}

/// ページのグラニュールで長さを 1 フレームに詰めた Ogg Vorbis（8 ch・ブロック 8192 のパケット 2,000 個、4 KB）は、
/// 形は小さくてもデコードする量（切り取る前の長さ。2,000 × 4,096 フレーム × 8 ch × 4 バイト ≒ 262 MB）を上限で
/// 確かめて断る（検証で見つけた、時間だけを使わせる形。上限を 64 MiB にして確かめる）。
#[test]
fn trimmed_away_decoding_is_bounded() {
    let bytes = std::fs::read(dir().join("ogg-vorbis-granule-trim.ogg")).unwrap();
    let limits = Limits {
        max_value_bytes: 64 << 20,
        ..Limits::default()
    };
    let r = CodecSet::pinned().decode(
        &mut Cursor::new(bytes),
        "ogg",
        &DecodeOptions::default(),
        &limits,
        &NoMonitor,
    );
    assert!(matches!(r, Err(EngineError::Limit(_))), "{r:?}");
}
