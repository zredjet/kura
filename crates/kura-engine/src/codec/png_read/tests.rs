//! 読み口の試験。照合の相手は前の読み手（`png_old`：png クレート 0.18.1）：前の読み手が読めた PNG は、形と値（画素・メタデータ）が
//! 同じでなければならない（識別子 `png-rs-dec/1`・`/2` を変えないため。利用者の決定）。前の読み手が断ったものを読むのは、記録した
//! 甘さ（png クレートの癖）のときだけ許す。

use std::io::Cursor;

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;

use crate::codec::png::{PngCodec, PngDecoderV1};
use crate::codec::png_old::{OldPng, OldPngV1};
use crate::codec::{Decoder, zlib};
use crate::doc::DecodeOptions;
use crate::limits::Limits;
use crate::monitor::NoMonitor;
use crate::testfiles::{ApngFrame, FileGen};

/// 1 つの読み手の結果（形・値のダイジェスト）。誤りは文字列。
type Out = (Result<String, String>, Result<String, String>);

fn run(d: &dyn Decoder, data: &[u8], limits: &Limits) -> Out {
    let opts = DecodeOptions::default();
    let shape = d
        .shape(&mut Cursor::new(data), "png", &opts, limits)
        .map(|s| format!("{s:?}"))
        .map_err(|e| e.to_string());
    let value = d
        .decode(&mut Cursor::new(data), "png", &opts, limits, &NoMonitor)
        .and_then(|v| v.digest())
        .map(|d| d.to_string())
        .map_err(|e| e.to_string());
    (shape, value)
}

/// 照らした数。
#[derive(Debug, Default)]
struct Tally {
    same_ok: usize,
    both_err: usize,
    /// 前の読み手が断り、新しい読み手が読んだ（甘くした所）。
    lenient: usize,
}

/// 前の読み手と新しい読み手を照らす（v2 と v1 の両方）。前に読めたものは同じ形・値でなければ失敗。
fn compare(data: &[u8], limits: &Limits, what: &str, t: &mut Tally) {
    for (new, old) in [
        (&PngCodec as &dyn Decoder, &OldPng as &dyn Decoder),
        (&PngDecoderV1, &OldPngV1),
    ] {
        let (ns, nv) = run(new, data, limits);
        let (os, ov) = run(old, data, limits);
        for (n, o, part) in [(&ns, &os, "形"), (&nv, &ov, "値")] {
            match (n, o) {
                (Ok(a), Ok(b)) => {
                    assert!(a == b, "{what}（{}・{part}）：新 {a} / 前 {b}", new.id());
                    t.same_ok += 1;
                }
                (Err(e), Ok(_)) => {
                    panic!(
                        "{what}（{}・{part}）：前は読めたが新しい読み手が断った：{e}",
                        new.id()
                    )
                }
                (Ok(_), Err(_)) => t.lenient += 1,
                (Err(_), Err(_)) => t.both_err += 1,
            }
        }
    }
}

