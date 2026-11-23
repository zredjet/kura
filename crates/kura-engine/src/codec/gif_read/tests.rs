//! GIF の読み口の試験。照合の相手は前の読み手（試験だけの `codec::gif_old`。gif クレート 0.14.2）。

use std::io::Cursor;

use rand_chacha::rand_core::Rng;

use crate::codec::Decoder;
use crate::codec::gif::GifCodec;
use crate::codec::gif_old::OldGif;
use crate::doc::DecodeOptions;
use crate::error::EngineError;
use crate::limits::Limits;
use crate::monitor::NoMonitor;
use crate::shape::Shape;
use crate::testfiles::{FileGen, GifFrame};
use crate::value::Value;

fn read(d: &dyn Decoder, bytes: &[u8], limits: &Limits) -> Result<(Shape, Value), EngineError> {
    crate::testing::catch_own_panic(|| {
        let s = d.shape(
            &mut Cursor::new(bytes.to_vec()),
            "gif",
            &DecodeOptions::default(),
            limits,
        )?;
        limits.check(&s).map_err(EngineError::Limit)?;
        let v = d.decode(
            &mut Cursor::new(bytes.to_vec()),
            "gif",
            &DecodeOptions::default(),
            limits,
            &NoMonitor,
        )?;
        Ok((s, v))
    })
    .unwrap_or_else(|at| panic!("panic：{at}"))
}

/// 照らした数。
#[derive(Debug, Default)]
struct Tally {
    same: usize,
    both_err: usize,
    /// 前は読めず、今は読める（途中で切れたものなど）。
    newly_read: usize,
}

/// 前に読めたものは今も同じ形と値で読む。
fn compare(bytes: &[u8], limits: &Limits, what: &str, t: &mut Tally) {
    let old = read(&OldGif, bytes, limits);
    let new = read(&GifCodec, bytes, limits);
    match (old, new) {
        (Ok((os, ov)), Ok((ns, nv))) => {
            assert_eq!(os, ns, "{what}：形が違う");
            assert!(ov == nv, "{what}：値が違う");
            t.same += 1;
        }
        (Ok(_), Err(e)) => panic!("{what}：前は読めたのに断る：{e}"),
        (Err(_), Ok(_)) => t.newly_read += 1,
        (Err(_), Err(_)) => t.both_err += 1,
    }
}

/// 乱数の GIF の作り方（形式・パレット・局所パレット・透明・dispose・インターレース・はみ出し・繰り返し）。
fn random_gif(rng: &mut rand_chacha::ChaCha8Rng) -> FileGen {
    let w = 1 + (rng.next_u32() % 70) as u16;
    let h = 1 + (rng.next_u32() % 50) as u16;
    let global_len = [0u16, 1, 2, 3, 5, 16, 100, 255, 256][rng.next_u32() as usize % 9];
    let n = 1 + rng.next_u32() % 4;
    let frames = (0..n)
        .map(|_| {
            let local_len = if global_len == 0 || rng.next_u32().is_multiple_of(4) {
                1 + (rng.next_u32() % 256) as u16
            } else {
                0
            };
            let len = if local_len > 0 { local_len } else { global_len };
            GifFrame {
                left: (rng.next_u32() % u32::from(w + 10)) as u16,
                top: (rng.next_u32() % u32::from(h + 10)) as u16,
                w: 1 + (rng.next_u32() % u32::from(w + 5)) as u16,
                h: 1 + (rng.next_u32() % u32::from(h + 5)) as u16,
                local_len,
                transparent: (rng.next_u32().is_multiple_of(2))
                    .then(|| (rng.next_u32() % u32::from(len + 2)) as u8),
                dispose: (rng.next_u32() % 5) as u8,
                delay: (rng.next_u32() % 300) as u16,
                interlaced: rng.next_u32().is_multiple_of(3),
            }
        })
        .collect();
    let repeat = match rng.next_u32() % 4 {
        0 => None,
        1 => Some(0),
        _ => Some((rng.next_u32() % 5) as u16 + 1),
    };
    FileGen::Gif {
        w,
        h,
        global_len,
        repeat,
        frames,
        seed: u64::from(rng.next_u32()),
    }
}

