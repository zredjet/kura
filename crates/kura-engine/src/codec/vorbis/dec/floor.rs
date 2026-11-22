//! floor の復号（floor 0 と floor 1）。
//!
//! | 関数 | C |
//! |---|---|
//! | [`Floor0Look::new`] | floor0.c:150–163 `floor0_look` |
//! | `Floor0Look::map_lazy_init` | floor0.c:107–148 `floor0_map_lazy_init` |
//! | [`floor0_inverse1`] | floor0.c:165–199 |
//! | [`Floor0Look::inverse2`] | floor0.c:201–222 |
//! | `lsp_to_curve` | lsp.c:248–282 `vorbis_lsp_to_curve`（`FLOAT_LOOKUP`・`INT_LOOKUP` を定義しない素朴な経路。lsp.c:56–57） |
//! | [`floor1_inverse1`] | floor1.c:976–1060 |
//! | [`floor1_inverse2`] | floor1.c:1062–1102 |
//! | `render_point` | floor1.c:260–274 |
//! | `render_line` | floor1.c:368–395（SSE でない経路） |
//!
//! floor 1 の look は書き出しの移植の [`crate::codec::vorbis::floor1::floor1_look`] を使う（同じ C の関数）。
//!
//! 超越関数（floor 0 の `cos`・`exp`・`atan`）は `crate::math`（規則 6。floor 0 は値を作るたびに呼ぶが、どれも libm で
//! どの環境でも同じ値）。`toBARK`・`fromdB` は scales.h:67・78 のマクロの式の型のまま（`float` の引数を `double` の関数へ）。

use vorbis_tables::FLOOR1_FROMDB_LOOKUP;

use super::bitwise::OggpackReader;
use super::codebook::{DecodeBook, ov_ilog};
use super::info::VorbisInfoFloor0;
use crate::codec::vorbis::floor1::VorbisLookFloor1;
use crate::error::EngineError;
use crate::math;

/// `toBARK(n)`（scales.h:78）の、`n` が `float` の式：`13.1f*atan(.00074f*(n))+2.24f*atan((n)*(n)*1.85e-8f)+1e-4f*(n)`。
fn to_bark(n: f32) -> f64 {
    let t1 = f64::from(13.1f32) * math::atan(f64::from(0.000_74_f32 * n));
    let t2 = f64::from(2.24f32) * math::atan(f64::from(n * n * 1.85e-8_f32));
    t1 + t2 + f64::from(1e-4_f32 * n)
}

/// `vorbis_look_floor0`（floor0.c:32–42）。
#[derive(Debug, Clone)]
pub(crate) struct Floor0Look {
    ln: i32,
    m: i32,
    linearmap: [Option<Vec<i32>>; 2],
    n: [i32; 2],
    vi: VorbisInfoFloor0,
}

impl Floor0Look {
    /// `floor0_look`（floor0.c:150–163）。
    pub fn new(info: &VorbisInfoFloor0) -> Self {
        Self {
            ln: info.barkmap as i32,
            m: info.order,
            linearmap: [None, None],
            n: [0, 0],
            vi: *info,
        }
    }

    /// `floor0_map_lazy_init`（floor0.c:107–148）。
    fn map_lazy_init(&mut self, w: usize, blocksize: i64) {
        if self.linearmap[w].is_some() {
            return;
        }
        let n = (blocksize / 2) as i32;
        // C：float scale=look->ln/toBARK(info->rate/2.f);（int / double を float に）
        let scale = (f64::from(self.ln) / to_bark(self.vi.rate as f32 / 2.0)) as f32;
        let mut map = Vec::with_capacity(n as usize + 1);
        for j in 0..n {
            // C：int val=floor( toBARK((info->rate/2.f)/n*j) *scale);
            let arg = self.vi.rate as f32 / 2.0 / n as f32 * j as f32;
            let mut val = (to_bark(arg) * f64::from(scale)).floor() as i32;
            if val >= self.ln {
                val = self.ln - 1;
            }
            map.push(val);
        }
        map.push(-1);
        self.linearmap[w] = Some(map);
        self.n[w] = n;
    }

    /// `floor0_inverse2`（floor0.c:201–222）。`memo` は inverse1 の結果（lsp と最後の振幅）。
    pub fn inverse2(
        &mut self,
        w: usize,
        blocksize: i64,
        memo: Option<&mut [f32]>,
        out: &mut [f32],
    ) -> bool {
        self.map_lazy_init(w, blocksize);
        let n = self.n[w] as usize;
        match (memo, &self.linearmap[w]) {
            (Some(lsp), Some(map)) => {
                let amp = lsp[self.m as usize];
                lsp_to_curve(out, map, n, self.ln, lsp, self.m, amp, self.vi.ampdb as f32);
                true
            }
            _ => {
                out[..n].fill(0.0);
                false
            }
        }
    }
}

