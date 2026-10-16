//! 塗りつぶしの範囲（docs/editing.md §10、E22）。連結成分として定義するので、たどる順番に左右されない。
//!
//! - 起点の画素の色を t とし、各チャンネルの差がすべて許容差以下の画素を「同じ色」とする（8 ビットは差 ≤ tol、
//!   16 ビットは差 ≤ tol·257。アルファも比べる）。Indexed8 は番号が同じものだけ（許容差は 0 であること）。
//! - `contiguous` なら、起点から 4 連結（上下左右）か 8 連結（斜めも）で同じ色をたどれる画素。そうでなければ画像全体の同じ色の画素。
//! - 起点が画像の外なら、何も塗らない。

use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::geom::IRect;
use crate::imaging::pixels::is_16;
use crate::monitor::Monitor;
use crate::paint::mask::{Mask, span_bits};
use crate::value::{PixelFormat, Raster};

/// 連結の仕方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Connectivity {
    Four,
    Eight,
}

fn same(fmt: PixelFormat, a: &[u8], b: &[u8], tol: u8) -> bool {
    if fmt == PixelFormat::Indexed8 || tol == 0 {
        return a == b;
    }
    if is_16(fmt) {
        let t = u32::from(tol) * 257;
        a.chunks_exact(2).zip(b.chunks_exact(2)).all(|(x, y)| {
            let (x, y) = (
                u32::from(u16::from_le_bytes([x[0], x[1]])),
                u32::from(u16::from_le_bytes([y[0], y[1]])),
            );
            x.abs_diff(y) <= t
        })
    } else {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= tol)
    }
}

/// 同じ色の画素の集合（画像全体）。タイルごとに並列に求める（タイルの左端は 256 の倍数なので、語の境目にそろう）。
fn matching(
    px: &Raster,
    target: &[u8],
    tolerance: u8,
    mon: &dyn Monitor,
) -> Result<Mask, EngineError> {
    let (w, h) = (px.width(), px.height());
    let fmt = px.format();
    let bpp = px.bpp();
    let mut mask = Mask::new(IRect::new(0, 0, w, h))?;
    let (tx_n, ty_n) = (px.tiles_x(), px.tiles_y());
    let tiles = par_blocks((tx_n as usize) * (ty_n as usize), mon, |i| {
        let (tx, ty) = (i as u32 % tx_n, i as u32 / tx_n);
        let tr = px.tile_rect(tx, ty);
        let tile = px.tile(tx, ty);
        let tw = (tr.w as usize).div_ceil(64);
        let mut words = vec![0u64; tw * tr.h as usize];
        for (r, row) in tile.chunks_exact(tr.w as usize * bpp).enumerate() {
            let out = &mut words[r * tw..(r + 1) * tw];
            for (x, p) in row.chunks_exact(bpp).enumerate() {
                if same(fmt, p, target, tolerance) {
                    out[x / 64] |= 1 << (x % 64);
                }
            }
        }
        Ok((tr, tw, words))
    })?;
    for (tr, tw, words) in tiles {
        let k0 = tr.x as usize / 64;
        for (r, chunk) in words.chunks_exact(tw).enumerate() {
            mask.row_mut(tr.y as usize + r)[k0..k0 + tw].copy_from_slice(chunk);
        }
    }
    Ok(mask)
}

/// 行の語で、`from` 以上で `avail` のビットが立っている最初の位置（`to` 以下。なければ None）。
fn next_set(m: &[u64], f: &[u64], from: usize, to: usize) -> Option<usize> {
    let mut k = from / 64;
    let mut word = (m.get(k)? & !f[k]) & (u64::MAX << (from % 64));
    loop {
        if word != 0 {
            let x = k * 64 + word.trailing_zeros() as usize;
            return (x <= to).then_some(x);
        }
        k += 1;
        if k * 64 > to {
            return None;
        }
        word = m.get(k)? & !f[k];
    }
}

/// `x` を含む `avail` のビットの続き（区間 [l, r]）。
fn run_at(m: &[u64], f: &[u64], x: usize) -> (usize, usize) {
    let avail = |k: usize| m[k] & !f[k];
    // 右へ：x 以上で最初に 0 のビット
    let mut k = x / 64;
    let mut inv = !avail(k) & (u64::MAX << (x % 64));
    while inv == 0 {
        k += 1;
        if k >= m.len() {
            break;
        }
        inv = !avail(k);
    }
    let r = if inv == 0 {
        m.len() * 64 - 1
    } else {
        k * 64 + inv.trailing_zeros() as usize - 1
    };
    // 左へ：x 以下で最後に 0 のビット
    let mut k = x / 64;
    let keep = if x % 64 == 63 {
        u64::MAX
    } else {
        (1u64 << (x % 64 + 1)) - 1
    };
    let mut inv = !avail(k) & keep;
    loop {
        if inv != 0 {
            break;
        }
        if k == 0 {
            return (0, r);
        }
        k -= 1;
        inv = !avail(k);
    }
    let l = k * 64 + (63 - inv.leading_zeros() as usize) + 1;
    (l, r)
}

