//! residue の復号（residue 0・1・2）。
//!
//! | 関数 | C |
//! |---|---|
//! | [`Res0Look::new`] | res0.c:255–313 `res0_look`（`decodemap` は表を作らず、使うときに同じ式で 1 桁ずつ求める） |
//! | `inverse_01` | res0.c:643–701 `_01inverse` |
//! | [`Res0Look::inverse`] | res0.c:703–713 `res0_inverse`・747–757 `res1_inverse`・802–859 `res2_inverse` |
//!
//! `decodemap[j][k]` は `j` を `parts` 進で表したときの上から `k` 桁目（res0.c:290–300 の `val/mult`）。表は
//! `partvals × dim` 個（最大で 2^24 個）になるので作らず、同じ整数の式を上の桁から順に進める（[`Digits`]。`k` は 0 から
//! 順に使うので、値は表を引くのと同じで、1 桁あたりの手間も同じ）。

use vorbis_tables::VorbisInfoResidue0;

use super::bad_header;
use super::bitwise::OggpackReader;
use super::codebook::{DecodeBook, ov_ilog};
use crate::error::EngineError;

/// `vorbis_look_residue0` の復号の欄。
#[derive(Debug, Clone)]
pub(crate) struct Res0Look {
    info: VorbisInfoResidue0,
    parts: i32,
    stages: i32,
    phrasebook: usize,
    /// [class][stage] の codebook の番号（本なしは None）。
    partbooks: Vec<Vec<Option<usize>>>,
    partvals: i64,
}

impl Res0Look {
    /// `res0_look`（res0.c:255–313）。
    pub fn new(info: &VorbisInfoResidue0, books: &[DecodeBook]) -> Result<Self, EngineError> {
        let phrasebook = info.groupbook as usize;
        let dim = books
            .get(phrasebook)
            .ok_or_else(|| bad_header("residue の codebook がありません"))?
            .dim;
        let parts = info.partitions;
        let mut acc = 0usize;
        let mut maxstage = 0;
        let mut partbooks = Vec::with_capacity(parts as usize);
        for j in 0..parts as usize {
            let stages = ov_ilog(info.secondstages[j] as u32);
            let mut row = Vec::new();
            if stages != 0 {
                if stages > maxstage {
                    maxstage = stages;
                }
                row = vec![None; stages as usize];
                for (k, slot) in row.iter_mut().enumerate() {
                    if info.secondstages[j] & (1 << k) != 0 {
                        *slot = Some(info.booklist[acc] as usize);
                        acc += 1;
                    }
                }
            }
            partbooks.push(row);
        }
        let mut partvals: i64 = 1;
        for _ in 0..dim {
            partvals = partvals.wrapping_mul(i64::from(parts));
        }
        Ok(Self {
            info: *info,
            parts,
            stages: maxstage,
            phrasebook,
            partbooks,
            partvals,
        })
    }

    /// `decodemap[j][0..]` を上の桁から順に出す読み手。
    fn digits(&self, j: i64) -> Digits {
        let parts = i64::from(self.parts);
        Digits {
            val: j,
            mult: self.partvals.checked_div(parts).unwrap_or(0),
            parts,
        }
    }

    fn inverse_01(
        &self,
        books: &[DecodeBook],
        opb: &mut OggpackReader<'_>,
        pcmend: i64,
        planes: &mut [&mut [f32]],
        res1: bool,
    ) -> Result<(), EngineError> {
        let info = &self.info;
        let ch = planes.len();
        // move all this setup out later
        let samples_per_partition = info.grouping;
        let partitions_per_word = books[self.phrasebook].dim as i32;
        let max = (pcmend >> 1) as i32;
        let end = if (info.end as i32) < max {
            info.end as i32
        } else {
            max
        };
        let n = end - info.begin as i32;
        if n <= 0 {
            return Ok(());
        }
        let partvals = n / samples_per_partition;
        let partwords = (partvals + partitions_per_word - 1) / partitions_per_word;
        let mut partword: Vec<Vec<i64>> = vec![vec![0; partwords.max(0) as usize]; ch];
        let mut digits = vec![Digits::default(); ch];
        for s in 0..self.stages {
            // each loop decodes on partition codeword containing partitions_per_word partitions
            let mut i = 0i32;
            let mut l = 0usize;
            while i < partvals {
                if s == 0 {
                    // fetch the partition word for each channel
                    for word in partword.iter_mut() {
                        let temp = books[self.phrasebook].decode(opb);
                        if temp == -1 || temp >= i64::from(info.partvals) {
                            return Ok(());
                        }
                        word[l] = temp;
                    }
                }
                // now we decode residual values for the partitions
                for (d, word) in digits.iter_mut().zip(&partword) {
                    *d = self.digits(word[l]);
                }
                let mut k = 0i32;
                while k < partitions_per_word && i < partvals {
                    for (j, plane) in planes.iter_mut().enumerate() {
                        let offset =
                            (info.begin + i64::from(i) * i64::from(samples_per_partition)) as usize;
                        let class = digits[j].next() as usize;
                        if info.secondstages[class] & (1 << s) != 0
                            && let Some(Some(b)) = self.partbooks[class].get(s as usize)
                        {
                            let book = &books[*b];
                            let a = &mut plane[offset..];
                            let r = if res1 {
                                book.decodev_add(a, opb, samples_per_partition)?
                            } else {
                                book.decodevs_add(a, opb, samples_per_partition)?
                            };
                            if r == -1 {
                                return Ok(());
                            }
                        }
                    }
                    k += 1;
                    i += 1;
                }
                l += 1;
            }
        }
        Ok(())
    }