/// 乱数で作った正しい GIF は、前の読み手と同じ形と値。
#[test]
fn generated_gifs_match_the_old_reader() {
    let mut rng = crate::determinism::rng(1);
    let mut t = Tally::default();
    for i in 0..1500 {
        let g = random_gif(&mut rng);
        let bytes = g.bytes().unwrap();
        compare(&bytes, &Limits::default(), &format!("{i} {g:?}"), &mut t);
    }
    assert!(t.same > 1400, "{t:?}");
}

/// 壊したもの（バイトの差し替え・切り詰め・挿入・削除・サブブロックの長さ・後ろのごみ・終わりを外す）：前に読めたものは
/// 同じ値。
#[test]
fn mutated_gifs_match_the_old_reader() {
    let mut rng = crate::determinism::rng(2);
    let mut t = Tally::default();
    let tight = Limits {
        max_pixels: 1 << 12,
        max_frames: 3,
        max_meta_bytes: 64,
        ..Limits::default()
    };
    for i in 0..4000 {
        let g = random_gif(&mut rng);
        let mut b = g.bytes().unwrap();
        let k = rng.next_u32() % 7;
        match k {
            0 => {
                for _ in 0..1 + rng.next_u32() % 3 {
                    let at = rng.next_u32() as usize % b.len();
                    b[at] = rng.next_u32() as u8;
                }
            }
            1 => {
                let at = rng.next_u32() as usize % b.len();
                b.truncate(at);
            }
            2 => {
                let at = rng.next_u32() as usize % b.len();
                b.insert(at, rng.next_u32() as u8);
            }
            3 => {
                let at = rng.next_u32() as usize % b.len();
                b.remove(at);
            }
            4 => b.extend((0..rng.next_u32() % 20).map(|_| rng.next_u32() as u8)),
            5 => {
                b.pop();
            }
            _ => {
                // 拡張・知らないブロックを差し込む
                let at = 13 + rng.next_u32() as usize % (b.len().saturating_sub(13)).max(1);
                let ext: Vec<u8> = match rng.next_u32() % 5 {
                    0 => vec![0x21, 0xFE, 3, b'a', b'b', b'c', 0],
                    1 => vec![0x21, 0xF9, 4, 9, 7, 0, 3, 0],
                    2 => {
                        let mut v = vec![0x21, 0xFF, 11];
                        v.extend_from_slice(b"NETSCAPE2.0");
                        v.extend_from_slice(&[3, 1, 5, 0, 0]);
                        v
                    }
                    3 => {
                        let mut v = vec![0x21, 0xFF, 11];
                        v.extend_from_slice(b"ICCRGBG1012");
                        v.extend_from_slice(&[4, 1, 2, 3, 4, 0]);
                        v
                    }
                    _ => vec![0x21, 0x42, 1, 0, 0],
                };
                let at = at.min(b.len());
                b.splice(at..at, ext);
            }
        }
        let limits = if i % 5 == 0 {
            tight.clone()
        } else {
            Limits::default()
        };
        compare(&b, &limits, &format!("{i} 壊し方 {k} {g:?}"), &mut t);
    }
    assert!(t.same > 500 && t.both_err > 100, "{t:?}");
    eprintln!("ZZMUTATE {t:?}");
}

