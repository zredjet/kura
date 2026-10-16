//! パレットへの割り当てとディザ（`image.convert` で Indexed8 にするとき）。整数だけで、式を凍結する。
//!
//! - なし：各画素をいちばん近いパレットの色にする（`quantize::nearest`：RGBA の差の 2 乗の和、同じなら小さい番号）。
//! - 組織的（ordered）：8×8 の Bayer 行列 B で、RGB に `⌊(2·B[y mod 8][x mod 8] + 1 − 64)·S / 128⌋`（床。負の数も小さいほうへ）を
//!   足して 0〜255 に切ってから、いちばん近い色にする。S は、`k³ ≥ 色の数` となる最小の k で `⌊255 / k⌋`（色の間隔の目安）。
//!   x・y はキャンバスの座標（フレームの位置を足したもの）なので、トリムしたフレームでも同じ位置に同じしきい値が当たる。
//! - 誤差拡散（Floyd–Steinberg）：行を上から、各行を左から。誤差は 16 倍の整数で持ち、画素の値は
//!   `clamp(c + ⌊(e + 8) / 16⌋, 0, 255)`。いちばん近い色との差 d を、右に 7d・左下に 3d・下に 5d・右下に 1d 足す（画像の外は捨てる）。
//! - どちらのディザも、アルファが 0 の画素は手を加えず（そのままいちばん近い色）、誤差も受け取らず配らない。
//!   アルファには手を加えない（RGB だけ）。

use crate::determinism::par_blocks;
use crate::error::EngineError;
use crate::imaging::quantize::nearest;
use crate::monitor::Monitor;

/// ディザの方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dither {
    None,
    Ordered,
    ErrorDiffusion,
}

