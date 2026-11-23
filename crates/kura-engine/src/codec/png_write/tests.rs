use std::io::Cursor;
use std::sync::Arc;

use rand_chacha::rand_core::Rng;
use serde_json::json;

use super::*;
use crate::codec::png::PngCodec;
use crate::codec::png_old::{OldPng, OldPngEnc};
use crate::codec::zlib::crc32;
use crate::codec::{Decoder, Encoder};
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::NoMonitor;
use crate::testgen::InputGen;
use crate::value::{Frames, Image, Palette};

const LEVELS: [Level; 3] = [Level::Fast, Level::Balanced, Level::High];

fn write(v: &Value, level: Level) -> Result<Vec<u8>, EngineError> {
    let mut out = Vec::new();
    let n = encode(v, level, &mut out, &NoMonitor)?;
    assert_eq!(n as usize, out.len());
    Ok(out)
}

fn read_with(d: &dyn Decoder, bytes: &[u8]) -> Value {
    d.decode(
        &mut Cursor::new(bytes),
        "png",
        &DecodeOptions::default(),
        &Limits::default(),
        &NoMonitor,
    )
    .unwrap()
}

/// チャンクの並び（種類と中身）。CRC を照らす。
fn chunks(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
    assert_eq!(&bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
    let mut at = 8;
    let mut out = Vec::new();
    while at < bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
        let data = bytes[at + 8..at + 8 + len].to_vec();
        let crc = u32::from_be_bytes(bytes[at + 8 + len..at + 12 + len].try_into().unwrap());
        assert_eq!(
            crc,
            crc32(&bytes[at + 4..at + 8 + len]),
            "{}",
            String::from_utf8_lossy(&kind)
        );
        out.push((kind, data));
        at += 12 + len;
    }
    out
}

fn image(px: Raster, meta: ImageMeta) -> Value {
    Value::Image(Arc::new(Image { px, meta }))
}

