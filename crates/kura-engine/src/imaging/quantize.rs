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

fn no_room(what: &str) -> EngineError {
    EngineError::Limit(format!("{what}を確保できません"))
}

/// 中止を確かめながら並べる（ブロックごとに並べてから、2 つずつ併合する。u32 の並べ替えは結果が 1 つに決まる）。
fn sort_cancelable(v: &mut Vec<u32>, mon: &dyn Monitor) -> Result<(), EngineError> {
    const CHUNK: usize = 1 << 20;
    for c in v.chunks_mut(CHUNK) {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        c.sort();
    }
    let mut width = CHUNK;
    let mut buf: Vec<u32> = Vec::new();
    while width < v.len() {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        buf.clear();
        buf.try_reserve_exact(v.len())
            .map_err(|_| no_room("色の表"))?;
        for pair in v.chunks(width * 2) {
            let (a, b) = pair.split_at(width.min(pair.len()));
            let (mut i, mut j) = (0, 0);
            while i < a.len() && j < b.len() {
                if a[i] <= b[j] {
                    buf.push(a[i]);
                    i += 1;
                } else {
                    buf.push(b[j]);
                    j += 1;
                }
            }
            buf.extend_from_slice(&a[i..]);
            buf.extend_from_slice(&b[j..]);
        }
        std::mem::swap(v, &mut buf);
        width *= 2;
    }
    Ok(())
}

/// 色と出現数の表（色の小さい順）。`pixels` は RGBA8 を詰めたもの。並べ替えの途中でも中止を確かめる。
pub fn histogram(pixels: &[u8], mon: &dyn Monitor) -> Result<Vec<(u32, u64)>, EngineError> {
    let mut colors: Vec<u32> = Vec::new();
    colors
        .try_reserve_exact(pixels.len() / 4)
        .map_err(|_| no_room("色の表"))?;
    colors.extend(
        pixels
            .chunks_exact(4)
            .map(|c| pack([c[0], c[1], c[2], c[3]])),
    );
    sort_cancelable(&mut colors, mon)?;
    let mut out: Vec<(u32, u64)> = Vec::new();
    for c in colors {
        match out.last_mut() {
            Some((last, n)) if *last == c => *n += 1,
            _ => {
                out.try_reserve(1).map_err(|_| no_room("色の表"))?;
                out.push((c, 1));
            }
        }
    }
    Ok(out)
}

/// 2 つの色の表を併合する（同じ色は数を足す。結果も色の小さい順）。フレームごとの表をまとめるのに使う。
pub fn merge_histograms(
    a: &[(u32, u64)],
    b: &[(u32, u64)],
) -> Result<Vec<(u32, u64)>, EngineError> {
    let mut out: Vec<(u32, u64)> = Vec::new();
    out.try_reserve_exact(a.len() + b.len())
        .map_err(|_| no_room("色の表"))?;
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let next = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) if x.0 == y.0 => {
                i += 1;
                j += 1;
                (x.0, x.1 + y.1)
            }
            (Some(x), Some(y)) if x.0 < y.0 => {
                i += 1;
                *x
            }
            (Some(_), Some(y)) | (None, Some(y)) => {
                j += 1;
                *y
            }
            (Some(x), None) => {
                i += 1;
                *x
            }
            (None, None) => break,
        };
        out.push(next);
    }
    Ok(out)
}