const BAYER: [[i32; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

/// 組織的ディザの幅 S。
pub fn spread(colors: usize) -> i32 {
    let mut k = 1i32;
    while (k as usize).pow(3) < colors.max(1) {
        k += 1;
    }
    255 / k
}

/// いちばん近い色を覚えておく表（純粋な関数の結果を覚えるだけなので、結果は変わらない）。
struct Cache {
    keys: Vec<u32>,
    vals: Vec<u8>,
}

impl Cache {
    const SIZE: usize = 4_096;

    fn new() -> Self {
        Self {
            keys: vec![u32::MAX; Self::SIZE],
            vals: vec![0; Self::SIZE],
        }
    }

    fn get(&mut self, c: [u8; 4], palette: &[[u8; 4]]) -> u8 {
        let key = u32::from_be_bytes(c);
        let slot = (key.wrapping_mul(0x9E37_79B1) >> 20) as usize % Self::SIZE;
        // u32::MAX（白で不透明）は空きの印と同じなので、表に入れずに毎回求める
        if key != u32::MAX && self.keys[slot] == key {
            return self.vals[slot];
        }
        let v = nearest(c, palette);
        self.keys[slot] = key;
        self.vals[slot] = v;
        v
    }
}

/// `w × h` の RGBA8 の画素（行を詰めたもの）を、パレットの番号にする。
/// `origin` は画素 (0, 0) のキャンバスの座標（組織的ディザのしきい値の位置。画像は (0, 0)、フレームはその位置）。
pub fn map_rgba8(
    pixels: &[u8],
    w: u32,
    h: u32,
    origin: (u64, u64),
    palette: &[[u8; 4]],
    dither: Dither,
    mon: &dyn Monitor,
) -> Result<Vec<u8>, EngineError> {
    let (w, h) = (w as usize, h as usize);
    if pixels.len() != w * h * 4 || palette.is_empty() || palette.len() > 256 {
        return Err(EngineError::Internal(
            "パレットへの割り当ての大きさが合いません".into(),
        ));
    }
    match dither {
        Dither::None | Dither::Ordered => {
            const BAND: usize = 64;
            let s = spread(palette.len());
            let bands = par_blocks(h.div_ceil(BAND), mon, |b| {
                let mut cache = Cache::new();
                let rows = b * BAND..((b + 1) * BAND).min(h);
                let mut out = Vec::with_capacity(rows.len() * w);
                for y in rows {
                    for x in 0..w {
                        let i = (y * w + x) * 4;
                        let mut c = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
                        if dither == Dither::Ordered && c[3] != 0 {
                            let (bx, by) = (
                                ((x as u64).wrapping_add(origin.0) % 8) as usize,
                                ((y as u64).wrapping_add(origin.1) % 8) as usize,
                            );
                            let off = ((2 * BAYER[by][bx] + 1 - 64) * s).div_euclid(128);
                            for v in c.iter_mut().take(3) {
                                *v = (i32::from(*v) + off).clamp(0, 255) as u8;
                            }
                        }
                        out.push(cache.get(c, palette));
                    }
                }
                Ok(out)
            })?;
            Ok(bands.concat())
        }
        Dither::ErrorDiffusion => {
            let mut out = Vec::with_capacity(w * h);
            let mut cache = Cache::new();
            // 誤差（16 倍）。今の行と次の行
            let mut cur = vec![[0i32; 3]; w + 2];
            let mut nxt = vec![[0i32; 3]; w + 2];
            for y in 0..h {
                if y % 64 == 0 && mon.canceled() {
                    return Err(EngineError::Canceled);
                }
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    let a = pixels[i + 3];
                    if a == 0 {
                        out.push(cache.get([pixels[i], pixels[i + 1], pixels[i + 2], a], palette));
                        continue;
                    }
                    let mut want = [0u8; 4];
                    for k in 0..3 {
                        let e = (cur[x + 1][k] + 8).div_euclid(16);
                        want[k] = (i32::from(pixels[i + k]) + e).clamp(0, 255) as u8;
                    }
                    want[3] = a;
                    let idx = cache.get(want, palette);
                    out.push(idx);
                    let q = palette[usize::from(idx)];
                    for k in 0..3 {
                        let d = i32::from(want[k]) - i32::from(q[k]);
                        cur[x + 2][k] += d * 7;
                        nxt[x][k] += d * 3;
                        nxt[x + 1][k] += d * 5;
                        nxt[x + 2][k] += d;
                    }
                }
                std::mem::swap(&mut cur, &mut nxt);
                nxt.iter_mut().for_each(|e| *e = [0; 3]);
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;

    #[test]
    fn spread_follows_the_palette_size() {
        assert_eq!(spread(256), 36);
        assert_eq!(spread(16), 85);
        assert_eq!(spread(2), 127);
        assert_eq!(spread(1), 255);
    }

    /// 2 色（黒・白）で 50% の灰色をディザすると、半分ずつになる。なしなら全部同じ色。
    #[test]
    fn mid_gray_dithers_to_half_and_half() {
        let pal = [[0, 0, 0, 255], [255, 255, 255, 255]];
        let (w, h) = (16u32, 16u32);
        let px: Vec<u8> = (0..w * h).flat_map(|_| [128u8, 128, 128, 255]).collect();
        let none = map_rgba8(&px, w, h, (0, 0), &pal, Dither::None, &NoMonitor).unwrap();
        assert!(none.iter().all(|i| *i == 1));
        for d in [Dither::Ordered, Dither::ErrorDiffusion] {
            let out = map_rgba8(&px, w, h, (0, 0), &pal, d, &NoMonitor).unwrap();
            let white = out.iter().filter(|i| **i == 1).count();
            assert!((120..=136).contains(&white), "{d:?}: {white}");
        }
    }

    /// 透明な画素は誤差を受け取らない・配らない。
    #[test]
    fn transparent_pixels_are_left_alone() {
        let pal = [[0, 0, 0, 0], [0, 0, 0, 255], [255, 255, 255, 255]];
        let px = [200, 200, 200, 255, 0, 0, 0, 0, 10, 10, 10, 255];
        let out = map_rgba8(&px, 3, 1, (0, 0), &pal, Dither::ErrorDiffusion, &NoMonitor).unwrap();
        assert_eq!(out, [2, 0, 1]);
    }

    /// 組織的ディザの式は床（負の分子も小さいほうへ）。(0, 0) の B は 0 なので分子は −63·S。
    /// パレット {黒, (3,3,3)}・S = 127 で (64,64,64) は −63 で 1 になり 0 番（0 への切り捨てなら −62 で 2、1 番）。
    #[test]
    fn ordered_offset_floors() {
        let pal = [[0, 0, 0, 255], [3, 3, 3, 255]];
        let out = map_rgba8(
            &[64, 64, 64, 255],
            1,
            1,
            (0, 0),
            &pal,
            Dither::Ordered,
            &NoMonitor,
        )
        .unwrap();
        assert_eq!(out, [0]);
        // 原点をずらすと、キャンバスの同じ位置に同じしきい値が当たる
        let px: Vec<u8> = (0..16u32 * 4)
            .flat_map(|i| [(i * 3) as u8, (i * 3) as u8, (i * 3) as u8, 255])
            .collect();
        let pal2 = [[0, 0, 0, 255], [255, 255, 255, 255]];
        let whole = map_rgba8(&px, 16, 4, (0, 0), &pal2, Dither::Ordered, &NoMonitor).unwrap();
        let part: Vec<u8> = (0..4)
            .flat_map(|y| px[(y * 16 + 5) * 4..(y * 16 + 16) * 4].to_vec())
            .collect();
        let shifted = map_rgba8(&part, 11, 4, (5, 0), &pal2, Dither::Ordered, &NoMonitor).unwrap();
        for y in 0..4 {
            assert_eq!(
                &shifted[y * 11..(y + 1) * 11],
                &whole[y * 16 + 5..(y + 1) * 16]
            );
        }
    }

    /// 帯の分け方・スレッド数によらない（組織的は位置だけで決まる）。
    #[test]
    fn ordered_is_positional() {
        let pal: Vec<[u8; 4]> = (0..8u8)
            .map(|i| [i * 36, 255 - i * 36, i * 20, 255])
            .collect();
        let (w, h) = (37u32, 150u32);
        let px: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let a = map_rgba8(&px, w, h, (0, 0), &pal, Dither::Ordered, &NoMonitor).unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let b = pool
            .install(|| map_rgba8(&px, w, h, (0, 0), &pal, Dither::Ordered, &NoMonitor).unwrap());
        assert_eq!(a, b);
    }
}
