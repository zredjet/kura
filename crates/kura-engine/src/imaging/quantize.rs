//! 減色（GIF の書き出しと、段 5 の変換の操作で使う）。整数だけで計算し、結果を凍結する。
//!
//! 1. 色（RGBA8 を `r<<24 | g<<16 | b<<8 | a` の 32 ビットにしたもの）と出現数の表を作る。
//! 2. 色の数が `max` 以下なら、その色をそのままパレットにする（小さい順。減色しない）。
//! 3. 多ければメディアンカット：
//!    - 分ける箱は、色が 2 つ以上ある箱のうち、いちばん幅の広いチャンネルの幅が最大のもの（同じなら出現数が多いもの、さらに同じなら前の箱）。
//!    - そのチャンネル（同じ幅なら r・g・b・a の順で前のもの）の値、同じなら色の数の順に並べ、
//!      出現数の累計の 2 倍が合計以上になる最初の位置で 2 つに分ける（両側に 1 色以上残す）。
//!    - 箱の色は、出現数で重み付けした各チャンネルの平均（四捨五入：`(Σ値·数 + 合計/2) / 合計`）。最後に小さい順に並べる。
//! 4. 各画素は、RGBA の差の 2 乗の和が最小のパレットの色にする（同じなら小さい番号）。

use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::monitor::Monitor;

/// RGBA8 を 32 ビットの数にする。
pub fn pack(c: [u8; 4]) -> u32 {
    u32::from_be_bytes(c)
}