/// `max` 色以下のパレットを作る（`max` は 1〜256）。
/// 分ける 1 回ごとに中止を確かめる。
pub fn palette_for(
    hist: &[(u32, u64)],
    max: usize,
    mon: &dyn Monitor,
) -> Result<Vec<[u8; 4]>, EngineError> {
    if !(1..=256).contains(&max) {
        return Err(EngineError::Internal(format!(
            "パレットの色の数が範囲の外です（{max}）"
        )));
    }
    if hist.len() <= max {
        return Ok(hist.iter().map(|(c, _)| unpack(*c)).collect());
    }
    let mut first: Vec<(u32, u64)> = Vec::new();
    first
        .try_reserve_exact(hist.len())
        .map_err(|_| no_room("減色の箱"))?;
    first.extend_from_slice(hist);
    let mut boxes: Vec<Vec<(u32, u64)>> = vec![first];
    while boxes.len() < max {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
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

/// 色の表の各色の、いちばん近いパレットの番号（並列。結果は位置で決まる）。フレームの列では 1 回だけ作って使い回す。
pub fn palette_lookup(
    hist: &[(u32, u64)],
    palette: &[[u8; 4]],
    mon: &dyn Monitor,
) -> Result<Vec<u8>, EngineError> {
    if palette.is_empty() || palette.len() > 256 {
        return Err(EngineError::Internal(
            "パレットの色の数が範囲の外です".into(),
        ));
    }
    const BLOCK: usize = 4_096;
    let blocks = par_blocks(hist.len().div_ceil(BLOCK), mon, |b| {
        let end = (b * BLOCK + BLOCK).min(hist.len());
        Ok(hist[b * BLOCK..end]
            .iter()
            .map(|(c, _)| nearest(unpack(*c), palette))
            .collect::<Vec<u8>>())
    })?;
    Ok(blocks.concat())
}

/// 各画素（RGBA8 を詰めたもの）を、`palette_lookup` の表でパレットの番号にする。
pub fn map_with_lookup(
    pixels: &[u8],
    hist: &[(u32, u64)],
    lookup: &[u8],
    mon: &dyn Monitor,
) -> Result<Vec<u8>, EngineError> {
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(pixels.len() / 4)
        .map_err(|_| EngineError::Limit("番号の表を確保できません".into()))?;
    for (k, c) in pixels.chunks_exact(4).enumerate() {
        if k % 65_536 == 65_535 && mon.canceled() {
            return Err(EngineError::Canceled);
        }
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

/// 各画素（RGBA8 を詰めたもの）をパレットの番号にする。色ごとに 1 回だけ探す。
pub fn map_to_palette(
    pixels: &[u8],
    hist: &[(u32, u64)],
    palette: &[[u8; 4]],
    mon: &dyn Monitor,
) -> Result<Vec<u8>, EngineError> {
    let lookup = palette_lookup(hist, palette, mon)?;
    map_with_lookup(pixels, hist, &lookup, mon)
}

/// 色の表をまとめて併合する（2 つずつの木の形で併合するので、手間は色の数 × log(表の数)）。
pub fn merge_all(
    mut hists: Vec<Vec<(u32, u64)>>,
    mon: &dyn Monitor,
) -> Result<Vec<(u32, u64)>, EngineError> {
    while hists.len() > 1 {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let mut next = Vec::with_capacity(hists.len().div_ceil(2));
        let mut it = hists.into_iter();
        while let Some(a) = it.next() {
            match it.next() {
                Some(b) => next.push(merge_histograms(&a, &b)?),
                None => next.push(a),
            }
        }
        hists = next;
    }
    Ok(hists.pop().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ブロックに分けた並べ替えと併合は、ふつうの並べ替えと同じ。フレームごとの表を併合しても同じ。
    #[test]
    fn chunked_sort_and_merge_match_a_plain_sort() {
        let mut r = crate::determinism::rng(5);
        use rand_chacha::rand_core::Rng;
        let n = (1 << 20) * 3 + 12_345;
        let v: Vec<u32> = (0..n).map(|_| r.next_u32() % 50_000).collect();
        let mut a = v.clone();
        sort_cancelable(&mut a, &NoMonitor).unwrap();
        let mut b = v.clone();
        b.sort();
        assert_eq!(a, b);
        let px: Vec<u8> = v.iter().flat_map(|c| c.to_be_bytes()).collect();
        let whole = histogram(&px, &NoMonitor).unwrap();
        let (x, y) = px.split_at(px.len() / 3 / 4 * 4);
        let parts = merge_histograms(
            &histogram(x, &NoMonitor).unwrap(),
            &histogram(y, &NoMonitor).unwrap(),
        )
        .unwrap();
        assert_eq!(parts, whole);
        let flag = crate::monitor::CancelFlag::default();
        flag.cancel();
        assert!(matches!(histogram(&px, &flag), Err(EngineError::Canceled)));
    }
    use crate::monitor::NoMonitor;

    #[test]
    fn few_colors_are_kept_exactly() {
        let px = [9, 9, 9, 255, 1, 2, 3, 255, 9, 9, 9, 255];
        let hist = histogram(&px, &NoMonitor).unwrap();
        assert_eq!(
            hist,
            vec![(pack([1, 2, 3, 255]), 1), (pack([9, 9, 9, 255]), 2)]
        );
        let pal = palette_for(&hist, 256, &NoMonitor).unwrap();
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
        let hist = histogram(&px, &NoMonitor).unwrap();
        let pal = palette_for(&hist, 16, &NoMonitor).unwrap();
        assert_eq!(pal.len(), 16);
        let mut reversed: Vec<u8> = px.chunks_exact(4).rev().flatten().copied().collect();
        let hist_r = histogram(&reversed, &NoMonitor).unwrap();
        assert_eq!(palette_for(&hist_r, 16, &NoMonitor).unwrap(), pal);
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
