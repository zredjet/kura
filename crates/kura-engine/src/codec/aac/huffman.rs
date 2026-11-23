//! ハフマンの符号表（`aac_tables` の符号と長さから、10 ビットの根と下の表の 2 段の引き表を作る）。

use std::sync::OnceLock;

use super::bits::Bits;
use super::decode_err;
use crate::error::EngineError;

/// 根の表のビット。
const ROOT: u32 = 10;

/// 2 段の引き表。項目は、葉なら `記号 << 8 | 添え << 5 | 長さ`（添えは記号ごとの 0〜7 の値。スペクトルの表では符号の
/// ビットの数）、下の表なら `1 << 31 | 下の表の頭 << 5 | 下の表のビット`。
pub(crate) struct Vlc {
    table: Vec<u32>,
}

impl Vlc {
    /// 符号と長さ（と記号ごとの添え。なければ 0）から作る。
    fn build(codes: &[u32], lens: &[u8], extra: Option<&[u8]>) -> Self {
        let leaf = |sym: usize, len: u32| {
            let x = extra
                .and_then(|e| e.get(sym))
                .map_or(0, |x| u32::from(*x) & 7);
            ((sym as u32) << 8) | (x << 5) | len
        };
        let mut table = vec![0u32; 1 << ROOT];
        // 根に収まる符号
        for (sym, (&code, &len)) in codes.iter().zip(lens).enumerate() {
            let len = u32::from(len);
            if len <= ROOT {
                let base = (code << (ROOT - len)) as usize;
                for e in &mut table[base..base + (1 << (ROOT - len))] {
                    *e = leaf(sym, len);
                }
            }
        }
        // 長い符号：根の頭ごとに下の表
        let mut prefixes: Vec<u32> = codes
            .iter()
            .zip(lens)
            .filter(|(_, l)| u32::from(**l) > ROOT)
            .map(|(&c, &l)| c >> (u32::from(l) - ROOT))
            .collect();
        prefixes.sort();
        prefixes.dedup();
        for p in prefixes {
            let members: Vec<(usize, u32, u32)> = codes
                .iter()
                .zip(lens)
                .enumerate()
                .filter(|(_, (c, l))| {
                    u32::from(**l) > ROOT && (**c >> (u32::from(**l) - ROOT)) == p
                })
                .map(|(s, (c, l))| (s, *c, u32::from(*l)))
                .collect();
            let sub = members.iter().map(|m| m.2 - ROOT).max().unwrap_or(0);
            let start = table.len();
            table.resize(start + (1 << sub), 0);
            for (sym, code, len) in members {
                let rest_len = len - ROOT;
                let rest = code & ((1 << rest_len) - 1);
                let base = start + (rest << (sub - rest_len)) as usize;
                for e in &mut table[base..base + (1 << (sub - rest_len))] {
                    *e = leaf(sym, rest_len);
                }
            }
            table[p as usize] = (1 << 31) | ((start as u32) << 5) | sub;
        }
        Self { table }
    }

    /// 64 ビットの窓（上位に詰めたもの）の頭の符号語の（記号, 長さ, 添え）。正しい符号語でなければ None。
    #[inline]
    pub fn lookup(&self, w: u64) -> Option<(u32, u32, u32)> {
        let e = self.table[(w >> (64 - ROOT)) as usize];
        if e & (1 << 31) == 0 {
            let len = e & 31;
            return (len != 0).then_some((e >> 8, len, (e >> 5) & 7));
        }
        let sub = e & 31;
        let start = ((e & !(1 << 31)) >> 5) as usize;
        let rest = ((w << ROOT) >> (64 - sub)) as usize;
        let e2 = self.table[start + rest];
        let len = e2 & 31;
        (len != 0).then_some((e2 >> 8, ROOT + len, (e2 >> 5) & 7))
    }

    /// 1 つの記号を読む。
    #[inline]
    pub fn decode(&self, bits: &mut Bits<'_>) -> Result<u32, EngineError> {
        let (sym, len, _) = self
            .lookup(bits.window())
            .ok_or_else(|| decode_err("ハフマンの符号が正しくありません"))?;
        bits.skip(len as usize)?;
        Ok(sym)
    }
}

/// スケールファクターの表と、スペクトルの表 1〜11。
pub(crate) struct Books {
    pub scf: Vlc,
    pub spectral: [Spectral; 11],
}