/// 32 ビットの数を RGBA8 に戻す。
pub fn unpack(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// 色と出現数の表（色の小さい順）。`pixels` は RGBA8 を詰めたもの。
pub fn histogram(pixels: &[u8]) -> Result<Vec<(u32, u64)>, EngineError> {
    let mut colors: Vec<u32> = Vec::new();
    colors
        .try_reserve_exact(pixels.len() / 4)
        .map_err(|_| EngineError::Limit("色の表を確保できません".into()))?;
    colors.extend(
        pixels
            .chunks_exact(4)
            .map(|c| pack([c[0], c[1], c[2], c[3]])),
    );
    colors.sort();
    let mut out: Vec<(u32, u64)> = Vec::new();
    for c in colors {
        match out.last_mut() {
            Some((last, n)) if *last == c => *n += 1,
            _ => out.push((c, 1)),
        }
    }
    Ok(out)
}

/// `max` 色以下のパレットを作る（`max` は 1〜256）。
pub fn palette_for(hist: &[(u32, u64)], max: usize) -> Result<Vec<[u8; 4]>, EngineError> {
    if !(1..=256).contains(&max) {
        return Err(EngineError::Internal(format!(
            "パレットの色の数が範囲の外です（{max}）"
        )));
    }
    if hist.len() <= max {
        return Ok(hist.iter().map(|(c, _)| unpack(*c)).collect());
    }
    let mut boxes: Vec<Vec<(u32, u64)>> = vec![hist.to_vec()];
    while boxes.len() < max {
        // 分ける箱を選ぶ
        let mut best: Option<(usize, u32, u64)> = None;
        for (i, b) in boxes.iter().enumerate() {
            if b.len() < 2 {
                continue;
            }
            let (_, width) = widest(b);
            let total: u64 = b.iter().map(|e| e.1).sum();
            let better = match best {
                None => true,
                Some((_, bw, bt)) => width > bw || (width == bw && total > bt),
            };
            if better {
                best = Some((i, width, total));
            }
        }
        let Some((i, _, total)) = best else { break };
        let mut b = boxes.remove(i);
        let (ch, _) = widest(&b);
        b.sort_by_key(|e| (unpack(e.0)[ch], e.0));
        let mut acc = 0u64;
        let mut m = b.len() - 1;
        for (k, e) in b.iter().enumerate() {
            acc += e.1;
            if acc * 2 >= total {
                m = k + 1;
                break;
            }
        }
        let m = m.clamp(1, b.len() - 1);
        let right = b.split_off(m);
        // 箱の並びは結果に効かない（最後に色で並べる）が、選ぶときの「前の箱」を決めるために位置を保つ
        boxes.insert(i, right);
        boxes.insert(i, b);
    }
    let mut palette: Vec<[u8; 4]> = boxes.iter().map(|b| average(b)).collect();
    palette.sort_by_key(|c| pack(*c));
    Ok(palette)
}

/// いちばん幅の広いチャンネル（同じなら前のもの）と、その幅。
fn widest(b: &[(u32, u64)]) -> (usize, u32) {
    let mut lo = [u8::MAX; 4];
    let mut hi = [0u8; 4];
    for (c, _) in b {
        let v = unpack(*c);
        for k in 0..4 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    let mut best = (0usize, 0u32);
    for k in 0..4 {
        let w = u32::from(hi[k] - lo[k]);
        if w > best.1 {
            best = (k, w);
        }
    }
    best
}

fn average(b: &[(u32, u64)]) -> [u8; 4] {
    let total: u64 = b.iter().map(|e| e.1).sum();
    let mut out = [0u8; 4];
    if total == 0 {
        return out;
    }
    for (k, o) in out.iter_mut().enumerate() {
        let sum: u64 = b.iter().map(|(c, n)| u64::from(unpack(*c)[k]) * n).sum();
        *o = ((sum + total / 2) / total) as u8;
    }
    out
}

/// いちばん近いパレットの色の番号（差の 2 乗の和。同じなら小さい番号）。
pub fn nearest(c: [u8; 4], palette: &[[u8; 4]]) -> u8 {
    let mut best = (0usize, u32::MAX);
    for (i, p) in palette.iter().enumerate() {
        let d: u32 = (0..4)
            .map(|k| {
                let x = i32::from(c[k]) - i32::from(p[k]);
                (x * x) as u32
            })
            .sum();
        if d < best.1 {
            best = (i, d);
        }
    }
    best.0 as u8
}

/// 各画素（RGBA8 を詰めたもの）をパレットの番号にする。色ごとに 1 回だけ探す。
pub fn map_to_palette(
    pixels: &[u8],
    hist: &[(u32, u64)],
    palette: &[[u8; 4]],
    mon: &dyn Monitor,
) -> Result<Vec<u8>, EngineError> {
    if palette.is_empty() || palette.len() > 256 {
        return Err(EngineError::Internal(
            "パレットの色の数が範囲の外です".into(),
        ));
    }
    // 色の表の順に、いちばん近い番号を求める（並列。結果は位置で決まる）
    const BLOCK: usize = 4_096;
    let blocks = par_blocks(hist.len().div_ceil(BLOCK), mon, |b| {
        let end = (b * BLOCK + BLOCK).min(hist.len());
        Ok(hist[b * BLOCK..end]
            .iter()
            .map(|(c, _)| nearest(unpack(*c), palette))
            .collect::<Vec<u8>>())
    })?;
    let lookup: Vec<u8> = blocks.concat();
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(pixels.len() / 4)
        .map_err(|_| EngineError::Limit("番号の表を確保できません".into()))?;
    for c in pixels.chunks_exact(4) {
        let v = pack([c[0], c[1], c[2], c[3]]);
        let i = hist.partition_point(|(h, _)| *h < v);
        let idx = lookup
            .get(i)
            .filter(|_| hist.get(i).is_some_and(|(h, _)| *h == v))
            .ok_or_else(|| EngineError::Internal("色の表にない色があります".into()))?;
        out.push(*idx);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    #[test]
    fn few_colors_are_kept_exactly() {
        let px = [9, 9, 9, 255, 1, 2, 3, 255, 9, 9, 9, 255];
        let hist = histogram(&px).unwrap();
        assert_eq!(
            hist,
            vec![(pack([1, 2, 3, 255]), 1), (pack([9, 9, 9, 255]), 2)]
        );
        let pal = palette_for(&hist, 256).unwrap();
        assert_eq!(pal, vec![[1, 2, 3, 255], [9, 9, 9, 255]]);
        assert_eq!(
            map_to_palette(&px, &hist, &pal, &NoMonitor).unwrap(),
            vec![1, 0, 1]
        );
    }

    /// 色の多い画像を減色する：色の数は上限以下、結果は凍結した値と一致（入力の並べ方によらない）。
    #[test]
    fn median_cut_is_frozen_and_order_independent() {
        let mut px = Vec::new();
        for i in 0..4_096u32 {
            px.extend_from_slice(&[
                (i % 64 * 4) as u8,
                (i / 64 * 4) as u8,
                (i * 7 % 256) as u8,
                255,
            ]);
        }
        let hist = histogram(&px).unwrap();
        let pal = palette_for(&hist, 16).unwrap();
        assert_eq!(pal.len(), 16);
        let mut reversed: Vec<u8> = px.chunks_exact(4).rev().flatten().copied().collect();
        let hist_r = histogram(&reversed).unwrap();
        assert_eq!(palette_for(&hist_r, 16).unwrap(), pal);
        let idx = map_to_palette(&px, &hist, &pal, &NoMonitor).unwrap();
        assert!(idx.iter().all(|i| usize::from(*i) < 16));
        // 凍結：パレットのダイジェスト
        let flat: Vec<u8> = pal.iter().flatten().copied().collect();
        assert_eq!(
            blake3::hash(&flat).to_hex().as_str(),
            "ed6445f3fe8b1f4151a0d32648dbbe0f53b255690cf925d57fac45756585cea4"
        );
        reversed.clear();
    }

    #[test]
    fn nearest_prefers_lower_index_on_ties() {
        let pal = [[0, 0, 0, 255], [2, 0, 0, 255]];
        assert_eq!(nearest([1, 0, 0, 255], &pal), 0);
        assert_eq!(nearest([2, 0, 0, 255], &pal), 1);
    }
}