/// 乱数の PNG（正しいもの）。色・深さ・インターレース・パレット・tRNS・テキスト・ICC・pHYs・eXIf を混ぜる。
fn random_png(rng: &mut ChaCha8Rng) -> FileGen {
    let r = |rng: &mut ChaCha8Rng, n: u32| rng.next_u32() % n;
    let (color, depths): (u8, &[u8]) = match r(rng, 5) {
        0 => (0, &[1, 2, 4, 8, 16]),
        1 => (2, &[8, 16]),
        2 => (3, &[1, 2, 4, 8]),
        3 => (4, &[8, 16]),
        _ => (6, &[8, 16]),
    };
    let depth = depths[r(rng, depths.len() as u32) as usize];
    let big = r(rng, 8) == 0;
    let w = r(rng, if big { 300 } else { 24 }) + 1;
    let h = r(rng, if big { 200 } else { 24 }) + 1;
    let palette_len = if color == 3 {
        (r(rng, 1 << depth.min(8)) + 1) as u16
    } else {
        0
    };
    let trns = match (color, r(rng, 3)) {
        (_, 0) => None,
        (0, _) => Some((0..2).map(|_| r(rng, 256) as u8).collect()),
        (2, _) => Some((0..6).map(|_| r(rng, 256) as u8).collect()),
        (3, _) => Some(
            (0..r(rng, u32::from(palette_len)) + 1)
                .map(|_| r(rng, 256) as u8)
                .collect(),
        ),
        _ => None,
    };
    let text = (0..r(rng, 3))
        .map(|i| {
            let key = match r(rng, 4) {
                0 => "Title".to_string(),
                1 => format!("k{i}"),
                2 => "x".repeat(79),
                _ => "Comment".to_string(),
            };
            (key, format!("値 {} é", r(rng, 1000)))
        })
        .collect();
    let icc = (r(rng, 3) == 0).then(|| (0..r(rng, 600) + 1).map(|_| r(rng, 256) as u8).collect());
    let phys = (r(rng, 3) == 0).then(|| (r(rng, 20_000), r(rng, 20_000), r(rng, 3) as u8));
    let exif = (r(rng, 5) == 0).then(|| {
        let mut e = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0".to_vec();
        e.push(r(rng, 9) as u8);
        e.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        e
    });
    FileGen::Png {
        w,
        h,
        color,
        depth,
        interlace: r(rng, 2) == 0,
        palette_len,
        trns,
        text,
        icc,
        phys,
        seed: rng.next_u64(),
        exif,
    }
}

/// 乱数の APNG（正しいもの）。
fn random_apng(rng: &mut ChaCha8Rng) -> FileGen {
    let r = |rng: &mut ChaCha8Rng, n: u32| rng.next_u32() % n;
    let color = [2u8, 3, 6][r(rng, 3) as usize];
    let (w, h) = (r(rng, 30) + 1, r(rng, 30) + 1);
    let palette_len = if color == 3 {
        (r(rng, 256) + 1) as u16
    } else {
        0
    };
    let trns = (color == 3 && r(rng, 2) == 0).then(|| {
        (0..r(rng, u32::from(palette_len)) + 1)
            .map(|i| if i == 0 { 0 } else { r(rng, 256) as u8 })
            .collect()
    });
    let separate_default = r(rng, 3) == 0;
    let n = r(rng, 5) + 1;
    let frames = (0..n)
        .map(|k| {
            let full = (k == 0 && !separate_default) || r(rng, 3) == 0;
            let (fw, fh) = if full {
                (w, h)
            } else {
                (r(rng, w) + 1, r(rng, h) + 1)
            };
            let (x, y) = if full {
                (0, 0)
            } else {
                (r(rng, w - fw + 1), r(rng, h - fh + 1))
            };
            ApngFrame {
                x,
                y,
                w: fw,
                h: fh,
                delay: (r(rng, 200) as u16, r(rng, 3) as u16 * 50),
                dispose: r(rng, 3) as u8,
                blend: r(rng, 2) as u8,
            }
        })
        .collect();
    FileGen::Apng {
        w,
        h,
        color,
        palette_len,
        trns,
        separate_default,
        plays: r(rng, 3),
        frames,
        exif: None,
        seed: rng.next_u64(),
        interlace: r(rng, 3) == 0,
    }
}

/// PNG をチャンクの並びに分ける（署名のあと。型・中身・CRC）。
fn chunks_of(data: &[u8]) -> Vec<([u8; 4], Vec<u8>, u32)> {
    let mut out = Vec::new();
    let mut at = 8;
    while at + 12 <= data.len() {
        let len = u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        let kind = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        if at + 12 + len > data.len() {
            break;
        }
        let body = data[at + 8..at + 8 + len].to_vec();
        let c = &data[at + 8 + len..at + 12 + len];
        out.push((kind, body, u32::from_be_bytes([c[0], c[1], c[2], c[3]])));
        at += 12 + len;
    }
    out
}

/// チャンクの並びから PNG を組む（CRC はそのまま）。
fn assemble(chunks: &[([u8; 4], Vec<u8>, u32)]) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, body, crc) in chunks {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out
}

fn crc_of(kind: &[u8; 4], body: &[u8]) -> u32 {
    zlib::crc32_update(zlib::crc32(kind), body)
}

