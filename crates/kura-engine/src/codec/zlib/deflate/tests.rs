//! deflate の圧縮の試験。展開は自作（`inflate_exact`）と miniz_oxide の両方で照らす。

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::Rng;

use super::*;
use crate::codec::zlib::inflate_exact;

const LEVELS: [Level; 3] = [Level::Fast, Level::Balanced, Level::High];

/// いろいろな中身（雑音・繰り返し・文字・0 の列・遠い写し・PNG の行のような差分）。
fn sample(rng: &mut ChaCha8Rng, len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        let n = (rng.next_u32() as usize % 8192 + 1).min(len - v.len());
        match rng.next_u32() % 6 {
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
            4 => {
                // 小さい差分（フィルタを当てた行のような値）
                v.extend((0..n).map(|_| {
                    let r = rng.next_u32();
                    if r.is_multiple_of(3) {
                        0
                    } else {
                        (r >> 8) as u8 % 5
                    }
                }))
            }
            _ => {
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

fn check_round_trip(data: &[u8], level: Level, what: &str) -> Vec<u8> {
    let z = deflate_zlib(data, level);
    let back = inflate_exact(&z, data.len()).unwrap_or_else(|e| panic!("{what} {level:?}: {e:?}"));
    assert!(back == data, "{what} {level:?}: 自作の展開で違う");
    let m = miniz_oxide::inflate::decompress_to_vec_zlib(&z)
        .unwrap_or_else(|e| panic!("{what} {level:?}: miniz {e:?}"));
    assert!(m == data, "{what} {level:?}: miniz の展開で違う");
    z
}

#[test]
fn round_trips_on_small_and_edge_inputs() {
    let mut cases: Vec<(String, Vec<u8>)> = vec![
        ("空".into(), vec![]),
        ("1 バイト".into(), vec![7]),
        ("2 バイト".into(), vec![0, 0]),
        ("3 バイト".into(), vec![1, 1, 1]),
        ("0 の列 300".into(), vec![0; 300]),
        ("0 の列 1 MiB".into(), vec![0; 1 << 20]),
        (
            "文字".into(),
            b"abracadabra abracadabra abracadabra".to_vec(),
        ),
    ];
    // 長さ 258 ちょうどの一致・窓の端（32,768）の一致
    let mut far = vec![0u8; 70_000];
    let mut rng = crate::determinism::rng(1);
    for b in far.iter_mut().take(300) {
        *b = rng.next_u32() as u8;
    }
    let head = far[..300].to_vec();
    far[32_768..32_768 + 300].copy_from_slice(&head);
    far[32_769 + 300..32_769 + 600].copy_from_slice(&head);
    cases.push(("窓の端".into(), far));
    for n in [1usize, 2, 3, 4, 7, 8, 9, 255, 256, 257, 258, 259, 300] {
        cases.push((
            format!("繰り返し {n}"),
            (0..n * 3).map(|i| (i % n) as u8).collect(),
        ));
    }
    for (what, data) in &cases {
        for level in LEVELS {
            check_round_trip(data, level, what);
        }
    }
}

#[test]
fn round_trips_on_mixed_inputs() {
    let mut rng = crate::determinism::rng(2);
    for len in [1000usize, 70_000, 300_000, 1_500_000] {
        let data = sample(&mut rng, len);
        for level in LEVELS {
            check_round_trip(&data, level, &format!("混ぜたもの {len}"));
        }
    }
}

/// 雑音は stored のブロックになり、ほとんど伸びない。
#[test]
fn noise_is_stored() {
    let mut rng = crate::determinism::rng(3);
    let data: Vec<u8> = (0..400_000).map(|_| rng.next_u32() as u8).collect();
    for level in LEVELS {
        let z = check_round_trip(&data, level, "雑音");
        // 頭 2・Adler 4・ブロック（記号 16,384 個）ごとに 5 バイト
        assert!(
            z.len() <= data.len() + data.len() / 16_384 * 5 + 64,
            "{level:?} {}",
            z.len()
        );
    }
}

/// 入力を渡す切れ目によらず同じバイト列になる。
#[test]
fn output_does_not_depend_on_how_input_is_split() {
    let mut rng = crate::determinism::rng(4);
    let data = sample(&mut rng, 900_000);
    for level in LEVELS {
        let whole = deflate_zlib(&data, level);
        let mut d = Deflate::new(level);
        let mut at = 0;
        let mut out = Vec::new();
        while at < data.len() {
            let n = (rng.next_u32() as usize % 5000).min(data.len() - at);
            d.write(&data[at..at + n]);
            at += n;
            // 途中で取り出しても同じ
            out.append(d.output());
        }
        d.finish();
        out.append(d.output());
        assert!(out == whole, "{level:?}");
    }
}

/// 32 ビットの位置が回っても同じバイト列になる（位置は距離の引き算でしか使わない）。
#[test]
fn output_does_not_depend_on_the_absolute_position() {
    let mut rng = crate::determinism::rng(5);
    let data = sample(&mut rng, 600_000);
    for level in LEVELS {
        let a = deflate_zlib(&data, level);
        for base in [(1u64 << 32) - 100_000, (1u64 << 32) - 1, 5 << 32] {
            let mut d = Deflate::with_base(level, base);
            d.write(&data);
            d.finish();
            assert!(*d.output() == a, "{level:?} {base}");
        }
    }
}

/// 符号の長さは上限に収まり、完全な符号（Kraft の和が 1）になる。
#[test]
fn huffman_lengths_are_limited_and_complete() {
    let mut rng = crate::determinism::rng(6);
    let mut cases: Vec<(Vec<u32>, u32)> = Vec::new();
    // フィボナッチの頻度（制限がなければ長い符号になる）
    let mut fib = vec![1u32, 1];
    while fib.len() < 30 {
        let n = fib[fib.len() - 1] + fib[fib.len() - 2];
        fib.push(n);
    }
    cases.push((fib.clone(), 15));
    cases.push((fib[..19].to_vec(), 7));
    for _ in 0..300 {
        let n = [2usize, 3, 19, 30, 286][rng.next_u32() as usize % 5];
        let limit = if n == 19 { 7 } else { 15 };
        let f: Vec<u32> = (0..n)
            .map(|_| match rng.next_u32() % 4 {
                0 => 0,
                1 => 1,
                2 => rng.next_u32() % 100,
                _ => rng.next_u32() % 100_000,
            })
            .collect();
        cases.push((f, limit));
    }
    cases.push((vec![0; 30], 15));
    cases.push((vec![5, 0, 0], 15));
    for (f, limit) in &cases {
        let mut lens = vec![0u8; f.len()];
        huff_lengths(f, *limit, &mut lens);
        let used = lens.iter().filter(|&&l| l > 0).count();
        assert!(used >= 2, "{f:?}");
        assert!(
            lens.iter().all(|&l| u32::from(l) <= *limit),
            "{f:?} {lens:?}"
        );
        for (i, &x) in f.iter().enumerate() {
            assert!(x == 0 || lens[i] > 0, "使う記号に符号がない {f:?}");
        }
        let kraft: u64 = lens
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 1u64 << (15 - l))
            .sum();
        assert_eq!(kraft, 1 << 15, "{f:?} {lens:?}");
    }
}

/// 制限のない最小冗長の長さは、ハフマンの木（頻度の和の比べ）と同じ総ビット数になる。
#[test]
fn huffman_lengths_are_optimal_without_the_limit() {
    let mut rng = crate::determinism::rng(7);
    for _ in 0..200 {
        let n = 2 + rng.next_u32() as usize % 40;
        let f: Vec<u32> = (0..n).map(|_| 1 + rng.next_u32() % 1000).collect();
        let mut lens = vec![0u8; n];
        huff_lengths(&f, 31, &mut lens);
        let got: u64 = f
            .iter()
            .zip(&lens)
            .map(|(&a, &l)| u64::from(a) * u64::from(l))
            .sum();
        // 2 つずつ最小を足す（ハフマンの総コスト = 足した和の合計）
        let mut q: Vec<u64> = f.iter().map(|&x| u64::from(x)).collect();
        let mut want = 0u64;
        while q.len() > 1 {
            q.sort_by(|a, b| b.cmp(a));
            let a = q.pop().unwrap_or(0);
            let b = q.pop().unwrap_or(0);
            want += a + b;
            q.push(a + b);
        }
        assert_eq!(got, want, "{f:?}");
    }
}