/// 途中で切れた GIF はブラウザのように読む：終わりのブロックがなくても同じ値、フレームのデータが途中で終われば届いた画素まで
/// 描き（残りは前のキャンバスのまま）、記述子が切れたフレームは数えない。
#[test]
fn truncated_gifs_are_read_like_browsers() {
    let g = FileGen::Gif {
        w: 40,
        h: 30,
        global_len: 16,
        repeat: Some(0),
        frames: vec![
            GifFrame {
                left: 0,
                top: 0,
                w: 40,
                h: 30,
                local_len: 0,
                transparent: None,
                dispose: 1,
                delay: 5,
                interlaced: false,
            },
            GifFrame {
                left: 5,
                top: 5,
                w: 30,
                h: 20,
                local_len: 0,
                transparent: None,
                dispose: 1,
                delay: 5,
                interlaced: false,
            },
        ],
        seed: 4,
    };
    let full = g.bytes().unwrap();
    let limits = Limits::default();
    let (_, want) = read(&GifCodec, &full, &limits).unwrap();
    // 終わりのブロックがない：同じ値（前は読めなかった）
    let no_trailer = &full[..full.len() - 1];
    assert!(read(&OldGif, no_trailer, &limits).is_err());
    let (_, v) = read(&GifCodec, no_trailer, &limits).unwrap();
    assert!(v == want);
    // 2 つ目のフレームのデータの途中で切る：2 フレームのまま、2 つ目は届いた画素まで
    let Value::Frames(want_fr) = &want else {
        panic!()
    };
    let cut = full.len() - 40;
    let (_, v) = read(&GifCodec, &full[..cut], &limits).unwrap();
    let Value::Frames(fr) = v else { panic!() };
    assert_eq!(fr.frames().len(), 2);
    assert!(fr.frames()[0].px == want_fr.frames()[0].px);
    let a = fr.frames()[1].px.to_rows().unwrap();
    let b = want_fr.frames()[1].px.to_rows().unwrap();
    let first = want_fr.frames()[0].px.to_rows().unwrap();
    assert_ne!(a, b, "途中までしか描いていない");
    // 描いたところは全部のものと同じ、描いていないところは 1 つ目のフレームのまま
    for (i, ((&x, &y), &f)) in a.iter().zip(&b).zip(&first).enumerate() {
        assert!(x == y || x == f, "{i}");
    }
    // 記述子の途中で切る：そのフレームは数えない（1 フレームと繰り返しの拡張なので、フレームの列のまま 1 つ）。2 つ目の
    // フレームは、1 つ目だけの GIF（終わりのブロックを除く）の直後の GCE（8 バイト）のあとに始まる
    let FileGen::Gif {
        w,
        h,
        global_len,
        repeat,
        frames,
        seed,
    } = g.clone()
    else {
        panic!()
    };
    let one = FileGen::Gif {
        w,
        h,
        global_len,
        repeat,
        frames: frames[..1].to_vec(),
        seed,
    }
    .bytes()
    .unwrap();
    let desc_at = one.len() - 1 + 8;
    assert_eq!(full[desc_at], 0x2C);
    let (_, v) = read(&GifCodec, &full[..desc_at + 4], &limits).unwrap();
    let Value::Frames(fr) = v else { panic!() };
    assert_eq!(fr.frames().len(), 1);
}

/// 前の gif クレートと同じく、データの最初のサブブロックの長さが 0 なら次のバイトから長さとして読み直す（`m 00 00` は
/// データなし、`m 00 03 …` は 3 バイトのサブブロック）。
#[test]
fn an_empty_first_sub_block_is_read_like_the_gif_crate() {
    let mut b = Vec::new();
    b.extend_from_slice(b"GIF89a");
    b.extend_from_slice(&[2, 0, 1, 0, 0x80, 0, 0]);
    b.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    b.extend_from_slice(&[0x2C, 0, 0, 0, 0, 2, 0, 1, 0, 0]);
    // m = 2、L0 = 0、そのあと 1 バイトのサブブロックに CLEAR 1 0 END（3 ビットずつ）
    let mut z = Vec::new();
    assert!(crate::codec::lzw::encode(&[1, 0], 2, &mut z));
    b.extend_from_slice(&[2, 0, z.len() as u8]);
    b.extend_from_slice(&z);
    b.extend_from_slice(&[0, 0x3B]);
    let limits = Limits::default();
    let (_, old) = read(&OldGif, &b, &limits).unwrap();
    let (_, new) = read(&GifCodec, &b, &limits).unwrap();
    assert!(old == new);
}

