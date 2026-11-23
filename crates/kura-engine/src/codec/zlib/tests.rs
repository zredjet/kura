//! zlib・deflate の展開と CRC-32・Adler-32 の試験。照合の相手は miniz_oxide（圧縮と展開）。

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;

use super::*;

/// いろいろな中身（雑音・繰り返し・文字・0 の列・それらの継ぎ）。
fn sample(rng: &mut ChaCha8Rng, len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        let n = (rng.next_u32() as usize % 4096 + 1).min(len - v.len());
        match rng.next_u32() % 5 {
            0 => v.extend((0..n).map(|_| rng.next_u32() as u8)),
            1 => {
                let b = rng.next_u32() as u8;
                v.extend(std::iter::repeat_n(b, n));
            }
            2 => {
                let period = rng.next_u32() as usize % 12 + 2;
                let pat: Vec<u8> = (0..period).map(|_| rng.next_u32() as u8).collect();
                v.extend((0..n).map(|i| pat[i % period]));
            }
            3 => v.extend((0..n).map(|i| b"the quick brown fox "[i % 20])),
            _ => {
                // 前の中身の写し（遠い一致）
                if v.len() > 100 {
                    let at = rng.next_u32() as usize % (v.len() - 50);
                    let k = n.min(v.len() - at);
                    let part = v[at..at + k].to_vec();
                    v.extend(part);
                } else {
                    v.extend(std::iter::repeat_n(0u8, n));
                }
            }
        }
    }
    v.truncate(len);
    v
}

/// 入力を `piece` バイトずつ渡す入力（0 なら乱数の長さ）。
struct Pieces<'a> {
    data: &'a [u8],
    at: usize,
    piece: usize,
    rng: ChaCha8Rng,
}