/// チャンクの層で壊す（CRC は直すか、わざと誤らせる）。
fn mutate(rng: &mut ChaCha8Rng, data: &[u8]) -> Vec<u8> {
    let r = |rng: &mut ChaCha8Rng, n: u32| rng.next_u32() % n.max(1);
    let mut c = chunks_of(data);
    if c.is_empty() {
        return data.to_vec();
    }
    for _ in 0..=r(rng, 2) {
        let n = c.len() as u32;
        match r(rng, 12) {
            // IDAT を 2 つに分ける
            0 => {
                if let Some(i) = c.iter().position(|x| &x.0 == b"IDAT") {
                    let body = c[i].1.clone();
                    let cut = r(rng, body.len() as u32 + 1) as usize;
                    let (a, b) = (body[..cut].to_vec(), body[cut..].to_vec());
                    let ca = crc_of(b"IDAT", &a);
                    let cb = crc_of(b"IDAT", &b);
                    c[i] = (*b"IDAT", a, ca);
                    c.insert(i + 1, (*b"IDAT", b, cb));
                }
            }
            // 知らない補助チャンクを差し込む
            1 => {
                let at = r(rng, n - 1) as usize + 1;
                let body: Vec<u8> = (0..r(rng, 40)).map(|_| r(rng, 256) as u8).collect();
                let k = *b"zzZz";
                let crc = crc_of(&k, &body);
                c.insert(at, (k, body, crc));
            }
            // 補助チャンクの CRC を誤らせる
            2 => {
                let anc: Vec<usize> = (0..c.len()).filter(|&i| c[i].0[0] & 0x20 != 0).collect();
                if !anc.is_empty() {
                    let i = anc[r(rng, anc.len() as u32) as usize];
                    c[i].2 ^= 1;
                }
            }
            // 最後の IDAT の CRC を誤らせる（静止画では読まない）
            3 => {
                if let Some(i) = c.iter().rposition(|x| &x.0 == b"IDAT") {
                    c[i].2 ^= 0x8000_0000;
                }
            }
            // IEND を外す・後ろにごみ
            4 => {
                if let Some(i) = c.iter().position(|x| &x.0 == b"IEND") {
                    c.truncate(i);
                }
            }
            5 => {
                let mut out = assemble(&c);
                out.extend((0..r(rng, 30) + 1).map(|_| r(rng, 256) as u8));
                return out;
            }
            // 途中で切る
            6 => {
                let out = assemble(&c);
                let cut = r(rng, out.len() as u32) as usize;
                return out[..cut].to_vec();
            }
            // チャンクの中身の 1 バイトを差し替え（CRC を直す・直さない）
            7 => {
                let i = r(rng, n) as usize;
                if !c[i].1.is_empty() {
                    let at = r(rng, c[i].1.len() as u32) as usize;
                    c[i].1[at] = r(rng, 256) as u8;
                    if r(rng, 2) == 0 {
                        c[i].2 = crc_of(&c[i].0, &c[i].1);
                    }
                }
            }
            // 補助チャンクを重ねる（tRNS・pHYs・iCCP・テキスト・acTL）
            8 => {
                let anc: Vec<usize> = (0..c.len()).filter(|&i| c[i].0[0] & 0x20 != 0).collect();
                if !anc.is_empty() {
                    let i = anc[r(rng, anc.len() as u32) as usize];
                    let dup = c[i].clone();
                    c.insert(i + 1, dup);
                }
            }
            // チャンクを並べ替える（隣と入れ替え）
            9 => {
                let i = r(rng, n.saturating_sub(1)) as usize + 1;
                if i + 1 < c.len() {
                    c.swap(i, i + 1);
                }
            }
            // 補助チャンクの長さを変える（中身を足す・削る）
            10 => {
                let anc: Vec<usize> = (0..c.len()).filter(|&i| c[i].0[0] & 0x20 != 0).collect();
                if !anc.is_empty() {
                    let i = anc[r(rng, anc.len() as u32) as usize];
                    if r(rng, 2) == 0 {
                        c[i].1.push(r(rng, 256) as u8);
                    } else {
                        c[i].1.pop();
                    }
                    c[i].2 = crc_of(&c[i].0, &c[i].1);
                }
            }
            // IDAT の zlib の Adler-32 を壊す（CRC は直す）
            _ => {
                if let Some(i) = c.iter().rposition(|x| &x.0 == b"IDAT") {
                    if let Some(last) = c[i].1.last_mut() {
                        *last ^= 0x5a;
                    }
                    c[i].2 = crc_of(b"IDAT", &c[i].1);
                }
            }
        }
    }
    assemble(&c)
}