fn noise(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Raster {
    let Value::Image(img) = (InputGen::Noise { w, h, fmt, seed }).generate().unwrap() else {
        panic!()
    };
    img.px.clone()
}

/// いろいろな値（すべての形式・パレットの透明の有無・メタデータ・細長い形・フレームの列）。
fn values() -> Vec<(String, Value)> {
    let mut v = Vec::new();
    let mut meta = ImageMeta {
        dpi: Some((300, 96)),
        icc: Some(Arc::new((0..3000u32).map(|i| (i % 7) as u8).collect())),
        exif: Some(Arc::new(crate::codec::exif::minimal(1, false))),
        ..Default::default()
    };
    meta.text.insert("Software".into(), "Kura".into());
    meta.text.insert("Title".into(), "ドット絵".into());
    meta.text.insert("café".into(), "naïve".into());
    for fmt in PixelFormat::ALL {
        v.push((
            format!("{fmt:?}"),
            image(noise(61, 23, fmt, 1), meta.clone()),
        ));
    }
    // 不透明なパレット（tRNS を書かない）と、末尾だけ不透明なパレット
    for (name, alphas) in [
        ("不透明", vec![255u8; 4]),
        ("末尾が不透明", vec![0, 128, 255, 255]),
    ] {
        let colors = alphas
            .iter()
            .enumerate()
            .map(|(i, &a)| [i as u8 * 60, 7, 9, a])
            .collect();
        let rows: Vec<u8> = (0..40 * 9).map(|i| (i % 4) as u8).collect();
        let px = Raster::from_rows(
            40,
            9,
            PixelFormat::Indexed8,
            Some(Palette { colors }),
            &rows,
        )
        .unwrap();
        v.push((format!("パレット {name}"), image(px, ImageMeta::default())));
    }
    v.push((
        "1×1".into(),
        image(noise(1, 1, PixelFormat::Rgba8, 2), ImageMeta::default()),
    ));
    // 帯の境目をまたぐ形（幅の広い行・高い列）
    v.push((
        "幅広".into(),
        image(noise(70_000, 5, PixelFormat::Rgb8, 3), ImageMeta::default()),
    ));
    v.push((
        "縦長".into(),
        image(
            noise(3, 40_000, PixelFormat::Gray16, 4),
            ImageMeta::default(),
        ),
    ));
    let flat = Raster::from_rows(
        500,
        300,
        PixelFormat::Rgba8,
        None,
        &vec![0u8; 500 * 300 * 4],
    )
    .unwrap();
    v.push(("平ら".into(), image(flat, ImageMeta::default())));
    for fmt in [
        PixelFormat::Rgba8,
        PixelFormat::Indexed8,
        PixelFormat::Gray16,
    ] {
        let Value::Frames(fr) = (InputGen::NoiseFrames {
            w: 33,
            h: 17,
            fmt,
            count: 4,
            seed: 5,
        })
        .generate()
        .unwrap() else {
            panic!()
        };
        let frames = fr
            .frames()
            .iter()
            .zip([0u32, 40, 70_000, 9_000_000])
            .map(|(f, d)| (f.px.clone(), d))
            .collect();
        let fr = Frames::full(frames, meta.clone(), 3).unwrap();
        v.push((format!("APNG {fmt:?}"), Value::Frames(Arc::new(fr))));
    }
    v
}

/// どの段で書いても、前の書き手（png クレート）で書いたものと同じ値に読める。png クレートの読み手（前の読み口）でも同じ値。
#[test]
fn writes_the_same_value_as_the_png_crate() {
    for (name, v) in values() {
        let mut old = Vec::new();
        OldPngEnc
            .encode(&v, "png", &json!({}), &mut old, &NoMonitor)
            .unwrap();
        let want = read_with(&PngCodec, &old);
        for level in LEVELS {
            let bytes = write(&v, level).unwrap();
            assert!(read_with(&PngCodec, &bytes) == want, "{name} {level:?}");
            assert!(
                read_with(&OldPng, &bytes) == want,
                "{name} {level:?}：png クレートで読む"
            );
        }
    }
}

/// チャンクの並びは前の書き手と同じ（IHDR・pHYs・iCCP・eXIf・acTL・PLTE・tRNS・テキスト・fcTL と IDAT・fdAT・IEND）。
/// fcTL・fdAT の番号は 0 から続き、既定の画像が最初のフレーム（dispose NONE・blend SOURCE）。
#[test]
fn chunk_order_and_sequence_numbers() {
    let (_, v) = values()
        .into_iter()
        .find(|(n, _)| n == "APNG Indexed8")
        .unwrap();
    let bytes = write(&v, Level::Balanced).unwrap();
    let cs = chunks(&bytes);
    let kinds: Vec<String> = cs
        .iter()
        .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
        .collect();
    let mut want = vec![
        "IHDR", "pHYs", "iCCP", "eXIf", "acTL", "PLTE", "tRNS", "tEXt", "iTXt", "iTXt",
    ];
    want.extend([
        "fcTL", "IDAT", "fcTL", "fdAT", "fcTL", "fdAT", "fcTL", "fdAT", "IEND",
    ]);
    assert_eq!(kinds, want);
    let mut seq = 0u32;
    for (k, d) in &cs {
        if k == b"fcTL" || k == b"fdAT" {
            assert_eq!(u32::from_be_bytes(d[..4].try_into().unwrap()), seq);
            seq += 1;
        }
        if k == b"fcTL" {
            assert_eq!(&d[4..12], &[0, 0, 0, 33, 0, 0, 0, 17]);
            assert_eq!(&d[12..20], &[0; 8]);
            assert_eq!(&d[24..26], &[0, 0], "dispose NONE・blend SOURCE");
        }
        if k == b"acTL" {
            assert_eq!(d, &[0, 0, 0, 4, 0, 0, 0, 3]);
        }
        if k == b"pHYs" {
            // 300 dpi → 11,811 ppm、96 dpi → 3,780 ppm（四捨五入）
            assert_eq!(d, &[0, 0, 0x2e, 0x23, 0, 0, 0x0e, 0xc4, 1]);
        }
        if k == b"iCCP" {
            assert_eq!(&d[..3], b"_\0\0");
        }
    }
    // 表示時間の分数
    let delays: Vec<[u8; 4]> = cs
        .iter()
        .filter(|(k, _)| k == b"fcTL")
        .map(|(_, d)| d[20..24].try_into().unwrap())
        .collect();
    assert_eq!(
        delays,
        vec![
            [0, 0, 3, 232],
            [0, 40, 3, 232],
            [27, 88, 0, 100],
            [35, 40, 0, 1]
        ]
    );
    // テキスト：ASCII は tEXt、ほかは圧縮しない iTXt（キーは Latin-1）
    let texts: Vec<&Vec<u8>> = cs
        .iter()
        .filter(|(k, _)| k == b"tEXt" || k == b"iTXt")
        .map(|(_, d)| d)
        .collect();
    assert_eq!(texts[0], &b"Software\0Kura".to_vec());
    assert_eq!(
        texts[1],
        &[b"Title\0\0\0\0\0".as_slice(), "ドット絵".as_bytes()].concat()
    );
    assert_eq!(
        texts[2],
        &[b"caf\xe9\0\0\0\0\0".as_slice(), "naïve".as_bytes()].concat()
    );
}

/// テキストのキーは Latin-1 で 1〜79 バイト（前の書き手と同じ。ほかは何も書かずに断る）。
#[test]
fn text_keywords_follow_the_png_crate() {
    for (key, ok) in [
        ("a", true),
        (&"k".repeat(79)[..], true),
        ("ÿ", true),
        ("", false),
        (&"k".repeat(80)[..], false),
        ("キー", false),
    ] {
        let mut meta = ImageMeta::default();
        meta.text.insert(key.to_string(), "v".into());
        let v = image(noise(3, 3, PixelFormat::Rgb8, 6), meta);
        let mut out = Vec::new();
        let r = encode(&v, Level::Fast, &mut out, &NoMonitor);
        assert_eq!(r.is_ok(), ok, "{key}");
        let mut old = Vec::new();
        assert_eq!(
            OldPngEnc
                .encode(&v, "png", &json!({}), &mut old, &NoMonitor)
                .is_ok(),
            ok,
            "{key}：前の書き手"
        );
        if !ok {
            assert!(matches!(r, Err(EngineError::Encode { .. })), "{r:?}");
            assert!(out.is_empty(), "断るときは何も書かない");
        }
    }
}

/// 画像のデータは IDAT_MAX バイトずつのチャンク（最後だけ短い）。fdAT も同じ（番号の 4 バイトを除いて）。
#[test]
fn image_data_is_split_into_fixed_chunks() {
    let px = noise(1100, 1000, PixelFormat::Rgb8, 7);
    let fr = Frames::full(
        vec![(px.clone(), 10), (px.clone(), 10)],
        ImageMeta::default(),
        0,
    )
    .unwrap();
    let bytes = write(&Value::Frames(Arc::new(fr)), Level::Fast).unwrap();
    let cs = chunks(&bytes);
    for kind in [b"IDAT", b"fdAT"] {
        let sizes: Vec<usize> = cs
            .iter()
            .filter(|(k, _)| k == kind)
            .map(|(k, d)| d.len() - if k == b"fdAT" { 4 } else { 0 })
            .collect();
        assert!(sizes.len() >= 3, "{sizes:?}");
        let (last, full) = sizes.split_last().unwrap();
        assert!(
            full.iter().all(|&n| n == IDAT_MAX) && *last <= IDAT_MAX && *last > 0,
            "{sizes:?}"
        );
    }
}

/// 2³¹ − 1 を超える繰り返しは PNG の整数の上限に詰める（そのまま書くと読めないファイルになる）。
#[test]
fn huge_loop_counts_are_clamped() {
    let px = noise(4, 3, PixelFormat::Rgb8, 12);
    for plays in [0x7FFF_FFFFu32, 0x8000_0000, u32::MAX] {
        let fr = Frames::full(vec![(px.clone(), 10)], ImageMeta::default(), plays).unwrap();
        let bytes = write(&Value::Frames(Arc::new(fr)), Level::Fast).unwrap();
        let Value::Frames(back) = read_with(&PngCodec, &bytes) else {
            panic!()
        };
        assert_eq!(back.loop_count, 0x7FFF_FFFF, "{plays}");
    }
}

/// 止める指示があれば Canceled。
#[test]
fn canceled_encoding_stops() {
    struct Stop;
    impl crate::monitor::Monitor for Stop {
        fn canceled(&self) -> bool {
            true
        }
    }
    let v = image(noise(50, 50, PixelFormat::Rgba8, 8), ImageMeta::default());
    let mut out = Vec::new();
    assert!(matches!(
        encode(&v, Level::Balanced, &mut out, &Stop),
        Err(EngineError::Canceled)
    ));
}

/// 乱数の大きさ・形式・中身の画像は、どの段でも往復して同じ値になり、png クレートの読み手でも同じ値に読める。
#[test]
fn random_images_round_trip() {
    let mut rng = crate::determinism::rng(9);
    for i in 0..60 {
        let w = 1 + rng.next_u32() % 300;
        let h = 1 + rng.next_u32() % 60;
        let fmt = PixelFormat::ALL[rng.next_u32() as usize % PixelFormat::ALL.len()];
        let mut px = noise(w, h, fmt, u64::from(rng.next_u32()));
        if i % 2 == 0 {
            // 平らな帯と繰り返しを混ぜる
            let mut rows = px.to_rows().unwrap();
            let n = rows.len();
            let k = rng.next_u32() as usize % n;
            rows[k..].iter_mut().for_each(|b| *b = 0);
            px = Raster::from_rows(w, h, fmt, px.palette().map(|p| (**p).clone()), &rows).unwrap();
        }
        let v = image(px, ImageMeta::default());
        for level in LEVELS {
            let bytes = write(&v, level).unwrap();
            assert!(read_with(&PngCodec, &bytes) == v, "{i} {fmt:?} {level:?}");
            assert!(
                read_with(&OldPng, &bytes) == v,
                "{i} {fmt:?} {level:?}：png クレート"
            );
        }
    }
}

/// 書き出しのコーパス（手で回す）：`KURA_PNGW_CORPUS=<フォルダ>[@<最小のバイト数>]:…` の PNG を読んで値にし、群（フォルダの名前）ごとに
/// 並べる。ドット絵（一括の計測 NFR-14 と同じ 1,000 件）の群も足す。
#[allow(clippy::disallowed_methods)]
fn corpus() -> Vec<(String, Vec<Value>)> {
    let mut groups = Vec::new();
    let Ok(spec) = std::env::var("KURA_PNGW_CORPUS") else {
        return groups;
    };
    let opts = DecodeOptions::default();
    for part in spec.split(':').filter(|p| !p.is_empty()) {
        let (dir, min) = match part.rsplit_once('@') {
            Some((d, m)) => (d, m.parse::<u64>().unwrap_or(0)),
            None => (part, 0),
        };
        let mut files = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|x| x.to_str()) == Some("png")
                    && e.metadata().is_ok_and(|m| m.len() >= min)
                {
                    files.push(p);
                }
            }
        }
        files.sort();
        let mut values = Vec::new();
        for p in &files {
            let Ok(data) = std::fs::read(p) else {
                continue;
            };
            if let Ok(v) = PngCodec.decode(
                &mut Cursor::new(&data),
                "png",
                &opts,
                &Limits::default(),
                &NoMonitor,
            ) {
                values.push(v);
            }
        }
        let name = std::path::Path::new(dir)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(dir)
            .to_string();
        groups.push((name, values));
    }
    let sprites = crate::testing::bench::sprite_batch(1000, 1)
        .iter()
        .filter_map(|g| g.generate().ok())
        .collect();
    groups.push(("sprites".to_string(), sprites));
    groups
}