/// 前の読み手と同じく、表にない LZW の符号・知らないブロック・知らない拡張・GCE の長さの誤り・最小の符号の長さの範囲の外は誤り。
#[test]
fn malformed_streams_are_errors_as_before() {
    let g = FileGen::Gif {
        w: 8,
        h: 8,
        global_len: 4,
        repeat: None,
        frames: vec![GifFrame {
            left: 0,
            top: 0,
            w: 8,
            h: 8,
            local_len: 0,
            transparent: None,
            dispose: 0,
            delay: 0,
            interlaced: false,
        }],
        seed: 1,
    };
    let full = g.bytes().unwrap();
    let limits = Limits::default();
    let at = full.iter().position(|&b| b == 0x2C).unwrap();
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut b = full.clone();
    b[at + 10] = 0; // 最小の符号の長さ 0
    cases.push(("最小の符号の長さ", b));
    let mut b = full.clone();
    b.splice(at..at, [0x99]);
    cases.push(("知らないブロック", b));
    let mut b = full.clone();
    b.splice(at..at, [0x21, 0x42, 1, 0, 0]);
    cases.push(("知らない拡張", b));
    let mut b = full.clone();
    b.splice(at..at, [0x21, 0xF9, 3, 0, 0, 0, 0]);
    cases.push(("GCE の長さ", b));
    for (what, b) in cases {
        assert!(read(&OldGif, &b, &limits).is_err(), "{what}");
        assert!(read(&GifCodec, &b, &limits).is_err(), "{what}");
    }
}

/// コーパス（手で回す。`KURA_GIF_CORPUS=<フォルダ> cargo test --release -p kura-engine --lib gif_corpus -- --ignored
/// --nocapture`）：フォルダの下の .gif をすべて前の読み手と照らす。それぞれを切り詰めたもの（いくつかの位置）も照らす。
#[test]
#[ignore]
// 手で回す確かめ：フォルダの場所（環境変数）と時間を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn gif_corpus_matches_the_gif_crate() {
    let Ok(root) = std::env::var("KURA_GIF_CORPUS") else {
        eprintln!("KURA_GIF_CORPUS がないので見送ります");
        return;
    };
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(root)];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("gif"))
            {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut t = Tally::default();
    let mut cut = Tally::default();
    let (mut t_new, mut t_old) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let limits = Limits::default();
    for p in &files {
        let Ok(data) = std::fs::read(p) else {
            continue;
        };
        let before = (t.both_err, t.newly_read);
        compare(&data, &limits, &p.display().to_string(), &mut t);
        if (t.both_err, t.newly_read) != before {
            let o = read(&OldGif, &data, &limits).err().map(|e| e.to_string());
            let n = read(&GifCodec, &data, &limits).err().map(|e| e.to_string());
            eprintln!("ZZFILE {} 前 {o:?} 今 {n:?}", p.display());
        }
        for k in [1usize, 2, 3, 5] {
            let at = data.len() * k / 7;
            compare(
                &data[..at],
                &limits,
                &format!("{} を {at} で切る", p.display()),
                &mut cut,
            );
        }
        for (d, acc) in [
            (&GifCodec as &dyn Decoder, &mut t_new),
            (&OldGif as &dyn Decoder, &mut t_old),
        ] {
            let s = std::time::Instant::now();
            let _ = read(d, &data, &limits);
            *acc += s.elapsed();
        }
    }
    eprintln!(
        "ZZCORPUS {} 本 {t:?} 切ったもの {cut:?} 読みの合計：新 {:.1} ms・前 {:.1} ms",
        files.len(),
        t_new.as_secs_f64() * 1e3,
        t_old.as_secs_f64() * 1e3
    );
}

