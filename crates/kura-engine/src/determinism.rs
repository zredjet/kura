//! 決定性の補助（docs/editing.md §8.2）。
//!
//! - 並列は、仕事をブロックに分けて添字の順に集める `par_blocks` だけを使う（rayon などのほかの口は clippy.toml で禁止）。
//!   スレッド数によらず、同じブロックの分け方・同じ足し合わせの順になる。
//! - 並列の分け方（`BAND_ROWS`・`AUDIO_BLOCK`・`TILE`）は速さのためだけの値で、変えても結果は変わらないように書く。
//!   結果に効く区切り（浮動小数の部分和）は、凍結した `SUM_BLOCK` で取る（`sum_blocks`）。
//! - 乱数は ChaCha8 だけ。種はノードの種・元のハッシュ・用途から `seed` で作る。
//! - 乱数の流れは**絶対位置**で決める（画像は行、音声はチャンネルを流れにして、列の位置へシークする。`rng_at`）。
//!   タイル・ブロック・フレームの番号を流れに使わない。分け方（TILE・AUDIO_BLOCK）を変えても、範囲だけ描いても同じ値になる。
//! - 種から ChaCha8 の鍵を作るのは自前の `key_from_seed`（rand_core の `seed_from_u64` の実装に頼らない）。

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
use rayon::prelude::*;

use crate::error::EngineError;
use crate::monitor::Monitor;

/// 画像を帯に分けるときの行数（並列の分け方だけ。結果に効く区切りに使わない）。
pub const BAND_ROWS: u32 = 64;
/// 音声を分けるときのフレーム数（並列の分け方だけ。結果に効く区切りに使わない）。
pub const AUDIO_BLOCK: usize = 4096;
/// 浮動小数の総和の区切り（要素数）。部分和はこの大きさの固定の区間（先頭から）ごとに取り、区間の順に足す。
/// **凍結する**：変えると総和の値が変わるので、変えるなら使う操作の版を上げる（§8.2）。
pub const SUM_BLOCK: usize = 4096;

/// splitmix64（kura-core の `random::splitmix64` と同じ値を返す）。
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// FNV-1a（64 ビット。kura-core の `random::fnv1a64` と同じ値を返す）。
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// 操作が使う乱数の種。ノードの種・元の中身のハッシュ・用途（操作の ID など）だけから決まる。
/// ノード ID・画面の操作の順・一括の並びに依存しないので、エディタと一括で同じになる（E4・E5）。
pub fn seed(node_seed: u64, source_hash: &[u8; 32], purpose: &str) -> u64 {
    let mut h = splitmix64(node_seed);
    h = splitmix64(h ^ fnv1a64(source_hash));
    splitmix64(h ^ fnv1a64(purpose.as_bytes()))
}

/// 種から ChaCha8 の 32 バイトの鍵を作る（splitmix64 を 4 回進めて、リトルエンディアンで並べる）。凍結する。
pub fn key_from_seed(seed: u64) -> [u8; 32] {
    let mut key = [0u8; 32];
    let mut state = seed;
    for chunk in key.chunks_exact_mut(8) {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        chunk.copy_from_slice(&splitmix64(state).to_le_bytes());
    }
    key
}

/// 種から乱数生成器を作る（流れ 0、先頭から）。
pub fn rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::from_seed(key_from_seed(seed))
}

/// 種と流れの番号から乱数生成器を作る（同じ種でも、流れごとに独立した列になる）。
pub fn rng_stream(seed: u64, stream: u64) -> ChaCha8Rng {
    let mut r = rng(seed);
    r.set_stream(stream);
    r
}

/// 絶対位置の乱数：流れ `stream`（画像なら行、音声ならチャンネル）の、`word` 番目の 32 ビット語から読む生成器。
/// 1 画素に k 語使うなら `word = x * k`。タイルやブロックの分け方によらず、同じ位置には同じ乱数が出る。
pub fn rng_at(seed: u64, stream: u64, word: u128) -> ChaCha8Rng {
    let mut r = rng_stream(seed, stream);
    r.set_word_pos(word);
    r
}

/// `0..count` の各ブロックを並列に計算し、添字の順に並べて返す。
/// ブロックの前で中止を確かめる。結果の順はスレッド数によらない。
pub fn par_blocks<T, F>(count: usize, mon: &dyn Monitor, f: F) -> Result<Vec<T>, EngineError>
where
    T: Send,
    F: Fn(usize) -> Result<T, EngineError> + Sync + Send,
{
    // 並列の口はここだけ（clippy.toml で、ほかの場所の into_par_iter などを禁じている）
    #[allow(clippy::disallowed_methods)]
    let blocks = (0..count).into_par_iter();
    blocks
        .map(|i| {
            if mon.canceled() {
                Err(EngineError::Canceled)
            } else {
                f(i)
            }
        })
        .collect()
}

/// ブロックごとの部分和を添字の順に足す（浮動小数の総和の順序を固定する）。
pub fn sum_ordered(parts: &[f64]) -> f64 {
    parts.iter().fold(0.0, |acc, x| acc + x)
}

