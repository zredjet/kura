//! 画素の集合（ビットの表）。行ごとに 64 ビットの語にそろえて持つ（塗りつぶしの区間の計算を語で行うため）。

use crate::error::EngineError;
use crate::geom::IRect;

/// 矩形の中の画素の集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mask {
    pub rect: IRect,
    /// 1 行の語の数。
    wpr: usize,
    bits: Vec<u64>,
}

/// `w`×`h` の集合が持つバイト数（行ごとに 64 ビットの語にそろえる。あふれは `u64::MAX` に飽和する）。作業領域の見積もりに使う。
pub fn mask_bytes(w: u64, h: u64) -> u64 {
    w.div_ceil(64).saturating_mul(8).saturating_mul(h)
}

impl Mask {
    /// 空の集合。`rect` は空でないこと。
    pub fn new(rect: IRect) -> Result<Self, EngineError> {
        let wpr = (rect.w as usize).div_ceil(64);
        let n = wpr
            .checked_mul(rect.h as usize)
            .ok_or_else(|| EngineError::Limit("描く範囲が大きすぎます".into()))?;
        let mut bits = Vec::new();
        bits.try_reserve_exact(n)
            .map_err(|_| EngineError::Limit("描く範囲を確保できません".into()))?;
        bits.resize(n, 0);
        Ok(Self { rect, wpr, bits })
    }

    /// 1 行の語の数。
    pub fn words_per_row(&self) -> usize {
        self.wpr
    }

    /// 行（矩形の中の行の番号）の語。幅より後ろのビットは 0。
    pub fn row(&self, r: usize) -> &[u64] {
        &self.bits[r * self.wpr..(r + 1) * self.wpr]
    }

    /// 行の語を書き換える（幅より後ろのビットは 0 のままにすること）。
    pub fn row_mut(&mut self, r: usize) -> &mut [u64] {
        &mut self.bits[r * self.wpr..(r + 1) * self.wpr]
    }

    fn local(&self, x: i64, y: i64) -> Option<(usize, usize)> {
        let (dx, dy) = (x.checked_sub(self.rect.x)?, y.checked_sub(self.rect.y)?);
        (dx >= 0 && dy >= 0 && dx < i64::from(self.rect.w) && dy < i64::from(self.rect.h))
            .then_some((dx as usize, dy as usize))
    }

    /// 画素を足す（範囲の外は捨てる）。
    pub fn set(&mut self, x: i64, y: i64) {
        if let Some((dx, dy)) = self.local(x, y) {
            self.bits[dy * self.wpr + dx / 64] |= 1 << (dx % 64);
        }
    }

    /// 画素が入っているか。
    pub fn get(&self, x: i64, y: i64) -> bool {
        self.local(x, y)
            .is_some_and(|(dx, dy)| self.bits[dy * self.wpr + dx / 64] & (1 << (dx % 64)) != 0)
    }

    /// 入っている画素を行の順に `f` に渡す。
    pub fn for_each(
        &self,
        mut f: impl FnMut(i64, i64) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        for r in 0..self.rect.h as usize {
            for (k, word) in self.row(r).iter().enumerate() {
                let mut bits = *word;
                while bits != 0 {
                    let b = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    f(self.rect.x + (k * 64 + b) as i64, self.rect.y + r as i64)?;
                }
            }
        }
        Ok(())
    }

    /// 入っている画素の数。
    pub fn count(&self) -> u64 {
        self.bits.iter().map(|w| u64::from(w.count_ones())).sum()
    }

    /// 矩形（絶対の座標）の中に入っている画素があるか。
    pub fn any_in(&self, area: IRect) -> bool {
        let Some((x0, y0, x1, y1)) = self.clip(area) else {
            return false;
        };
        (y0..y1).any(|r| {
            let row = self.row(r);
            (x0 / 64..x1.div_ceil(64)).any(|k| row[k] & span_bits(k, x0, x1) != 0)
        })
    }

    /// 行の中の [x0, x1)（絶対の座標）で入っている画素を、左から `f` に渡す。
    pub fn for_each_in_row(
        &self,
        y: i64,
        x0: i64,
        x1: i64,
        mut f: impl FnMut(i64) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        let Some((lx0, ly, lx1, _)) = self.clip(IRect::new(x0, y, (x1 - x0).max(0) as u32, 1))
        else {
            return Ok(());
        };
        let row = self.row(ly);
        for (k, word) in row.iter().enumerate().take(lx1.div_ceil(64)).skip(lx0 / 64) {
            let mut bits = word & span_bits(k, lx0, lx1);
            while bits != 0 {
                let b = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                f(self.rect.x + (k * 64 + b) as i64)?;
            }
        }
        Ok(())
    }

    /// 絶対の座標の矩形を、この集合の矩形の中の (x0, y0, x1, y1) にする（重ならなければ None）。
    fn clip(&self, area: IRect) -> Option<(usize, usize, usize, usize)> {
        let x0 = area.x.max(self.rect.x) - self.rect.x;
        let y0 = area.y.max(self.rect.y) - self.rect.y;
        let x1 = area.right().min(self.rect.right()) - self.rect.x;
        let y1 = area.bottom().min(self.rect.bottom()) - self.rect.y;
        (x0 < x1 && y0 < y1).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
    }
}

/// 語 k のうち、[x0, x1) に入るビット。
pub fn span_bits(k: usize, x0: usize, x1: usize) -> u64 {
    let lo = k * 64;
    let from = x0.saturating_sub(lo).min(64);
    let to = x1.saturating_sub(lo).min(64);
    if from >= to {
        return 0;
    }
    let upper = if to == 64 { u64::MAX } else { (1u64 << to) - 1 };
    upper & !((1u64 << from) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_word_aligned_and_ranges_work() {
        let mut m = Mask::new(IRect::new(-3, 2, 130, 3)).unwrap();
        assert_eq!(m.words_per_row(), 3);
        for x in [-3, 60, 61, 126] {
            m.set(x, 3);
        }
        m.set(200, 3);
        m.set(0, 1);
        assert_eq!(m.count(), 4);
        assert!(m.get(126, 3) && !m.get(127, 3));
        assert!(m.any_in(IRect::new(55, 0, 7, 10)));
        assert!(!m.any_in(IRect::new(62, 0, 60, 10)));
        let mut seen = Vec::new();
        m.for_each_in_row(3, 0, 127, |x| {
            seen.push(x);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [60, 61, 126]);
        assert_eq!(span_bits(1, 70, 72), 0b1100_0000);
        assert_eq!(span_bits(0, 0, 64), u64::MAX);
        assert_eq!(span_bits(2, 0, 100), 0);
    }
}