impl Input for Pieces<'_> {
    fn more(&mut self, dst: &mut Vec<u8>) -> Result<usize, InputError> {
        let n = if self.piece == 0 {
            self.rng.next_u32() as usize % 300 + 1
        } else {
            self.piece
        }
        .min(self.data.len() - self.at);
        dst.extend_from_slice(&self.data[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// `data` を展開する（入力を `piece` ずつ、出力を `want` ずつ取る）。
fn inflate_all(
    data: &[u8],
    zlib: bool,
    piece: usize,
    want: usize,
    limit: u64,
) -> Result<Vec<u8>, Error> {
    let mut z = Inflate::new(
        Pieces {
            data,
            at: 0,
            piece,
            rng: crate::determinism::rng(9),
        },
        Options {
            zlib,
            check_adler: true,
            limit,
            flavor: Flavor::Miniz,
        },
    );
    let mut out = Vec::new();
    loop {
        let n = z.fill(want)?;
        out.extend_from_slice(&z.ready()[..n]);
        z.consume(n);
        if z.finished() {
            return Ok(out);
        }
        assert!(n > 0, "進まない");
    }
}

#[test]
fn crc32_matches_the_check_values() {
    assert_eq!(crc32(b""), 0);
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    let mut rng = crate::determinism::rng(1);
    let data = sample(&mut rng, 100_003);
    // 1 バイトずつの素朴な式と、切れ目をずらした続きの計算が同じ
    let naive = {
        let mut c = !0u32;
        for &b in &data {
            c ^= u32::from(b);
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    };
    assert_eq!(crc32(&data), naive);
    for cut in [0, 1, 7, 8, 9, 4095, 100_002] {
        assert_eq!(
            crc32_update(crc32(&data[..cut]), &data[cut..]),
            naive,
            "{cut}"
        );
    }
}

#[test]
fn adler32_matches_the_check_values() {
    assert_eq!(adler32(b""), 1);
    assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    let data = vec![0xffu8; 1_000_000];
    let naive = {
        let (mut a, mut b) = (1u64, 0u64);
        for &x in &data {
            a = (a + u64::from(x)) % 65_521;
            b = (b + a) % 65_521;
        }
        ((b << 16) | a) as u32
    };
    assert_eq!(adler32(&data), naive);
    for cut in [1, 5_551, 5_552, 5_553, 999_999] {
        assert_eq!(
            adler32_update(adler32(&data[..cut]), &data[cut..]),
            naive,
            "{cut}"
        );
    }
}

/// miniz_oxide の全部の水準（0 は stored のブロック、1 は固定のハフマンが多い）で圧縮したものを、入力の切り方と出力の取り方を
/// 変えて展開すると元に戻る（zlib と生の deflate）。
#[test]
fn round_trips_miniz_at_every_level_and_cut() {
    let mut rng = crate::determinism::rng(2);
    for len in [0usize, 1, 2, 257, 32_768, 32_769, 70_000, 300_001] {
        let data = sample(&mut rng, len);
        for level in 0..=10u8 {
            let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, level);
            let raw = miniz_oxide::deflate::compress_to_vec(&data, level);
            for (piece, want) in [(1 << 20, 1 << 20), (1, 1), (0, 777), (7, 65_536)] {
                if len > 70_000 && piece == 1 {
                    continue;
                }
                let got = inflate_all(&z, true, piece, want, u64::MAX).unwrap();
                assert!(got == data, "zlib {len} {level} {piece} {want}");
                let got = inflate_all(&raw, false, piece, want, u64::MAX).unwrap();
                assert!(got == data, "raw {len} {level} {piece} {want}");
            }
            if level == 6 {
                assert_eq!(inflate_exact(&z, data.len()).unwrap(), data);
            }
        }
    }
}

/// 上限：ちょうどなら読め、1 バイト足りなければ `TooLong`。`inflate_exact` は多くても少なくても誤り。
#[test]
fn the_output_limit_is_exact() {
    let mut rng = crate::determinism::rng(3);
    let data = sample(&mut rng, 50_000);
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
    assert_eq!(inflate_all(&z, true, 0, 4096, 50_000).unwrap(), data);
    assert!(matches!(
        inflate_all(&z, true, 0, 4096, 49_999),
        Err(Error::Z(ZError::TooLong))
    ));
    assert!(inflate_exact(&z, 49_999).is_err());
    assert!(inflate_exact(&z, 50_001).is_err());
    let stored = miniz_oxide::deflate::compress_to_vec_zlib(&data, 0);
    assert!(matches!(
        inflate_all(&stored, true, 0, 4096, 49_999),
        Err(Error::Z(ZError::TooLong))
    ));
}

/// 途中で切った流れはどこで切っても誤り（切れた流れか壊れた流れ）で、panic しない。
#[test]
fn truncated_streams_are_refused_everywhere() {
    let mut rng = crate::determinism::rng(4);
    let data = sample(&mut rng, 20_000);
    for level in [0u8, 1, 6, 9] {
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, level);
        for cut in 0..z.len() {
            let r = inflate_all(&z[..cut], true, 0, 1000, u64::MAX);
            assert!(r.is_err(), "{level} {cut}");
        }
    }
}

/// 壊した流れ（ビットを反転・バイトを差し替え）を miniz_oxide と照らす：両方が読めれば同じ出力、片方だけが読むのは、記録した
/// 違い（miniz は不完全な符号の表を一部許す）のときだけ。panic しない。
#[test]
fn damaged_streams_agree_with_miniz() {
    let mut rng = crate::determinism::rng(5);
    let mut both_ok = 0;
    let mut both_err = 0;
    for round in 0..3000 {
        let len = (rng.next_u32() % 3000) as usize;
        let data = sample(&mut rng, len);
        let level = (rng.next_u32() % 10) as u8;
        let mut z = miniz_oxide::deflate::compress_to_vec(&data, level);
        if z.is_empty() {
            continue;
        }
        for _ in 0..=(rng.next_u32() % 4) {
            let at = rng.next_u32() as usize % z.len();
            if rng.next_u32().is_multiple_of(2) {
                z[at] ^= 1 << (rng.next_u32() % 8);
            } else {
                z[at] = rng.next_u32() as u8;
            }
        }
        let ours = inflate_all(&z, false, 0, 1 << 16, 1 << 24);
        let theirs = miniz_oxide::inflate::decompress_to_vec_with_limit(&z, 1 << 24);
        match (&ours, &theirs) {
            (Ok(a), Ok(b)) => {
                assert!(a == b, "{round}: 出力が違う");
                both_ok += 1;
            }
            (Err(_), Err(_)) => both_err += 1,
            (Ok(_), Err(e)) => panic!("{round}: 自作だけが読めた（miniz：{e:?}）"),
            (Err(e), Ok(_)) => panic!("{round}: miniz だけが読めた（自作：{e:?}）"),
        }
    }
    assert!(both_ok > 100 && both_err > 100, "{both_ok} {both_err}");
}

/// 手で組んだ流れのビットの書き手（下位から）。
struct BitW {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitW {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }

    fn put(&mut self, v: u32, bits: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// ハフマンの符号（上位から詰める）。
    fn code(&mut self, code: u32, len: u32) {
        self.put(reverse(code, len), len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// 固定のハフマンの文字と長さの符号。
fn fixed_code(w: &mut BitW, sym: u32) {
    match sym {
        0..=143 => w.code(0x30 + sym, 8),
        144..=255 => w.code(0x190 + sym - 144, 9),
        256..=279 => w.code(sym - 256, 7),
        _ => w.code(0xc0 + sym - 280, 8),
    }
}

fn raw(data: &[u8]) -> Result<Vec<u8>, Error> {
    inflate_all(data, false, 0, 1 << 16, 1 << 20)
}

/// RFC 1951 の端：固定の符号の 286・287 と距離の符号 30・31、頭より前を指す距離、ブロックの型 3、stored の長さの食い違い、
/// 長さ 258、距離 32768 は読める。
#[test]
fn rfc1951_edges_of_fixed_blocks() {
    let fixed = |f: &dyn Fn(&mut BitW)| {
        let mut w = BitW::new();
        w.put(1, 1);
        w.put(1, 2);
        f(&mut w);
        w.finish()
    };
    // 'a' と end-of-block
    assert_eq!(
        raw(&fixed(&|w| {
            fixed_code(w, u32::from(b'a'));
            fixed_code(w, 256);
        }))
        .unwrap(),
        b"a"
    );
    for bad in [286, 287] {
        let s = fixed(&|w| {
            fixed_code(w, u32::from(b'a'));
            fixed_code(w, bad);
            w.code(0, 5);
            fixed_code(w, 256);
        });
        assert!(
            matches!(raw(&s), Err(Error::Z(ZError::Corrupt(_)))),
            "{bad}"
        );
    }
    for bad_dist in [30, 31] {
        let s = fixed(&|w| {
            fixed_code(w, u32::from(b'a'));
            fixed_code(w, 257);
            w.code(bad_dist, 5);
            fixed_code(w, 256);
        });
        assert!(
            matches!(raw(&s), Err(Error::Z(ZError::Corrupt(_)))),
            "{bad_dist}"
        );
    }
    // 距離 2 は 1 バイトしか書いていないので頭より前
    let s = fixed(&|w| {
        fixed_code(w, u32::from(b'a'));
        fixed_code(w, 257);
        w.code(1, 5);
        fixed_code(w, 256);
    });
    assert!(matches!(raw(&s), Err(Error::Z(ZError::Corrupt(_)))));
    // 長さ 258（符号 285）を距離 1 で：'a' × 259
    let s = fixed(&|w| {
        fixed_code(w, u32::from(b'a'));
        fixed_code(w, 285);
        w.code(0, 5);
        fixed_code(w, 256);
    });
    assert_eq!(raw(&s).unwrap(), vec![b'a'; 259]);
    // ブロックの型 3
    let mut w = BitW::new();
    w.put(1, 1);
    w.put(3, 2);
    assert!(matches!(
        raw(&w.finish()),
        Err(Error::Z(ZError::Corrupt(_)))
    ));
    // stored：LEN と NLEN の食い違い・空の stored・続く固定
    assert!(matches!(
        raw(&[0x01, 0x03, 0x00, 0xfc, 0xfe, b'x', b'y', b'z']),
        Err(Error::Z(ZError::Corrupt(_)))
    ));
    assert_eq!(raw(&[0x01, 0x00, 0x00, 0xff, 0xff]).unwrap(), b"");
    assert_eq!(
        raw(&[0x01, 0x03, 0x00, 0xfc, 0xff, b'x', b'y', b'z']).unwrap(),
        b"xyz"
    );
    // 距離 32768（符号 29 と追加 13 ビットの最大）：32768 バイトの stored のあとに固定のブロックで写す
    let mut data = Vec::new();
    data.extend_from_slice(&[0x00, 0x00, 0x80, 0xff, 0x7f]);
    let body: Vec<u8> = (0..32_768u32).map(|i| (i % 251) as u8).collect();
    data.extend_from_slice(&body);
    let mut w = BitW::new();
    w.put(1, 1);
    w.put(1, 2);
    fixed_code(&mut w, 257);
    w.code(29, 5);
    w.put(8191, 13);
    fixed_code(&mut w, 256);
    data.extend(w.finish());
    let got = raw(&data).unwrap();
    assert_eq!(got.len(), 32_771);
    assert_eq!(&got[32_768..], &body[..3]);
}

/// 動的ブロックの表の端：繰り返しで始まる・数を超える繰り返し・end-of-block の符号がない・過剰な符号・不完全な符号（1 つだけの
/// 長さ 1 の距離の符号と、符号のない距離の表は許す）。
#[test]
fn rfc1951_edges_of_dynamic_tables() {
    // 符号の長さの符号はどれも 4 ビット（0〜15 の 16 個は 4 ビットの完全な符号にならないので、16・17・18 と 0〜15 で 19 個 → 長さ
    // 5 にする）。簡単のため、長さの符号は 18 個を長さ 5、2 個（16・17 以外の 1 つ）を省く…ではなく、19 個すべて 5 ビットに
    // すると過剰でない不完全な符号（32 のうち 19）になり誤りになる。そこで 16 個を 4 ビットにして完全にする
    let dynamic = |lens: &[u8], body: &dyn Fn(&mut BitW, &[u32])| -> Vec<u8> {
        // 符号の長さの符号：0〜15 を 4 ビット（完全）、16・17・18 は使わない
        let mut w = BitW::new();
        w.put(1, 1);
        w.put(2, 2);
        let hlit = 257;
        let hdist = lens.len() - hlit;
        w.put((hlit - 257) as u32, 5);
        w.put((hdist - 1) as u32, 5);
        w.put(15, 4); // HCLEN = 19
        let mut clen = [0u32; 19];
        for (i, c) in clen.iter_mut().enumerate().take(16) {
            *c = 4;
            let _ = i;
        }
        for &sym in &CLEN_ORDER {
            w.put(clen[sym], 3);
        }
        // 符号の長さの符号 0〜15 は 4 ビットの正規の符号 = 符号の値そのもの
        for &l in lens {
            w.code(u32::from(l), 4);
        }
        // 文字と長さの符号の正規の符号
        let codes = canonical(&lens[..hlit]);
        body(&mut w, &codes);
        w.finish()
    };
    // 'a'（長さ 1）と end-of-block（長さ 1）だけ、距離は 1 つの長さ 1 の符号
    let mut lens = vec![0u8; 257 + 1];
    lens[usize::from(b'a')] = 1;
    lens[256] = 1;
    lens[257] = 1;
    let s = dynamic(&lens, &|w, codes| {
        w.code(codes[usize::from(b'a')], 1);
        w.code(codes[256], 1);
    });
    assert_eq!(raw(&s).unwrap(), b"a", "距離の 1 つだけの符号は許す");
    // end-of-block がない
    let mut lens = vec![0u8; 258];
    lens[0] = 1;
    lens[1] = 1;
    lens[257] = 1;
    assert!(matches!(
        raw(&dynamic(&lens, &|_, _| {})),
        Err(Error::Z(ZError::Corrupt(_)))
    ));
    // 過剰（長さ 1 が 3 つ）
    let mut lens = vec![0u8; 258];
    lens[0] = 1;
    lens[1] = 1;
    lens[256] = 1;
    lens[257] = 1;
    assert!(matches!(
        raw(&dynamic(&lens, &|_, _| {})),
        Err(Error::Z(ZError::Corrupt(_)))
    ));
    // 文字の符号が不完全（長さ 2 が 2 つ）
    let mut lens = vec![0u8; 258];
    lens[0] = 2;
    lens[256] = 2;
    lens[257] = 1;
    assert!(matches!(
        raw(&dynamic(&lens, &|_, _| {})),
        Err(Error::Z(ZError::Corrupt(_)))
    ));
}

/// 符号の長さから正規の符号（RFC 1951 §3.2.2）。
fn canonical(lens: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 16];
    for &l in lens {
        count[usize::from(l)] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0;
    for l in 1..16 {
        code = (code + count[l - 1]) << 1;
        next[l] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[usize::from(l)];
                next[usize::from(l)] += 1;
                c
            }
        })
        .collect()
}

/// zlib の頭：方式・窓・検査・辞書の誤りと、Adler-32 の誤り（照らさない設定なら通る）。
#[test]
fn zlib_header_and_trailer() {
    let data = b"hello hello hello";
    let z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    assert_eq!(inflate_all(&z, true, 0, 64, u64::MAX).unwrap(), data);
    let mut bad = z.clone();
    bad[0] = 0x79; // 方式 9
    assert!(inflate_all(&bad, true, 0, 64, u64::MAX).is_err());
    let mut bad = z.clone();
    bad[0] = 0x88; // 窓 2^16
    bad[1] = (31 - ((0x88u32 << 8) % 31)) as u8;
    assert!(inflate_all(&bad, true, 0, 64, u64::MAX).is_err());
    let mut bad = z.clone();
    bad[1] ^= 1; // FCHECK
    assert!(inflate_all(&bad, true, 0, 64, u64::MAX).is_err());
    let mut bad = z.clone();
    let n = bad.len();
    bad[n - 1] ^= 1;
    assert!(matches!(
        inflate_all(&bad, true, 0, 64, u64::MAX),
        Err(Error::Z(ZError::Adler))
    ));
    let mut zz = Inflate::new(
        SliceInput::new(&bad),
        Options {
            zlib: true,
            check_adler: false,
            limit: u64::MAX,
            flavor: Flavor::Miniz,
        },
    );
    zz.fill(1000).unwrap();
    assert!(zz.finished());
    assert_eq!(zz.ready(), data);
    // 尾のあとに残ったデータ
    let mut more = z.clone();
    more.extend_from_slice(b"junk");
    let mut zz = Inflate::new(
        SliceInput::new(&more),
        Options {
            zlib: true,
            check_adler: true,
            limit: u64::MAX,
            flavor: Flavor::Miniz,
        },
    );
    zz.fill(1000).unwrap();
    assert!(zz.finished());
    assert_eq!(zz.leftover(), 4);
}

/// 速さ（手で回す。`cargo test --release -p kura-engine --lib speed_against_miniz -- --ignored --nocapture`）：64 MiB の中身を
/// 水準 6 で圧縮したものを、miniz_oxide と自作で展開する。
#[test]
#[ignore]
// 手で回す速さの計測：時間を読む。値には効かない
#[allow(clippy::disallowed_methods)]
fn speed_against_miniz() {
    let mut rng = crate::determinism::rng(77);
    // 写真に近い中身：ゆるやかな勾配に小さな雑音（PNG のフィルタのあとに近い）と、スプライトに近い繰り返し
    let mut data = Vec::with_capacity(64 << 20);
    while data.len() < 32 << 20 {
        let v = (data.len() / 4096) as u8;
        data.push(v.wrapping_add((rng.next_u32() % 5) as u8));
    }
    data.extend(sample(&mut rng, 32 << 20));
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&data, 6);
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let a = miniz_oxide::inflate::decompress_to_vec_zlib(&z).unwrap();
        let tm = t.elapsed();
        let t = std::time::Instant::now();
        let mut zz = Inflate::new(
            SliceInput::new(&z),
            Options {
                zlib: true,
                check_adler: false,
                limit: u64::MAX,
                flavor: Flavor::Zlib,
            },
        );
        let mut n_out = 0usize;
        let mut sum = 0u64;
        while !zz.finished() {
            let n = zz.fill(1 << 20).unwrap();
            sum = sum.wrapping_add(u64::from(zz.ready().first().copied().unwrap_or(0)));
            n_out += n;
            zz.consume(n);
        }
        let to = t.elapsed();
        assert_eq!(n_out, a.len());
        eprintln!(
            "ZZSPEED miniz {:.1} ms, 自作 {:.1} ms（{} MB → {} MB, {sum}）",
            tm.as_secs_f64() * 1e3,
            to.as_secs_f64() * 1e3,
            z.len() >> 20,
            a.len() >> 20
        );
    }
}

/// 圧縮しない zlib（試験のファイルの作り手）：自作と miniz_oxide のどちらでも元に戻る（空・ちょうど 65,535・またぐ長さ）。
#[test]
fn stored_zlib_round_trips() {
    let mut rng = crate::determinism::rng(8);
    for len in [0usize, 1, 65_535, 65_536, 200_000] {
        let data = sample(&mut rng, len);
        let z = store_zlib(&data);
        assert_eq!(inflate_exact(&z, len).unwrap(), data, "{len}");
        assert_eq!(
            miniz_oxide::inflate::decompress_to_vec_zlib(&z).unwrap(),
            data,
            "{len}"
        );
    }
}

/// fdeflate の受け付け（PNG・iCCP）を fdeflate 0.3.7 と照らす：miniz_oxide で圧縮したものを壊した流れと、固定のブロックの
/// 286・287 を混ぜた流れで、`decompress_to_vec`（入力の終わりまで・Adler-32 を照らす）と同じく読めるか断るか、読めれば同じ値。
#[test]
fn fdeflate_flavor_matches_fdeflate() {
    let mut rng = crate::determinism::rng(10);
    let (mut same, mut both_err) = (0, 0);
    for round in 0..3000 {
        let len = (rng.next_u32() % 3000) as usize;
        let data = sample(&mut rng, len);
        let level = (rng.next_u32() % 10) as u8;
        let mut z = miniz_oxide::deflate::compress_to_vec_zlib(&data, level);
        if round % 7 == 0 {
            // 固定のブロックに 286・287 を入れた生の流れを zlib で包む（fdeflate はブロックの終わりとして読む）
            let mut w = BitW::new();
            w.put(0, 1);
            w.put(1, 2);
            fixed_code(&mut w, u32::from(b'a'));
            fixed_code(&mut w, 286 + rng.next_u32() % 2);
            w.put(1, 1);
            w.put(1, 2);
            fixed_code(&mut w, u32::from(b'b'));
            fixed_code(&mut w, 256);
            let mut body = vec![0x78, 0x01];
            body.extend(w.finish());
            body.extend_from_slice(&adler32(b"ab").to_be_bytes());
            z = body;
        } else {
            for _ in 0..(rng.next_u32() % 3) {
                let at = rng.next_u32() as usize % z.len();
                z[at] ^= 1 << (rng.next_u32() % 8);
            }
        }
        let mut ours = Inflate::new(
            SliceInput::new(&z),
            Options {
                zlib: true,
                check_adler: true,
                limit: 1 << 24,
                flavor: Flavor::Fdeflate,
            },
        );
        let mut got = Vec::new();
        let ours = loop {
            match ours.fill(1 << 16) {
                Ok(n) => {
                    got.extend_from_slice(&ours.ready()[..n]);
                    ours.consume(n);
                    if ours.finished() {
                        break Ok(got);
                    }
                }
                Err(e) => break Err(e),
            }
        };
        let theirs = fdeflate::decompress_to_vec(&z);
        match (ours, theirs) {
            (Ok(a), Ok(b)) => {
                assert!(a == b, "{round}: 値が違う");
                same += 1;
            }
            (Err(_), Err(_)) => both_err += 1,
            (Ok(_), Err(e)) => panic!("{round}: 自作だけが読めた（fdeflate：{e:?}）"),
            (Err(e), Ok(_)) => panic!("{round}: fdeflate だけが読めた（自作：{e:?}）"),
        }
    }
    assert!(same > 500 && both_err > 300, "{same} {both_err}");
}

/// 固定のブロックの表は、続く固定のブロックで作り直さない（空の固定のブロックを並べた流れで、ブロックごとに作らない）。
#[test]
fn fixed_tables_are_built_once() {
    let mut w = BitW::new();
    for i in 0..10_000 {
        w.put(u32::from(i == 9_999), 1);
        w.put(1, 2);
        fixed_code(&mut w, 256);
    }
    let raw = w.finish();
    let mut z = Inflate::new(
        SliceInput::new(&raw),
        Options {
            zlib: false,
            check_adler: false,
            limit: 1 << 20,
            flavor: Flavor::Fdeflate,
        },
    );
    assert_eq!(z.fill(1).unwrap(), 0);
    assert!(z.finished());
    assert_eq!(z.builds, 1);
}

/// 出力の総量の見込みより先を求めても、`fill` は進む（置き場を伸ばす）。
#[test]
fn asking_beyond_the_hint_still_progresses() {
    let mut rng = crate::determinism::rng(11);
    let data = sample(&mut rng, 20_000);
    for level in [0u8, 6] {
        let zz = miniz_oxide::deflate::compress_to_vec_zlib(&data, level);
        let mut z = Inflate::new(
            SliceInput::new(&zz),
            Options {
                zlib: true,
                check_adler: true,
                limit: u64::MAX,
                flavor: Flavor::Zlib,
            },
        );
        z.set_size_hint(100);
        let n = z.fill(5000).unwrap();
        assert!(n >= 5000, "{n}");
        let mut out = z.ready()[..n].to_vec();
        z.consume(n);
        while !z.finished() {
            let n = z.fill(7000).unwrap();
            out.extend_from_slice(&z.ready()[..n]);
            z.consume(n);
        }
        assert!(out == data, "{level}");
    }
}