fn fill_span(f: &mut [u64], l: usize, r: usize) {
    for (k, word) in f.iter_mut().enumerate().take(r / 64 + 1).skip(l / 64) {
        *word |= span_bits(k, l, r + 1);
    }
}

/// 塗る画素の集合。起点が画像の外なら None。
pub fn fill_mask(
    px: &Raster,
    at: (i64, i64),
    tolerance: u8,
    connectivity: Connectivity,
    contiguous: bool,
    mon: &dyn Monitor,
) -> Result<Option<Mask>, EngineError> {
    let (w, h) = (px.width(), px.height());
    let (ax, ay) = at;
    if ax < 0 || ay < 0 || ax >= i64::from(w) || ay >= i64::from(h) {
        return Ok(None);
    }
    let target = px
        .pixel(ax as u32, ay as u32)
        .ok_or_else(|| EngineError::Internal("起点の画素が読めません".into()))?
        .to_vec();
    let m = matching(px, &target, tolerance, mon)?;
    if !contiguous {
        return Ok(Some(m));
    }
    // 連結成分：同じ色の区間（行の中の続き）ごとに埋め、上下の行の重なる区間（8 連結は 1 つ広げた範囲）を調べる。
    // 区間は語の演算で探すので、画素ごとに調べるより速い。結果は連結成分なので、調べる順によらない。
    let mut filled = Mask::new(IRect::new(0, 0, w, h))?;
    let d: usize = match connectivity {
        Connectivity::Four => 0,
        Connectivity::Eight => 1,
    };
    let last = w as usize - 1;
    // (行, 調べる範囲の左, 右)。積む前に、その範囲にまだ塗れる画素があるかを語で確かめる（無駄な要素を積まない）。
    // 要素は 12 バイト。山は、市松模様の 8 連結（区間が 1 画素ずつ）を中央から塗ると画素の約 0.3 になる（4×4 の周期の模様を
    // すべて試した最悪。区間は 1 行に w/2 個までなので、積む数は画素の数＋2h を超えない）。画素 2 つに 1 つ（6 バイト／画素）
    // ＋ 行の数の 2 倍を上限にし、超えたら落ちずに Limit にする
    let cap = u64::from(w) * u64::from(h) / 2 + 2 * u64::from(h) + (1 << 16);
    let mut queue: Vec<(u32, u32, u32)> = vec![(ay as u32, ax as u32, ax as u32)];
    let mut steps = 0u64;
    while let Some((y, lo, hi)) = queue.pop() {
        steps += 1;
        if steps.is_multiple_of(4_096) && mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let (y, mut x, hi) = (y as usize, lo as usize, hi as usize);
        while x <= hi {
            let Some(s) = next_set(m.row(y), filled.row(y), x, hi) else {
                break;
            };
            let (l, r) = run_at(m.row(y), filled.row(y), s);
            fill_span(filled.row_mut(y), l, r);
            let (nl, nr) = (l.saturating_sub(d), (r + d).min(last));
            for ny in [y.wrapping_sub(1), y + 1] {
                if ny >= h as usize || next_set(m.row(ny), filled.row(ny), nl, nr).is_none() {
                    continue;
                }
                if queue.len() as u64 >= cap {
                    return Err(EngineError::Limit(
                        "塗りつぶしの作業領域が大きすぎます（区間の多すぎる形）".into(),
                    ));
                }
                queue.try_reserve(1).map_err(|_| {
                    EngineError::Limit("塗りつぶしの作業領域を確保できません".into())
                })?;
                queue.push((ny as u32, nl as u32, nr as u32));
            }
            x = r + 2;
        }
    }
    Ok(Some(filled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use proptest::prelude::*;

    fn gray(w: u32, h: u32, data: &[u8]) -> Raster {
        Raster::from_rows(w, h, PixelFormat::Gray8, None, data).unwrap()
    }

    fn cells(m: &Mask) -> Vec<(i64, i64)> {
        let mut v = Vec::new();
        m.for_each(|x, y| {
            v.push((x, y));
            Ok(())
        })
        .unwrap();
        v
    }

    #[test]
    fn four_and_eight_connectivity() {
        // 斜めにつながった 0 の画素
        let px = gray(3, 3, &[0, 9, 9, 9, 0, 9, 9, 9, 0]);
        let four = fill_mask(&px, (0, 0), 0, Connectivity::Four, true, &NoMonitor)
            .unwrap()
            .unwrap();
        assert_eq!(cells(&four), [(0, 0)]);
        let eight = fill_mask(&px, (0, 0), 0, Connectivity::Eight, true, &NoMonitor)
            .unwrap()
            .unwrap();
        assert_eq!(cells(&eight), [(0, 0), (1, 1), (2, 2)]);
        let global = fill_mask(&px, (0, 1), 0, Connectivity::Four, false, &NoMonitor)
            .unwrap()
            .unwrap();
        assert_eq!(global.count(), 6);
        assert!(
            fill_mask(&px, (3, 0), 0, Connectivity::Four, true, &NoMonitor)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn tolerance_is_per_channel() {
        let px = gray(4, 1, &[100, 103, 104, 100]);
        let m = fill_mask(&px, (0, 0), 3, Connectivity::Four, true, &NoMonitor)
            .unwrap()
            .unwrap();
        assert_eq!(cells(&m), [(0, 0), (1, 0)]);
        let m16 = Raster::from_rows(2, 1, PixelFormat::Gray16, None, &[0, 0, 1, 1]).unwrap();
        // 差 257 は許容差 1（×257）に収まる
        assert_eq!(
            fill_mask(&m16, (0, 0), 1, Connectivity::Four, true, &NoMonitor)
                .unwrap()
                .unwrap()
                .count(),
            2
        );
        assert_eq!(
            fill_mask(&m16, (0, 0), 0, Connectivity::Four, true, &NoMonitor)
                .unwrap()
                .unwrap()
                .count(),
            1
        );
    }

    /// たどる順に左右されない：何度も広げて止まるまで繰り返す素朴な定義と同じ集合になる（E22）。
    fn naive(px: &Raster, at: (u32, u32), tol: u8, conn: Connectivity) -> Vec<(i64, i64)> {
        let (w, h) = (px.width(), px.height());
        let t = px.pixel(at.0, at.1).unwrap().to_vec();
        let mut inside = vec![false; (w * h) as usize];
        inside[(at.1 * w + at.0) as usize] = true;
        loop {
            let mut changed = false;
            for y in 0..h {
                for x in 0..w {
                    if inside[(y * w + x) as usize]
                        || !same(px.format(), px.pixel(x, y).unwrap(), &t, tol)
                    {
                        continue;
                    }
                    let near = (-1i64..=1).any(|dy| {
                        (-1i64..=1).any(|dx| {
                            let ok = match conn {
                                Connectivity::Four => (dx == 0) != (dy == 0),
                                Connectivity::Eight => dx != 0 || dy != 0,
                            };
                            let (nx, ny) = (i64::from(x) + dx, i64::from(y) + dy);
                            ok && nx >= 0
                                && ny >= 0
                                && nx < i64::from(w)
                                && ny < i64::from(h)
                                && inside[(ny as u32 * w + nx as u32) as usize]
                        })
                    });
                    if near {
                        inside[(y * w + x) as usize] = true;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|(x, y)| inside[(y * w + x) as usize])
            .map(|(x, y)| (i64::from(x), i64::from(y)))
            .collect()
    }

    proptest! {
        #[test]
        fn matches_the_naive_definition(data in proptest::collection::vec(0u8..4, 48), x in 0u32..8, y in 0u32..6, tol in 0u8..3, eight: bool) {
            let px = gray(8, 6, &data);
            let conn = if eight { Connectivity::Eight } else { Connectivity::Four };
            let m = fill_mask(&px, (i64::from(x), i64::from(y)), tol, conn, true, &NoMonitor).unwrap().unwrap();
            prop_assert_eq!(cells(&m), naive(&px, (x, y), tol, conn));
        }

        /// 市松模様の 8 連結を、どの起点から塗っても待ち行列の上限に当たらない（中央からは山が画素の約 0.3）。
        #[test]
        fn checkerboards_fill_from_any_origin(x in 0u32..256, y in 0u32..256) {
            let n = 256u32;
            let data: Vec<u8> = (0..n * n).map(|i| (((i % n) + (i / n)) % 2) as u8).collect();
            let px = gray(n, n, &data);
            let m = fill_mask(&px, (i64::from(x), i64::from(y)), 0, Connectivity::Eight, true, &NoMonitor).unwrap().unwrap();
            prop_assert_eq!(m.count(), u64::from(n * n / 2));
        }

        /// 語の境目（64 画素）をまたぐ幅でも同じ。
        #[test]
        fn matches_the_naive_definition_across_words(w in 60u32..140, h in 1u32..5, seed in 0u64..1_000, x in 0u32..60, eight: bool) {
            let data: Vec<u8> = (0..w * h).map(|i| u8::from(crate::determinism::splitmix64(seed ^ u64::from(i)).is_multiple_of(5))).collect();
            let px = gray(w, h, &data);
            let y = h / 2;
            let conn = if eight { Connectivity::Eight } else { Connectivity::Four };
            let m = fill_mask(&px, (i64::from(x), i64::from(y)), 0, conn, true, &NoMonitor).unwrap().unwrap();
            prop_assert_eq!(cells(&m), naive(&px, (x, y), 0, conn));
        }
    }
}