/// `vorbis_lsp_to_curve`（lsp.c:248–282）。`lsp` を lsp の cos に書き換える（C と同じ副作用）。
fn lsp_to_curve(
    curve: &mut [f32],
    map: &[i32],
    n: usize,
    ln: i32,
    lsp: &mut [f32],
    m: i32,
    amp: f32,
    ampoffset: f32,
) {
    // C：float wdel=M_PI/ln;
    let wdel = (std::f64::consts::PI / f64::from(ln)) as f32;
    let m = m as usize;
    for l in lsp.iter_mut().take(m) {
        *l = (f64::from(2.0f32) * math::cos(f64::from(*l))) as f32;
    }
    let mut i = 0usize;
    while i < n {
        let k = map[i];
        let mut p = 0.5f32;
        let mut q = 0.5f32;
        let w = (f64::from(2.0f32) * math::cos(f64::from(wdel * k as f32))) as f32;
        let mut j = 1usize;
        while j < m {
            q *= w - lsp[j - 1];
            p *= w - lsp[j];
            j += 2;
        }
        if j == m {
            // odd order filter; slightly assymetric
            q *= w - lsp[j - 1];
            // C：p*=p*(4.f-w*w);（`p = p * (p * …)` の順）
            p = p * (p * (4.0f32 - w * w));
            q *= q;
        } else {
            // even order filter; still symmetric
            // C：p*=p*(2.f-w); q*=q*(2.f+w);
            p = p * (p * (2.0f32 - w));
            q = q * (q * (2.0f32 + w));
        }
        // C：q=fromdB(amp/sqrt(p+q)-ampoffset);  fromdB(x) = exp((x)*.11512925f)
        let x = f64::from(amp) / f64::from(p + q).sqrt() - f64::from(ampoffset);
        let q = math::exp(x * f64::from(0.115_129_25_f32)) as f32;
        curve[i] *= q;
        i += 1;
        while map[i] == k {
            curve[i] *= q;
            i += 1;
        }
    }
}

/// `floor0_inverse1`（floor0.c:165–199）。lsp（`m` 個）と振幅（`m` 番目）。なければ None。
pub(crate) fn floor0_inverse1(
    look: &Floor0Look,
    books: &[DecodeBook],
    opb: &mut OggpackReader<'_>,
) -> Result<Option<Vec<f32>>, EngineError> {
    let info = &look.vi;
    let ampraw = opb.read(info.ampbits) as i32;
    // also handles the -1 out of data case
    if ampraw > 0 {
        // C：long maxval=(1<<info->ampbits)-1;（int のずらしと引き算。aarch64 で 1<<31 は INT_MIN、-fwrapv で引いて INT_MAX）
        let maxval = i64::from(1i32.wrapping_shl(info.ampbits as u32).wrapping_sub(1));
        // C：float amp=(float)ampraw/maxval*info->ampdB;
        let amp = ampraw as f32 / maxval as f32 * info.ampdb as f32;
        let booknum = opb.read(ov_ilog(info.numbooks as u32)) as i32;
        // be paranoid
        if booknum != -1 && booknum < info.numbooks {
            let b = &books[info.books[booknum as usize] as usize];
            let m = look.m as usize;
            // the additional b->dim is a guard against any possible stack smash
            let mut lsp = vec![0f32; m + b.dim as usize + 1];
            if b.decodev_set(&mut lsp, opb, look.m)? == -1 {
                return Ok(None);
            }
            let mut last = 0f32;
            let mut j = 0usize;
            while j < m {
                let mut k = 0usize;
                while j < m && k < b.dim as usize {
                    lsp[j] += last;
                    k += 1;
                    j += 1;
                }
                last = lsp[j - 1];
            }
            lsp[m] = amp;
            return Ok(Some(lsp));
        }
    }
    Ok(None)
}

/// `render_point`（floor1.c:260–274）。
fn render_point(x0: i32, x1: i32, y0: i32, y1: i32, x: i32) -> i32 {
    let y0 = y0 & 0x7fff;
    let y1 = y1 & 0x7fff;
    let dy = y1 - y0;
    let adx = x1 - x0;
    let ady = dy.abs();
    let err = ady * (x - x0);
    let off = err / adx;
    if dy < 0 { y0 - off } else { y0 + off }
}

/// `render_line`（floor1.c:368–395）。`x0..min(n, x1)` を順に（最初の点のあと、点ごとに `err`・`y` を進めて）掛ける。
fn render_line(n: i32, x0: i32, x1: i32, y0: i32, y1: i32, d: &mut [f32]) {
    let dy = y1 - y0;
    let adx = x1 - x0;
    let mut ady = dy.abs();
    let base = dy / adx;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let mut y = y0;
    let mut err = 0;
    ady -= (base * adx).abs();
    let n = if n > x1 { x1 } else { n };
    if x0 >= n {
        return;
    }
    let Some((first, rest)) = d[x0 as usize..n as usize].split_first_mut() else {
        return;
    };
    *first *= FLOOR1_FROMDB_LOOKUP[y as usize];
    for v in rest {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        *v *= FLOOR1_FROMDB_LOOKUP[y as usize];
    }
}