/// 壊したものの長い照らし（手で回す。`KURA_GIF_SOAK=<回数> KURA_GIF_CORPUS=<フォルダ> cargo test --release -p kura-engine
/// --lib gif_soak -- --ignored --nocapture`）：コーパスと乱数の GIF を種に、`mutated_gifs_match_the_old_reader` と同じ壊し方で
/// 照らす。前に読めたものは同じ値（インターレースのフレームを持つものの値の違いは、数えて出す）。
#[test]
#[ignore]
// 手で回す確かめ：回数とフォルダ（環境変数）を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn gif_soak() {
    let Some(n) = std::env::var("KURA_GIF_SOAK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    let mut seeds: Vec<Vec<u8>> = Vec::new();
    if let Ok(root) = std::env::var("KURA_GIF_CORPUS") {
        let mut stack = vec![std::path::PathBuf::from(root)];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|x| x.to_str()) == Some("gif")
                    && let Ok(d) = std::fs::read(&p)
                    && d.len() < 200_000
                {
                    seeds.push(d);
                }
            }
        }
    }
    let mut rng = crate::determinism::rng(77);
    for _ in 0..200 {
        seeds.push(random_gif(&mut rng).bytes().unwrap());
    }
    let mut t = Tally::default();
    let mut differ = 0usize;
    let limits = Limits::default();
    for i in 0..n {
        let mut b = seeds[rng.next_u32() as usize % seeds.len()].clone();
        for _ in 0..1 + rng.next_u32() % 3 {
            if b.is_empty() {
                break;
            }
            let at = rng.next_u32() as usize % b.len();
            match rng.next_u32() % 5 {
                0 => b[at] = rng.next_u32() as u8,
                1 => b.truncate(at.max(13)),
                2 => b.insert(at, rng.next_u32() as u8),
                3 => {
                    b.remove(at);
                }
                _ => b[at] ^= 1 << (rng.next_u32() % 8),
            }
        }
        let old = read(&OldGif, &b, &limits);
        let new = read(&GifCodec, &b, &limits);
        match (old, new) {
            (Ok((os, ov)), Ok((ns, nv))) => {
                assert_eq!(os, ns, "{i}：形が違う");
                if ov == nv {
                    t.same += 1;
                } else {
                    differ += 1;
                    eprintln!("ZZDIFFER {i} {} バイト", b.len());
                }
            }
            (Ok(_), Err(e)) => panic!("{i}：前は読めたのに断る：{e}"),
            (Err(_), Ok(_)) => t.newly_read += 1,
            (Err(_), Err(_)) => t.both_err += 1,
        }
    }
    eprintln!("ZZSOAK {n} 回 {t:?} 値の違い {differ}");
}

/// 1 フレームの GIF（40×30、大域パレット 16 色）を、画面いっぱいで透明なし・dispose Keep で作る。`local` なら局所パレット。
fn one_frame(local: bool) -> Vec<u8> {
    FileGen::Gif {
        w: 40,
        h: 30,
        global_len: if local { 0 } else { 16 },
        repeat: None,
        frames: vec![GifFrame {
            left: 0,
            top: 0,
            w: 40,
            h: 30,
            local_len: if local { 16 } else { 0 },
            transparent: None,
            dispose: 1,
            delay: 0,
            interlaced: false,
        }],
        seed: 6,
    }
    .bytes()
    .unwrap()
}

/// 見直しの指摘：途中で切れた最初のフレームは、大域パレットでも届かなかった画素が透明（Indexed8 なら消す色を足す）。局所
/// パレット（Rgba8）と同じ見え方。
#[test]
fn a_truncated_first_frame_leaves_transparent_pixels() {
    let limits = Limits::default();
    for local in [false, true] {
        let full = one_frame(local);
        let cut = &full[..full.len() - 300];
        let (_, v) = read(&GifCodec, cut, &limits).unwrap();
        let Value::Image(img) = v else { panic!() };
        let rgba = crate::imaging::convert::to_rgba8_rows(&img.px).unwrap();
        let last = &rgba[rgba.len() - 4..];
        assert_eq!(last, &[0, 0, 0, 0], "local {local}");
        let (_, whole) = read(&GifCodec, &full, &limits).unwrap();
        let Value::Image(whole) = whole else { panic!() };
        let want = crate::imaging::convert::to_rgba8_rows(&whole.px).unwrap();
        // 描いた画素は全部のものと同じ
        assert_eq!(&rgba[..400], &want[..400], "local {local}");
    }
}

/// 見直しの指摘：データが終わりまであるのに画素が足りないフレームは、前と同じく誤り（途中で切れたファイルだけを読む）。
#[test]
fn a_complete_frame_with_too_few_pixels_is_an_error() {
    let mut b = Vec::new();
    b.extend_from_slice(b"GIF89a");
    b.extend_from_slice(&[4, 0, 4, 0, 0x80, 0, 0]);
    b.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    b.extend_from_slice(&[0x2C, 0, 0, 0, 0, 4, 0, 4, 0, 0, 2]);
    let mut z = Vec::new();
    assert!(crate::codec::lzw::encode(&[1, 0, 1], 2, &mut z));
    b.push(z.len() as u8);
    b.extend_from_slice(&z);
    b.extend_from_slice(&[0, 0x3B]);
    let limits = Limits::default();
    assert!(read(&OldGif, &b, &limits).is_err());
    assert!(read(&GifCodec, &b, &limits).is_err());
}

