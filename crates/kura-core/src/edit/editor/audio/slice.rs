//! 裏の仕事の区切り（M13 段 7。docs/editing.md §12.12）：1 回の区切り（`WORK_SLICE`）の締め切りまでに流すチャンクの数を、
//! 1 チャンクを流すのにかかる時間の見積もり（[`Pace`]）から窓ごとに決める。
//!
//! - 窓（波形の段は流れの窓 `STREAM_CHUNKS`、計りは `MEASURE_WINDOW_CHUNKS`）は小さくしない。締め切りまで十分なら窓を丸ごと、
//!   近づいたら窓の前から収まるチャンクの数だけを流し、締め切りを過ぎたら止める。続き（窓の残り）は次の区切りで流す。
//!   流す量は窓を小さくするより保てる（窓ごとの見積もり・確保・プールに入る手間が増えない）。
//! - 見積もりは窓ごとのかかった時間 ÷ チャンクの数の、半分ずつの移動平均（窓の前の確保と呼び出しの手間も含むので、チャンクの
//!   少ない窓ほど大きめに出て、締め切りの前では控えめになる）。見積もりがなければ 1 チャンクで測る。見積もりが合っていれば、
//!   区切りは締め切り ＋ 1 チャンクまでで終わる（[`Pace::fit`] の約束）。結果（段・ラウドネス）は切り方によらず同じ。

use std::time::{Duration, Instant};

/// 1 チャンクを流すのにかかる時間の見積もり（波形の段と計りで別に持つ。流す出力の重さが違う）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Pace {
    per_chunk: Option<Duration>,
}

impl Pace {
    /// 残り `left` で流すチャンクの数（`max` まで）。残りがない・`max` が 0 なら 0、見積もりがなければ 1（測る）、あれば
    /// ⌊`left` ÷ 1 チャンク⌋ を 1〜`max` に収めたもの。見積もりどおりなら、流し終わるのは締め切り ＋ 1 チャンクまで
    /// （(n − 1) × 1 チャンク ≤ `left`）。
    pub(crate) fn fit(&self, left: Duration, max: u64) -> u64 {
        if max == 0 || left.is_zero() {
            return 0;
        }
        let n = match self.per_chunk {
            None => 1,
            Some(p) if p.is_zero() => max,
            Some(p) => u64::try_from(left.as_nanos() / p.as_nanos()).unwrap_or(u64::MAX),
        };
        n.clamp(1, max)
    }

    /// `chunks` チャンクを `took` で流した（見積もりに足す）。
    pub(crate) fn note(&mut self, chunks: u64, took: Duration) {
        let Ok(n) = u32::try_from(chunks) else {
            return;
        };
        if n == 0 {
            return;
        }
        let p = took / n;
        self.per_chunk = Some(match self.per_chunk {
            None => p,
            Some(old) => (old + p) / 2,
        });
    }

    /// 今の見積もり（1 チャンク）。
    pub(crate) fn per_chunk(&self) -> Option<Duration> {
        self.per_chunk
    }
}

/// 1 回の区切りの上限（波形の段と計りで 1 つを分け合う：波形の段が先に使い、残りで計りを進める）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Limit {
    /// 締め切り（`WORK_SLICE` の終わり）まで。流す数は見積もりで決める。
    Deadline(Instant),
    /// テスト：この区切りで流してよいチャンクの数（時間によらずに、窓の途中で止める・切り方によらず同じを確かめる）。
    #[cfg(test)]
    Chunks(u64),
}

impl Limit {
    /// 次に流すチャンクの数（`max` まで。0 は止める）：締め切りなら残りの時間と `pace` の見積もりで、テストの数なら残りから
    /// 引く。
    pub(crate) fn grant(&mut self, pace: &Pace, max: u64) -> u64 {
        match self {
            Self::Deadline(deadline) => {
                pace.fit(deadline.saturating_duration_since(Instant::now()), max)
            }
            #[cfg(test)]
            Self::Chunks(left) => {
                let n = (*left).min(max);
                *left -= n;
                n
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(x: u64) -> Duration {
        Duration::from_millis(x)
    }

    /// 見積もりがなければ 1 チャンクで測り、あれば残りに収まる数（1〜max）。残りがなければ 0。どの残り・見積もりでも、
    /// 見積もりどおりの終わりは締め切り ＋ 1 チャンクまで。
    #[test]
    fn fit_ends_within_one_chunk_after_the_deadline() {
        let none = Pace::default();
        assert_eq!(none.fit(ms(40), 16), 1);
        assert_eq!(none.fit(Duration::ZERO, 16), 0);
        assert_eq!(none.fit(ms(40), 0), 0);
        let mut p = Pace::default();
        p.note(4, ms(10));
        assert_eq!(p.per_chunk(), Some(Duration::from_micros(2_500)));
        assert_eq!(p.fit(ms(40), 16), 16);
        assert_eq!(p.fit(ms(10), 16), 4);
        assert_eq!(
            p.fit(Duration::from_micros(2_400), 16),
            1,
            "残りが 1 チャンクに満たなくても 1"
        );
        assert_eq!(p.fit(ms(40), 3), 3);
        let mut r = kura_engine::determinism::rng(17);
        use rand_chacha::rand_core::Rng;
        for _ in 0..2_000 {
            let mut p = Pace::default();
            p.note(
                1 + u64::from(r.next_u32() % 16),
                Duration::from_micros(1 + u64::from(r.next_u32() % 50_000)),
            );
            let left = Duration::from_micros(u64::from(r.next_u32() % 60_000));
            let max = u64::from(r.next_u32() % 20);
            let n = p.fit(left, max);
            let each = p.per_chunk().unwrap();
            assert!(n <= max);
            if left.is_zero() || max == 0 {
                assert_eq!(n, 0);
                continue;
            }
            assert!(n >= 1);
            assert!(
                each * (n as u32 - 1) <= left,
                "{n} × {each:?} は {left:?} ＋ 1 チャンクを超える"
            );
        }
    }

    /// 見積もりは半分ずつの移動平均。0 チャンクは足さない。
    #[test]
    fn pace_is_a_moving_average_per_chunk() {
        let mut p = Pace::default();
        p.note(0, ms(5));
        assert_eq!(p.per_chunk(), None);
        p.note(2, ms(8));
        assert_eq!(p.per_chunk(), Some(ms(4)));
        p.note(1, ms(2));
        assert_eq!(p.per_chunk(), Some(ms(3)));
        let mut zero = Pace::default();
        zero.note(4, Duration::ZERO);
        assert_eq!(zero.fit(ms(1), 16), 16);
    }

    /// 締め切りを過ぎたら 0。テストの数は区切りの中で分け合う（尽きたら 0）。
    #[test]
    fn limits_stop_at_the_deadline_or_the_count() {
        let mut p = Pace::default();
        p.note(1, ms(1));
        assert_eq!(Limit::Deadline(Instant::now()).grant(&p, 16), 0);
        assert_eq!(
            Limit::Deadline(Instant::now() + ms(60_000)).grant(&p, 16),
            16
        );
        let mut l = Limit::Chunks(5);
        assert_eq!(l.grant(&p, 4), 4);
        assert_eq!(l.grant(&p, 4), 1);
        assert_eq!(l.grant(&p, 4), 0);
    }
}