/// 書いた大きさ（バイト）と時間（ms）。
type Encode<'a> = &'a dyn Fn(&Value, &serde_json::Value) -> Vec<u8>;

/// 大きさと速さ（手で回す。`KURA_PNGW_CORPUS=… cargo test --release -p kura-engine --lib png_write_corpus -- --ignored --nocapture`）：
/// 群ごと・段（fast・balanced・high）ごとに、前の書き手（png クレート）と今の書き手の大きさの合計と時間を出す。
/// 今の書き手で書いたものは読み直して、値（ダイジェスト）が同じかを照らす。`KURA_PNGW_ROUNDS`（既定 1）回の最短の時間。
#[test]
#[ignore]
// 手で回す確かめ：フォルダの場所（環境変数）と時間を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn png_write_corpus() {
    let groups = corpus();
    if groups.is_empty() {
        eprintln!("KURA_PNGW_CORPUS がないので見送ります");
        return;
    }
    let rounds: usize = std::env::var("KURA_PNGW_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let only_old = std::env::var("KURA_PNGW_OLD_ONLY").is_ok();
    let old = |v: &Value, o: &serde_json::Value| {
        let mut out = Vec::new();
        OldPngEnc.encode(v, "png", o, &mut out, &NoMonitor).unwrap();
        out
    };
    let new = |v: &Value, o: &serde_json::Value| {
        let mut out = Vec::new();
        PngCodec.encode(v, "png", o, &mut out, &NoMonitor).unwrap();
        out
    };
    let only_new = std::env::var("KURA_PNGW_NEW_ONLY").is_ok();
    let mut encoders: Vec<(&str, Encode)> = Vec::new();
    if !only_new {
        encoders.push(("前", &old));
    }
    if !only_old {
        encoders.push(("今", &new));
    }
    let tiers: Vec<String> = std::env::var("KURA_PNGW_TIERS")
        .map(|t| t.split(',').map(str::to_string).collect())
        .unwrap_or_else(|_| vec!["fast".into(), "balanced".into(), "high".into()]);
    let opts = DecodeOptions::default();
    for (name, values) in &groups {
        let raw: u64 = values
            .iter()
            .map(|v| crate::budget::value_bytes(&v.shape()))
            .sum();
        eprintln!(
            "ZZGROUP {name} {} 件・値 {:.1} MB",
            values.len(),
            raw as f64 / 1e6
        );
        for tier in &tiers {
            let tier = tier.as_str();
            let o = json!({ "compression": tier });
            for (who, enc) in &encoders {
                let mut best = f64::MAX;
                let mut size = 0u64;
                let mut bad = 0usize;
                for round in 0..rounds {
                    let s = std::time::Instant::now();
                    let outs: Vec<Vec<u8>> = values.iter().map(|v| enc(v, &o)).collect();
                    best = best.min(s.elapsed().as_secs_f64() * 1e3);
                    if round == 0 {
                        size = outs.iter().map(|b| b.len() as u64).sum();
                        if *who == "今" {
                            for (v, b) in values.iter().zip(&outs) {
                                let back = PngCodec.decode(
                                    &mut Cursor::new(b),
                                    "png",
                                    &opts,
                                    &Limits::default(),
                                    &NoMonitor,
                                );
                                if back.ok().and_then(|b| b.digest().ok()) != v.digest().ok() {
                                    bad += 1;
                                }
                            }
                        }
                    }
                }
                eprintln!(
                    "ZZTIER {name} {tier:8} {who} {size:>12} B {best:>9.1} ms {:>7.1} MB/s 違い {bad}",
                    raw as f64 / 1e3 / best.max(1e-9)
                );
            }
        }
    }
}

/// 書き出しのピークのメモリ（手で回す。`/usr/bin/time -l` の下で
/// `KURA_PNGW_PEAK=fast|balanced|high|none cargo test --release -p kura-engine --lib png_write_peak -- --ignored`）：
/// 4096² の乱数の Rgba8 を捨てる書き出し先に書く。`none` は値を作るだけ（引く基準）。
#[test]
#[ignore]
// 手で回す確かめ：段を環境変数で読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn png_write_peak() {
    let Ok(which) = std::env::var("KURA_PNGW_PEAK") else {
        return;
    };
    let v = image(
        noise(4096, 4096, PixelFormat::Rgba8, 11),
        ImageMeta::default(),
    );
    let level = match which.as_str() {
        "fast" => Level::Fast,
        "balanced" => Level::Balanced,
        "high" => Level::High,
        _ => return,
    };
    let n = encode(&v, level, &mut std::io::sink(), &NoMonitor).unwrap();
    eprintln!("ZZPEAK {which} {n}");
}