    /// `res0_inverse`・`res1_inverse`・`res2_inverse`（res0.c:703–713・747–757・802–859）。`kind` は residue の種類。
    pub fn inverse(
        &self,
        kind: i32,
        books: &[DecodeBook],
        opb: &mut OggpackReader<'_>,
        pcmend: i64,
        planes: &mut [&mut [f32]],
        nonzero: &[bool],
    ) -> Result<(), EngineError> {
        if kind != 2 {
            let mut used: Vec<&mut [f32]> = planes
                .iter_mut()
                .zip(nonzero)
                .filter(|(_, nz)| **nz)
                .map(|(p, _)| &mut **p)
                .collect();
            if used.is_empty() {
                return Ok(());
            }
            return self.inverse_01(books, opb, pcmend, &mut used, kind == 1);
        }
        self.inverse_2(books, opb, pcmend, planes, nonzero)
    }

    /// `res2_inverse`（res0.c:802–859）。
    fn inverse_2(
        &self,
        books: &[DecodeBook],
        opb: &mut OggpackReader<'_>,
        pcmend: i64,
        planes: &mut [&mut [f32]],
        nonzero: &[bool],
    ) -> Result<(), EngineError> {
        let info = &self.info;
        let ch = planes.len() as i32;
        // move all this setup out later
        let samples_per_partition = info.grouping;
        let partitions_per_word = books[self.phrasebook].dim as i32;
        let max = ((pcmend * i64::from(ch)) >> 1) as i32;
        let end = if (info.end as i32) < max {
            info.end as i32
        } else {
            max
        };
        let n = end - info.begin as i32;
        if n <= 0 {
            return Ok(());
        }
        let partvals = n / samples_per_partition;
        let partwords = (partvals + partitions_per_word - 1) / partitions_per_word;
        let mut partword = vec![0i64; partwords.max(0) as usize];
        if !nonzero.iter().any(|nz| *nz) {
            // no nonzero vectors
            return Ok(());
        }
        for s in 0..self.stages {
            let mut i = 0i32;
            let mut l = 0usize;
            while i < partvals {
                if s == 0 {
                    // fetch the partition word
                    let temp = books[self.phrasebook].decode(opb);
                    if temp == -1 || temp >= i64::from(info.partvals) {
                        return Ok(());
                    }
                    partword[l] = temp;
                }
                // now we decode residual values for the partitions
                let mut digits = self.digits(partword[l]);
                let mut k = 0i32;
                while k < partitions_per_word && i < partvals {
                    let class = digits.next() as usize;
                    if info.secondstages[class] & (1 << s) != 0
                        && let Some(Some(b)) = self.partbooks[class].get(s as usize)
                    {
                        let offset = i64::from(i) * i64::from(samples_per_partition) + info.begin;
                        if books[*b].decodevv_add(planes, offset, ch, opb, samples_per_partition)?
                            == -1
                        {
                            return Ok(());
                        }
                    }
                    k += 1;
                    i += 1;
                }
                l += 1;
            }
        }
        Ok(())
    }
}

/// `decodemap[j][k]`（res0.c:290–300）を `k` = 0, 1, … の順に出す：`deco = val / mult; val -= deco * mult; mult /= parts`。
/// `mult` は `partvals / parts` から始まり、使う桁（phrasebook の dim まで）の間は 1 以上（0 なら 0 を返す）。
#[derive(Debug, Clone, Copy, Default)]
struct Digits {
    val: i64,
    mult: i64,
    parts: i64,
}

impl Digits {
    fn next(&mut self) -> i64 {
        let deco = self.val.checked_div(self.mult).unwrap_or(0);
        self.val -= deco * self.mult;
        self.mult = self.mult.checked_div(self.parts).unwrap_or(0);
        deco
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 桁ずつ進める読み手が、C の `decodemap` の表（res0.c:290–300 をそのまま組んだもの）と同じ桁を出す。
    #[test]
    fn digits_match_the_c_table() {
        for parts in 1i64..=6 {
            for dim in 1u32..=5 {
                let partvals = parts.pow(dim);
                for j in 0..partvals {
                    let mut val = j;
                    let mut mult = partvals / parts;
                    let mut d = Digits {
                        val: j,
                        mult,
                        parts,
                    };
                    for k in 0..dim {
                        let deco = val / mult;
                        val -= deco * mult;
                        mult /= parts;
                        assert_eq!(d.next(), deco, "{parts} {dim} {j} {k}");
                    }
                }
            }
        }
    }
}
