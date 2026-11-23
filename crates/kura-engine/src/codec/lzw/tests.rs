//! GIF の LZW の試験。照合の相手は weezl 0.1.12（前の gif クレートの中の展開と圧縮）。

use rand_chacha::rand_core::Rng;

use super::*;

/// 展開を最後まで（出力は `n` まで）。
fn decode_all(z: &[u8], min: u32, n: usize, step: usize) -> Result<Vec<u8>, InvalidCode> {
    let mut d = Decoder::new(min);
    let mut out = vec![0u8; n];
    let mut at = 0;
    for piece in z.chunks(step.max(1)) {
        d.feed(piece, &mut out, &mut at)?;
    }
    out.truncate(at.min(n));
    Ok(out)
}

/// 行ずつの置き場（幅 `w`）で展開する（入りきらない文字列は次の行へ続く）。
fn decode_rows(
    z: &[u8],
    min: u32,
    n: usize,
    w: usize,
    step: usize,
) -> Result<Vec<u8>, InvalidCode> {
    let mut d = Decoder::new(min);
    let mut all = Vec::new();
    let mut row = vec![0u8; w.max(1)];
    let mut filled = 0;
    let flush = |row: &mut Vec<u8>, filled: &mut usize, all: &mut Vec<u8>| {
        all.extend_from_slice(&row[..*filled]);
        *filled = 0;
    };
    for piece in z.chunks(step.max(1)) {
        let mut p = piece;
        loop {
            if all.len() + filled >= n {
                break;
            }
            let used = d.feed(p, &mut row, &mut filled)?;
            p = &p[used..];
            if filled == row.len() {
                flush(&mut row, &mut filled, &mut all);
            } else if p.is_empty() {
                break;
            }
        }
    }
    while d.has_pending() && all.len() + filled < n {
        d.feed(&[], &mut row, &mut filled)?;
        flush(&mut row, &mut filled, &mut all);
    }
    flush(&mut row, &mut filled, &mut all);
    all.truncate(n);
    Ok(all)
}

/// weezl で展開する（出力は `n` まで。前の gif クレートと同じく、満ちたら止める）。
fn weezl_decode(z: &[u8], min: u32, n: usize) -> Result<Vec<u8>, ()> {
    let mut d = weezl::decode::Decoder::new(weezl::BitOrder::Lsb, min as u8);
    let mut out = vec![0u8; n];
    let mut written = 0;
    let mut inp = z;
    // weezl は入力も出力も進めずに clear だけを処理する回がある（ビットの置き場の中の符号）。回の数で止める
    for _ in 0..z.len() * 8 + 16 {
        let r = d.decode_bytes(inp, &mut out[written..]);
        inp = &inp[r.consumed_in..];
        written += r.consumed_out;
        match r.status {
            Err(_) => return Err(()),
            Ok(weezl::LzwStatus::Done) => break,
            Ok(weezl::LzwStatus::NoProgress) => break,
            Ok(weezl::LzwStatus::Ok) => {
                if written == n {
                    break;
                }
            }
        }
    }
    out.truncate(written);
    Ok(out)
}

fn sample(rng: &mut rand_chacha::ChaCha8Rng, n: usize, colors: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        let run = 1 + rng.next_u32() as usize % 40;
        match rng.next_u32() % 3 {
            0 => {
                let c = (rng.next_u32() % colors) as u8;
                v.extend(std::iter::repeat_n(c, run));
            }
            1 => v.extend((0..run).map(|_| (rng.next_u32() % colors) as u8)),
            _ => {
                if v.len() > 50 {
                    let at = rng.next_u32() as usize % (v.len() - 20);
                    let part = v[at..at + 20].to_vec();
                    v.extend(part);
                }
            }
        }
    }
    v.truncate(n);
    v
}

/// 圧縮して展開すると元に戻る（自作と weezl の両方で）。表が何度も満ちる長さ・どの最小の符号の長さでも。
#[test]
fn round_trips_through_both_decoders() {
    let mut rng = crate::determinism::rng(1);
    for min in 2..=8u32 {
        for n in [0usize, 1, 2, 3, 7, 100, 5000, 70_000, 300_000] {
            let colors = 1 << min;
            let data = sample(&mut rng, n, colors);
            let mut z = Vec::new();
            assert!(encode(&data, min, &mut z));
            for step in [1usize, 3, 255, 1 << 20] {
                assert_eq!(
                    decode_all(&z, min, n, step).unwrap(),
                    data,
                    "{min} {n} {step}"
                );
                if n <= 70_000 {
                    for w in [1usize, 2, 7, 64, 333] {
                        assert_eq!(
                            decode_rows(&z, min, n, w, step).unwrap(),
                            data,
                            "行 {w} {min} {n} {step}"
                        );
                    }
                }
            }
            assert_eq!(weezl_decode(&z, min, n).unwrap(), data, "weezl {min} {n}");
        }
    }
}

/// 番号が最小の符号の長さの外なら書かない。
#[test]
fn out_of_range_indices_are_refused() {
    let mut z = Vec::new();
    assert!(!encode(&[0, 4], 2, &mut z));
    assert!(z.is_empty());
    assert!(!encode(&[1], 1, &mut z));
    assert_eq!(min_size_for(&[]), 2);
    assert_eq!(min_size_for(&[3]), 2);
    assert_eq!(min_size_for(&[4]), 3);
    assert_eq!(min_size_for(&[255]), 8);
}

/// 乱数のバイト列（壊れた流れを含む）を weezl と同じに読む：weezl が誤りなく出したものは同じ値（自作は出力が満ちたら
/// 次の符号を読まないので、weezl が満ちたあとの符号で誤りにしたものも読める）。自作が誤りにするのは weezl も誤りにするもの
/// だけ。
#[test]
fn random_streams_match_weezl() {
    let mut rng = crate::determinism::rng(2);
    for i in 0..20_000 {
        let min = 1 + rng.next_u32() % 11;
        let len = rng.next_u32() as usize % 300;
        let mut z: Vec<u8> = (0..len).map(|_| rng.next_u32() as u8).collect();
        if i % 3 == 0 {
            // 正しい流れを少し壊す
            let data = sample(&mut rng, 200, 1 << min.min(8));
            let m = min.clamp(2, 8);
            z.clear();
            encode(&data, m, &mut z);
            if !z.is_empty() {
                let at = rng.next_u32() as usize % z.len();
                z[at] ^= 1 << (rng.next_u32() % 8);
            }
            let n = 1 + rng.next_u32() as usize % 400;
            check(&z, m, n);
            continue;
        }
        let n = 1 + rng.next_u32() as usize % 400;
        check(&z, min, n);
    }
}

fn check(z: &[u8], min: u32, n: usize) {
    let ours = decode_all(z, min, n, 1 + (z.len() % 7));
    let theirs = weezl_decode(z, min, n);
    match (&ours, &theirs) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "{z:?} {min} {n}"),
        (Ok(a), Err(())) => {
            // weezl が満ちたあとの符号で誤りにしたものだけ
            assert_eq!(a.len(), n, "{z:?} {min} {n}");
        }
        (Err(_), Ok(b)) => panic!("自作だけが誤り {z:?} {min} {n} {b:?}"),
        (Err(_), Err(())) => {}
    }
}