/// `term(0)` から `term(len - 1)` までの総和。`SUM_BLOCK` ずつの区間の部分和を並列に取り、区間の順に足す。
/// 区間の中は添字の順に足す。スレッド数・並列の分け方によらず同じ値になる。
pub fn sum_blocks<F>(len: usize, mon: &dyn Monitor, term: F) -> Result<f64, EngineError>
where
    F: Fn(usize) -> f64 + Sync + Send,
{
    let parts = par_blocks(block_count(len, SUM_BLOCK), mon, |b| {
        let start = b * SUM_BLOCK;
        let end = start.saturating_add(SUM_BLOCK).min(len);
        Ok((start..end).fold(0.0, |acc, i| acc + term(i)))
    })?;
    Ok(sum_ordered(&parts))
}

/// `len` を `block` ずつに分けたときのブロック数。
pub fn block_count(len: usize, block: usize) -> usize {
    if block == 0 { 0 } else { len.div_ceil(block) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::{CancelFlag, NoMonitor};
    use rand_chacha::rand_core::Rng;

    #[test]
    fn mixers_match_kura_core() {
        // kura-core の random.rs のテストと同じ既知の値
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(splitmix64(1), splitmix64(2));
    }

    /// 種の作り方を凍結する（保存した文書の乱数が変わらないように）。
    #[test]
    fn seed_is_frozen() {
        let hash = [7u8; 32];
        assert_eq!(seed(1, &hash, "image.dither"), 0x20c3_9b48_fb8e_5ec5);
        assert_ne!(
            seed(1, &hash, "image.dither"),
            seed(2, &hash, "image.dither")
        );
        assert_ne!(
            seed(1, &hash, "image.dither"),
            seed(1, &[8u8; 32], "image.dither")
        );
        assert_ne!(
            seed(1, &hash, "image.dither"),
            seed(1, &hash, "audio.dither")
        );
    }

    /// 鍵の作り方を凍結する（rand_core の版によらない）。
    #[test]
    fn key_is_frozen() {
        let key = key_from_seed(1);
        assert_eq!(
            &key[..8],
            &splitmix64(1u64.wrapping_add(0x9E37_79B9_7F4A_7C15)).to_le_bytes()
        );
        use rand_chacha::rand_core::Rng;
        // ChaCha8 の実装（rand_chacha）と鍵の作り方の両方を凍結する
        assert_eq!(
            rng(1).next_u64(),
            7_432_140_298_125_134_002,
            "rng(1) の最初の値"
        );
    }

    #[test]
    fn rng_at_matches_sequential_reading() {
        use rand_chacha::rand_core::Rng;
        let mut seq = rng_stream(5, 3);
        let all: Vec<u32> = (0..100).map(|_| seq.next_u32()).collect();
        for start in [0u128, 1, 37, 99] {
            let mut r = rng_at(5, 3, start);
            assert_eq!(r.next_u32(), all[start as usize]);
        }
    }

    #[test]
    fn streams_are_independent_and_repeatable() {
        let a: Vec<u64> = (0..4)
            .map(|_| 0)
            .scan(rng_stream(9, 0), |r, _: u64| Some(r.next_u64()))
            .collect();
        let b: Vec<u64> = (0..4)
            .map(|_| 0)
            .scan(rng_stream(9, 0), |r, _: u64| Some(r.next_u64()))
            .collect();
        let c: Vec<u64> = (0..4)
            .map(|_| 0)
            .scan(rng_stream(9, 1), |r, _: u64| Some(r.next_u64()))
            .collect();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn par_blocks_keeps_order_for_any_thread_count() {
        let mut results = Vec::new();
        for threads in [1, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let parts = pool
                .install(|| par_blocks(1000, &NoMonitor, |i| Ok((i as f64).sqrt() * 0.1)))
                .unwrap();
            results.push(sum_ordered(&parts).to_bits());
        }
        assert!(results.windows(2).all(|w| w[0] == w[1]));
    }

    #[test]
    fn par_blocks_stops_when_canceled() {
        let flag = CancelFlag::default();
        flag.cancel();
        let r = par_blocks(10, &flag, Ok);
        assert!(matches!(r, Err(EngineError::Canceled)));
    }

    /// 総和は SUM_BLOCK ずつの区間で取り、スレッド数によらない。区間の切り方は凍結した値（先頭から SUM_BLOCK ずつ）。
    #[test]
    fn sum_blocks_is_thread_independent_and_uses_the_frozen_split() {
        let term = |i: usize| {
            (i as f64 * 0.37).sqrt() * 1e-3 + 1e10 * f64::from(u8::from(i.is_multiple_of(7)))
        };
        let len = SUM_BLOCK * 3 + 123;
        let mut results = Vec::new();
        for threads in [1, 3, 16] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            results.push(
                pool.install(|| sum_blocks(len, &NoMonitor, term))
                    .unwrap()
                    .to_bits(),
            );
        }
        assert!(results.windows(2).all(|w| w[0] == w[1]));
        // 参照：先頭から SUM_BLOCK ずつの部分和を、区間の順に足す
        let reference = (0..len)
            .collect::<Vec<_>>()
            .chunks(SUM_BLOCK)
            .map(|c| c.iter().fold(0.0, |acc, i| acc + term(*i)))
            .fold(0.0, |acc, x| acc + x);
        assert_eq!(results[0], reference.to_bits());
        assert_eq!(sum_blocks(0, &NoMonitor, term).unwrap(), 0.0);
    }

    #[test]
    fn block_count_rounds_up() {
        assert_eq!(block_count(0, 4096), 0);
        assert_eq!(block_count(1, 4096), 1);
        assert_eq!(block_count(4096, 4096), 1);
        assert_eq!(block_count(4097, 4096), 2);
    }
}
