//! シード付きの乱数（docs/playlists.md §5.4）。並べ替えはすべて ChaCha8 で行い、同じシードなら同じ結果にする。

use std::time::{SystemTime, UNIX_EPOCH};

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};

/// シードから乱数生成器を作る。
pub fn rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

/// 新しいシード（現在時刻を splitmix64 で混ぜたもの）。結果を再現したいときは、このシードを保存して使い回す。
pub fn new_seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    splitmix64(nanos)
}

/// splitmix64（シードを混ぜるのに使う）。
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `0..n` から一様に 1 つ選ぶ（n > 0）。64 ビットの乱数に n を掛けた上位を使う。
pub fn below(rng: &mut ChaCha8Rng, n: usize) -> usize {
    ((u128::from(rng.next_u64()) * n as u128) >> 64) as usize
}

/// Fisher–Yates でシャッフルする。
pub fn shuffle<T>(items: &mut [T], rng: &mut ChaCha8Rng) {
    for i in (1..items.len()).rev() {
        let j = below(rng, i + 1);
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_order() {
        let mut a: Vec<u32> = (0..50).collect();
        let mut b = a.clone();
        shuffle(&mut a, &mut rng(42));
        shuffle(&mut b, &mut rng(42));
        assert_eq!(a, b);
        assert_ne!(a, (0..50).collect::<Vec<_>>());

        let mut c: Vec<u32> = (0..50).collect();
        shuffle(&mut c, &mut rng(43));
        assert_ne!(a, c);

        let mut sorted = a.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn below_stays_in_range_and_covers_it() {
        let mut r = rng(1);
        let mut seen = [0usize; 7];
        for _ in 0..7000 {
            seen[below(&mut r, 7)] += 1;
        }
        assert!(seen.iter().all(|&n| n > 800), "{seen:?}");
    }

    #[test]
    fn seeds_differ_over_time() {
        assert_ne!(splitmix64(1), splitmix64(2));
        assert_ne!(new_seed(), 0);
    }
}