/// 正しい PNG・APNG を前の読み手と照らす（既定の上限と、ICC・テキストに効く小さな上限）。
#[test]
fn valid_pngs_match_the_png_crate() {
    let mut rng = crate::determinism::rng(601);
    let mut t = Tally::default();
    let small = Limits {
        max_meta_bytes: 900,
        ..Limits::default()
    };
    // debug のビルドでは少なく（release で回すと多く）
    let n = if cfg!(debug_assertions) { 200 } else { 1500 };
    for i in 0..n {
        let g = if i % 3 == 2 {
            random_apng(&mut rng)
        } else {
            random_png(&mut rng)
        };
        let data = g.bytes().unwrap();
        compare(&data, &Limits::default(), &format!("{i} {g:?}"), &mut t);
        compare(&data, &small, &format!("{i} 小さな上限 {g:?}"), &mut t);
    }
    assert!(t.same_ok > n * 2, "{t:?}");
}

/// 壊した PNG・APNG を前の読み手と照らす：前に読めたものは同じ値。
#[test]
fn damaged_pngs_match_the_png_crate() {
    let mut rng = crate::determinism::rng(602);
    let mut t = Tally::default();
    let n = if cfg!(debug_assertions) { 400 } else { 4000 };
    for i in 0..n {
        let g = if i % 3 == 2 {
            random_apng(&mut rng)
        } else {
            random_png(&mut rng)
        };
        let data = mutate(&mut rng, &g.bytes().unwrap());
        compare(&data, &Limits::default(), &format!("{i} {g:?}"), &mut t);
    }
    eprintln!("{t:?}");
    assert!(t.same_ok > n / 2 && t.both_err > n / 8, "{t:?}");
}

/// 展開の形：静止画の最後の IDAT の CRC・Adler-32・後ろのごみ・IEND のなさは見ない（png クレートと同じ）。2 つの IDAT の 1 つ目の
/// CRC の誤りは、2 つ目を使えば壊れたファイル。
#[test]
fn crc_of_the_last_idat_is_only_checked_when_used() {
    let g = FileGen::Png {
        w: 64,
        h: 64,
        color: 2,
        depth: 8,
        interlace: false,
        palette_len: 0,
        trns: None,
        text: vec![],
        icc: None,
        phys: None,
        seed: 5,
        exif: None,
    };
    let data = g.bytes().unwrap();
    let base = run(&PngCodec, &data, &Limits::default()).1.unwrap();
    let mut c = chunks_of(&data);
    let i = c.iter().position(|x| &x.0 == b"IDAT").unwrap();
    // 唯一の IDAT の CRC
    let mut bad = c.clone();
    bad[i].2 ^= 1;
    assert_eq!(
        run(&PngCodec, &assemble(&bad), &Limits::default())
            .1
            .unwrap(),
        base
    );
    // 2 つに分けて 1 つ目の CRC を誤らせる：2 つ目を使うので断る
    let body = c[i].1.clone();
    let half = body.len() / 2;
    let (a, b) = (body[..half].to_vec(), body[half..].to_vec());
    let ca = crc_of(b"IDAT", &a) ^ 1;
    let cb = crc_of(b"IDAT", &b);
    c[i] = (*b"IDAT", a, ca);
    c.insert(i + 1, (*b"IDAT", b, cb));
    assert!(run(&PngCodec, &assemble(&c), &Limits::default()).1.is_err());
    assert!(run(&OldPng, &assemble(&c), &Limits::default()).1.is_err());
}