/// `floor1_inverse1`（floor1.c:976–1060）。ポストの値（`posts` 個）。なければ None。
pub(crate) fn floor1_inverse1(
    look: &VorbisLookFloor1,
    books: &[DecodeBook],
    opb: &mut OggpackReader<'_>,
) -> Option<Vec<i32>> {
    let info = &look.vi;
    // unpack wrapped/predicted values from stream
    if opb.read(1) != 1 {
        return None;
    }
    let posts = look.posts as usize;
    let mut fit_value = vec![0i32; posts];
    let bits = ov_ilog((look.quant_q - 1) as u32);
    fit_value[0] = opb.read(bits) as i32;
    fit_value[1] = opb.read(bits) as i32;
    // partition by partition
    let mut j = 2usize;
    for i in 0..info.partitions as usize {
        let class = info.partitionclass[i] as usize;
        let cdim = info.class_dim[class] as usize;
        let csubbits = info.class_subs[class];
        let csub = 1i32 << csubbits;
        let mut cval = 0i32;
        // decode the partition's first stage cascade value
        if csubbits != 0 {
            cval = books[info.class_book[class] as usize].decode(opb) as i32;
            if cval == -1 {
                return None;
            }
        }
        for k in 0..cdim {
            let book = info.class_subbook[class][(cval & (csub - 1)) as usize];
            cval >>= csubbits;
            if book >= 0 {
                let v = books[book as usize].decode(opb) as i32;
                fit_value[j + k] = v;
                if v == -1 {
                    return None;
                }
            } else {
                fit_value[j + k] = 0;
            }
        }
        j += cdim;
    }
    // unwrap positive values and reconsitute via linear interpolation
    for i in 2..posts {
        let lo = look.loneighbor[i - 2] as usize;
        let hi = look.hineighbor[i - 2] as usize;
        let predicted = render_point(
            info.postlist[lo],
            info.postlist[hi],
            fit_value[lo],
            fit_value[hi],
            info.postlist[i],
        );
        let hiroom = look.quant_q - predicted;
        let loroom = predicted;
        let room = (if hiroom < loroom { hiroom } else { loroom }) << 1;
        let mut val = fit_value[i];
        if val != 0 {
            if val >= room {
                if hiroom > loroom {
                    val -= loroom;
                } else {
                    val = -1 - (val - hiroom);
                }
            } else if val & 1 != 0 {
                val = -((val + 1) >> 1);
            } else {
                val >>= 1;
            }
            fit_value[i] = (val + predicted) & 0x7fff;
            fit_value[lo] &= 0x7fff;
            fit_value[hi] &= 0x7fff;
        } else {
            fit_value[i] = predicted | 0x8000;
        }
    }
    Some(fit_value)
}

/// `floor1_inverse2`（floor1.c:1062–1102）。`n` は窓の長さの半分。
pub(crate) fn floor1_inverse2(
    look: &VorbisLookFloor1,
    n: i32,
    memo: Option<&[i32]>,
    out: &mut [f32],
) -> bool {
    let info = &look.vi;
    let Some(fit_value) = memo else {
        out[..n as usize].fill(0.0);
        return false;
    };
    // render the lines
    let mut hx = 0;
    let mut lx = 0;
    let ly = fit_value[0].wrapping_mul(info.mult);
    // guard lookup against out-of-range values
    let mut ly = ly.clamp(0, 255);
    for j in 1..look.posts as usize {
        let current = look.forward_index[j] as usize;
        let hy = fit_value[current] & 0x7fff;
        if hy == fit_value[current] {
            hx = info.postlist[current];
            let hy = hy.wrapping_mul(info.mult).clamp(0, 255);
            render_line(n, lx, hx, ly, hy, out);
            lx = hx;
            ly = hy;
        }
    }
    // be certain
    for v in out.iter_mut().take(n as usize).skip(hx.max(0) as usize) {
        *v *= FLOOR1_FROMDB_LOOKUP[ly as usize];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_point_and_line_follow_the_c() {
        assert_eq!(render_point(0, 128, 10, 20, 64), 15);
        assert_eq!(render_point(0, 128, 20, 10 | 0x8000, 64), 15);
        let mut d = vec![1.0f32; 16];
        render_line(16, 0, 8, 0, 255, &mut d);
        assert_eq!(d[0], FLOOR1_FROMDB_LOOKUP[0]);
        assert!(d[8..].iter().all(|v| *v == 1.0));
    }
}