/// スペクトルの表（ISO/IEC 14496-3 4.6.3.3）：符号語の引き表と、記号と符号のビットの組からの値。
pub(crate) struct Spectral {
    /// 符号語の引き表。添えは記号ごとの符号のビットの数（符号なしの表 3・4・7〜11 の、0 でない値の数。符号つきの表は 0）。
    pub vlc: Vlc,
    /// 値の数（表 1〜4 は 4、5〜11 は 2）。`shift` も同じ数（記号ごとの符号のビットの組は 2^dim 通りまで）。
    pub dim: usize,
    shift: u32,
    /// `記号 << shift | 符号のビット`（0 でない値の順に上のビットから。1 なら負）→ 値。
    values: Vec<[i8; 4]>,
}

impl Spectral {
    fn build(cb: usize) -> Self {
        let (codes, lens) = aac_tables::SPECTRUM_CODEBOOKS[cb - 1];
        let dim: usize = if cb <= 4 { 4 } else { 2 };
        // 符号のビットは値の数まで
        let shift = dim as u32;
        let unsigned = !matches!(cb, 1 | 2 | 5 | 6);
        let mut signs = Vec::with_capacity(codes.len());
        let mut values = vec![[0i8; 4]; codes.len() << shift];
        for sym in 0..codes.len() {
            let base = values_of(cb, sym as i32);
            let nz = if unsigned {
                base[..dim].iter().filter(|v| **v != 0).count() as u32
            } else {
                0
            };
            signs.push(nz as u8);
            for bitsv in 0..1u32 << nz {
                let mut v = base;
                for (j, x) in v[..dim]
                    .iter_mut()
                    .filter(|x| **x != 0 && unsigned)
                    .enumerate()
                {
                    if (bitsv >> (nz - 1 - j as u32)) & 1 == 1 {
                        *x = -*x;
                    }
                }
                values[(sym << shift) | bitsv as usize] = v;
            }
        }
        Self {
            vlc: Vlc::build(codes, lens, Some(&signs)),
            dim,
            shift,
            values,
        }
    }

    /// 記号 `sym` と符号のビット `signs` の値。
    #[inline]
    pub fn values(&self, sym: usize, signs: usize) -> Option<&[i8; 4]> {
        self.values.get((sym << self.shift) | signs)
    }
}

/// 表 `cb`（1〜11）の記号 `idx` の値（符号なしの表は大きさ。ISO/IEC 14496-3 4.6.3.3）。
fn values_of(cb: usize, idx: i32) -> [i8; 4] {
    let v = match cb {
        1 | 2 => [
            idx / 27 - 1,
            (idx / 9) % 3 - 1,
            (idx / 3) % 3 - 1,
            idx % 3 - 1,
        ],
        3 | 4 => [idx / 27, (idx / 9) % 3, (idx / 3) % 3, idx % 3],
        5 | 6 => [idx / 9 - 4, idx % 9 - 4, 0, 0],
        7 | 8 => [idx / 8, idx % 8, 0, 0],
        9 | 10 => [idx / 13, idx % 13, 0, 0],
        _ => [idx / 17, idx % 17, 0, 0],
    };
    v.map(|x| x as i8)
}

/// 引き表（1 度だけ作る）。
pub(crate) fn books() -> &'static Books {
    static BOOKS: OnceLock<Books> = OnceLock::new();
    BOOKS.get_or_init(|| Books {
        scf: Vlc::build(
            &aac_tables::SCF_CODEBOOK_CODES,
            &aac_tables::SCF_CODEBOOK_LENS,
            None,
        ),
        spectral: std::array::from_fn(|i| Spectral::build(i + 1)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MSB から書く。
    fn write(codes: &[(u32, u8)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut at = 0usize;
        for &(c, l) in codes {
            for k in (0..l).rev() {
                if at.is_multiple_of(8) {
                    out.push(0);
                }
                if (c >> k) & 1 == 1 {
                    let n = out.len() - 1;
                    out[n] |= 0x80 >> (at % 8);
                }
                at += 1;
            }
        }
        out
    }

    /// どの表のどの記号も、書いた符号から読み戻せる。
    #[test]
    fn every_symbol_round_trips() {
        let b = books();
        let mut all = vec![(
            &b.scf,
            &aac_tables::SCF_CODEBOOK_CODES[..],
            &aac_tables::SCF_CODEBOOK_LENS[..],
        )];
        for (i, v) in b.spectral.iter().map(|s| &s.vlc).enumerate() {
            let (c, l) = aac_tables::SPECTRUM_CODEBOOKS[i];
            all.push((v, c, l));
        }
        for (vlc, codes, lens) in all {
            let seq: Vec<(u32, u8)> = codes.iter().copied().zip(lens.iter().copied()).collect();
            let bytes = write(&seq);
            let mut r = Bits::new(&bytes);
            for sym in 0..codes.len() {
                assert_eq!(vlc.decode(&mut r).unwrap(), sym as u32);
            }
        }
    }
}