/// IDAT をどこで 2 つに分け、1 つ目の CRC を誤らせても、前の読み手と同じ（2 つ目のデータを展開に使うときだけ断る）。インターレースと
/// 非インターレース、Paeth の多い絵とそうでない絵で、分け目をすべて試す。
#[test]
fn idat_crc_boundaries_match_the_png_crate() {
    let mut t = Tally::default();
    for (w, h, interlace, seed) in [(13u32, 9u32, false, 1u64), (9, 13, true, 2)] {
        let g = FileGen::Png {
            w,
            h,
            color: 6,
            depth: 8,
            interlace,
            palette_len: 0,
            trns: None,
            text: vec![],
            icc: None,
            phys: None,
            seed,
            exif: None,
        };
        let data = g.bytes().unwrap();
        let c = chunks_of(&data);
        let i = c.iter().position(|x| &x.0 == b"IDAT").unwrap();
        let body = c[i].1.clone();
        for cut in 0..=body.len() {
            let mut d = c.clone();
            let (a, b) = (body[..cut].to_vec(), body[cut..].to_vec());
            let ca = crc_of(b"IDAT", &a) ^ 1;
            let cb = crc_of(b"IDAT", &b);
            d[i] = (*b"IDAT", a, ca);
            d.insert(i + 1, (*b"IDAT", b, cb));
            compare(
                &assemble(&d),
                &Limits::default(),
                &format!("{w}x{h} 分け目 {cut}"),
                &mut t,
            );
        }
    }
    assert!(t.same_ok > 0 && t.both_err > 0, "{t:?}");
}

/// 最後のフレームの後ろにある壊れた fdAT（番号の誤り・短すぎる）は、要る行が揃えば読まない（前の読み手と同じく読める）。途中の
/// フレームのあとなら、次のフレームへ進むときに断る（前の読み手と同じ）。
#[test]
fn broken_fdat_after_the_needed_data_matches_the_png_crate() {
    let mut t = Tally::default();
    let frame = |x, y, w, h| ApngFrame {
        x,
        y,
        w,
        h,
        delay: (10, 100),
        dispose: 0,
        blend: 0,
    };
    let g = FileGen::Apng {
        w: 12,
        h: 10,
        color: 6,
        palette_len: 0,
        trns: None,
        separate_default: false,
        plays: 0,
        frames: vec![frame(0, 0, 12, 10), frame(2, 1, 5, 4), frame(1, 2, 8, 7)],
        exif: None,
        seed: 3,
        interlace: false,
    };
    let data = g.bytes().unwrap();
    let c = chunks_of(&data);
    let fdats: Vec<usize> = (0..c.len()).filter(|&i| &c[i].0 == b"fdAT").collect();
    assert!(fdats.len() >= 2);
    for &after in &[*fdats.last().unwrap(), fdats[0]] {
        for body in [
            {
                let mut b = 999u32.to_be_bytes().to_vec();
                b.extend_from_slice(b"junk");
                b
            },
            vec![0, 0],
        ] {
            let mut d = c.clone();
            let crc = crc_of(b"fdAT", &body);
            d.insert(after + 1, (*b"fdAT", body, crc));
            compare(
                &assemble(&d),
                &Limits::default(),
                &format!("fdAT のあと {after}"),
                &mut t,
            );
        }
    }
    // 最後のフレームのあとの 2 つは読め、途中のあとの 2 つは断る（v2 の値。v1 は既定の画像だけなのでどれも読める）
    assert!(t.same_ok >= 4 && t.both_err >= 2, "{t:?}");
}