/// 見直しの指摘：画面より大きなフレームの小さなファイルでも、フレーム全体の置き場を作らない（1 行ずつ展開する）。
#[test]
// 時間で確かめる（値には効かない）
#[allow(clippy::disallowed_methods)]
fn huge_frames_in_tiny_files_are_cheap() {
    let mut b = Vec::new();
    b.extend_from_slice(b"GIF89a");
    b.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0]);
    b.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    for _ in 0..200 {
        // 16384×16384・インターレースのフレーム。データは clear だけで、終わりがない（途中で切れたものとして読む）
        b.extend_from_slice(&[0x2C, 0, 0, 0, 0, 0, 0x40, 0, 0x40, 0x40, 2, 1, 0x04, 0]);
    }
    b.extend_from_slice(&[0x3B]);
    let limits = Limits::default();
    let s = std::time::Instant::now();
    // データが終わりまであって画素が足りないので誤り。すぐに断る
    assert!(read(&GifCodec, &b, &limits).is_err());
    assert!(
        s.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        s.elapsed()
    );
    // 最後のフレームのデータの途中で切る：200 フレームの 1×1 の画像の列として読める
    let cut = &b[..b.len() - 2];
    let s = std::time::Instant::now();
    let r = read(&GifCodec, cut, &limits);
    assert!(
        s.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        s.elapsed()
    );
    assert!(r.is_err(), "途中のフレームのデータが足りない");
}

/// 見直しの指摘：ICC の拡張が 2 つあり前のものが上限を超えても、残った方が収まれば読む（前と同じ）。
#[test]
fn only_the_remaining_icc_counts_against_the_limit() {
    let mut b = Vec::new();
    b.extend_from_slice(b"GIF89a");
    b.extend_from_slice(&[1, 0, 1, 0, 0x80, 0, 0]);
    b.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
    for len in [200usize, 10] {
        b.extend_from_slice(&[0x21, 0xFF, 11]);
        b.extend_from_slice(b"ICCRGBG1012");
        b.push(len as u8);
        b.extend(std::iter::repeat_n(7u8, len));
        b.push(0);
    }
    let mut z = Vec::new();
    assert!(crate::codec::lzw::encode(&[1], 2, &mut z));
    b.extend_from_slice(&[0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, z.len() as u8]);
    b.extend_from_slice(&z);
    b.extend_from_slice(&[0, 0x3B]);
    let limits = Limits {
        max_meta_bytes: 100,
        ..Limits::default()
    };
    let (_, old) = read(&OldGif, &b, &limits).unwrap();
    let (_, new) = read(&GifCodec, &b, &limits).unwrap();
    assert!(old == new);
    let Value::Image(img) = new else { panic!() };
    assert_eq!(img.meta.icc.as_deref().map(Vec::len), Some(10));
}

/// 見直しの指摘：サブブロックの途中で切れても、届いたバイトの画素まで描く。
#[test]
fn bytes_of_a_cut_sub_block_are_drawn() {
    let full = one_frame(true);
    let limits = Limits::default();
    let count = |b: &[u8]| -> usize {
        let (_, v) = read(&GifCodec, b, &limits).unwrap();
        let Value::Image(img) = v else { panic!() };
        crate::imaging::convert::to_rgba8_rows(&img.px)
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] != 0)
            .count()
    };
    // データの始まり（局所パレットと最小の符号の長さのあと）から、切る位置を 1 バイトずつ後ろへ
    let data_at = 13 + 6 + 8 + 10 + 48 + 1;
    let mut last = 0;
    let mut grew = 0;
    for cut in data_at + 2..data_at + 200 {
        let n = count(&full[..cut]);
        assert!(n >= last, "{cut}");
        if n > last {
            grew += 1;
        }
        last = n;
    }
    assert!(grew > 100, "1 バイトごとに描く画素が増える：{grew}");
}