/// コーパス（手で回す。`KURA_PNG_CORPUS=<フォルダ> cargo test --release -p kura-engine --lib png_corpus -- --ignored --nocapture`）：
/// フォルダの下の .png・.apng をすべて、前の読み手と照らす（前に読めたものは同じ形・値）。読みの時間の合計も比べる。
#[test]
#[ignore]
// 手で回す確かめ：フォルダの場所（環境変数）と時間を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn png_corpus_matches_the_png_crate() {
    let Ok(root) = std::env::var("KURA_PNG_CORPUS") else {
        eprintln!("KURA_PNG_CORPUS がないので見送ります");
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
                .is_some_and(|x| x.eq_ignore_ascii_case("png") || x.eq_ignore_ascii_case("apng"))
            {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut t = Tally::default();
    let (mut t_new, mut t_old) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let opts = DecodeOptions::default();
    for p in &files {
        let Ok(data) = std::fs::read(p) else {
            continue;
        };
        compare(&data, &Limits::default(), &p.display().to_string(), &mut t);
        for (d, acc) in [
            (&PngCodec as &dyn Decoder, &mut t_new),
            (&OldPng as &dyn Decoder, &mut t_old),
        ] {
            let s = std::time::Instant::now();
            let _ = d.decode(
                &mut Cursor::new(&data),
                "png",
                &opts,
                &Limits::default(),
                &NoMonitor,
            );
            *acc += s.elapsed();
        }
    }
    eprintln!(
        "ZZCORPUS {} 本 {t:?} 読みの合計：新 {:.1} ms・前 {:.1} ms",
        files.len(),
        t_new.as_secs_f64() * 1e3,
        t_old.as_secs_f64() * 1e3
    );
}

/// 速さ（手で回す。`KURA_PNG_CORPUS=<フォルダ> cargo test --release -p kura-engine --lib png_speed -- --ignored --nocapture`）：
/// コーパスの大きい順に 400 本をメモリに読み、新しい読み手と前の読み手で `KURA_PNG_ROUNDS`（既定 3）回ずつ読む。
#[test]
#[ignore]
// 手で回す確かめ：フォルダの場所（環境変数）と時間を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn png_speed_against_the_png_crate() {
    let Ok(root) = std::env::var("KURA_PNG_CORPUS") else {
        return;
    };
    let rounds: usize = std::env::var("KURA_PNG_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
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
            } else if p.extension().and_then(|x| x.to_str()) == Some("png")
                && let Ok(d) = std::fs::read(&p)
            {
                files.push(d);
            }
        }
    }
    files.sort_by_key(|d| std::cmp::Reverse(d.len()));
    files.truncate(400);
    let opts = DecodeOptions::default();
    let total: usize = files.iter().map(Vec::len).sum();
    for _ in 0..rounds {
        for (name, d) in [("新", &PngCodec as &dyn Decoder), ("前", &OldPng)] {
            let s = std::time::Instant::now();
            for f in &files {
                let _ = d.decode(
                    &mut Cursor::new(f),
                    "png",
                    &opts,
                    &Limits::default(),
                    &NoMonitor,
                );
            }
            eprintln!(
                "ZZSPEED {name} {:.1} ms（{} 本・{} MB）",
                s.elapsed().as_secs_f64() * 1e3,
                files.len(),
                total >> 20
            );
        }
    }
}

/// 速さ（手で回す）：一括の計測（NFR-14）と同じドット絵の PNG 1,000 件を、新しい読み手と前の読み手で読む。
#[test]
#[ignore]
// 手で回す確かめ：フォルダの場所（環境変数）と時間を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn png_speed_on_sprites() {
    let set = crate::codec::CodecSet::pinned();
    let files: Vec<Vec<u8>> = crate::testing::bench::sprite_batch(1000, 1)
        .iter()
        .map(|g| {
            let v = g.generate().unwrap();
            set.encode_bytes(
                &v,
                "png",
                &crate::testing::bench::cases::bulk_png(),
                &NoMonitor,
            )
            .unwrap()
            .0
        })
        .collect();
    let opts = DecodeOptions::default();
    for _ in 0..3 {
        for (name, d) in [("新", &PngCodec as &dyn Decoder), ("前", &OldPng)] {
            let s = std::time::Instant::now();
            for f in &files {
                let _ = d.decode(
                    &mut Cursor::new(f),
                    "png",
                    &opts,
                    &Limits::default(),
                    &NoMonitor,
                );
            }
            eprintln!("ZZSPRITE {name} {:.1} ms", s.elapsed().as_secs_f64() * 1e3);
        }
    }
}
