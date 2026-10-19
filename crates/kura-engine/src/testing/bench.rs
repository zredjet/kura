//! ベンチの部品（E30。docs/editing.md §16・requirements.md の NFR-11〜17）。`tests/bench_edit.rs` と
//! `kura-devtools bench edit` が同じものを使う。
//!
//! - 時計は呼び手が渡す（kura-engine は時刻を読めない。clippy.toml）。`Instant` からナノ秒の時計を作って [`measure`] に渡す。
//! - 統計は最近順位法（昇順に並べた ⌈p·n/100⌉ 番目）。n < 20 は p95 を出さず、最大で判定する（[`Stats::judged`]）。
//!   慣らしは [`WARMUP`] 回で、測った値に入れない。
//! - 入力は ChaCha8 と整数だけで作る（`InputGen::Photo`・`InputGen::PixelSprite`・[`walk_stroke`]）ので、機械によらず同じ。
//!   [`program_audio`] は `crate::math` の超越関数を使う（OS の libm に頼らないので、これも機械によらず同じ）。
//!
//! - 経路（[`path`]：エディタの経路と一括の経路）とケース（[`cases`]：NFR ごとの文書と入力）も同じものを使う。
//!
//! テスト用の補助だが本番のビルドにも入る（`pub mod testing`）ので、ここでも panic しない。

pub mod cases;
pub mod path;

use std::sync::Arc;

use rand_chacha::rand_core::Rng;

use crate::determinism;
use crate::error::EngineError;
use crate::geom::IPoint;
use crate::math;
use crate::paint::{Blend, Brush, BrushShape, PaintAction, PaintColor, PointList, Tool};
use crate::testgen::InputGen;
use crate::value::{Audio, AudioMeta, PixelFormat, Value};

/// 慣らしの回数（測った値に入れない）。
pub const WARMUP: usize = 10;

/// p95 を出す最小の回数（これより少なければ最大で判定する）。
pub const MIN_FOR_P95: usize = 20;

/// 写真のベンチの大きさ（24MP）。
pub const PHOTO_W: u32 = 6000;
/// 写真のベンチの大きさ（24MP）。
pub const PHOTO_H: u32 = 4000;

/// 最近順位法の百分位：昇順に並べた `sorted` の ⌈pct·n/100⌉ 番目（1 始まり。少なくとも 1 番目）。空なら `None`。
pub fn nearest_rank(sorted: &[u64], pct: u64) -> Option<u64> {
    let n = sorted.len() as u64;
    if n == 0 {
        return None;
    }
    let rank = pct.saturating_mul(n).div_ceil(100).clamp(1, n);
    sorted.get(rank as usize - 1).copied()
}

/// 測った値の統計（単位は時計の単位のまま。ナノ秒の時計ならナノ秒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stats {
    /// 測った回数（慣らしを除く）。
    pub n: usize,
    pub min: u64,
    /// 中央値（最近順位法の 50 番目の百分位）。
    pub p50: u64,
    /// 95 番目の百分位（最近順位法）。`n` が [`MIN_FOR_P95`] より少なければ `None`。
    pub p95: Option<u64>,
    pub max: u64,
    /// 平均（切り捨て）。
    pub mean: u64,
    /// 測った値（測った順）。
    pub samples: Vec<u64>,
}

impl Stats {
    /// 測った値から作る。空なら `None`。
    pub fn from_samples(samples: Vec<u64>) -> Option<Self> {
        let mut sorted = samples.clone();
        sorted.sort();
        let n = sorted.len();
        let p50 = nearest_rank(&sorted, 50)?;
        let p95 = if n >= MIN_FOR_P95 {
            nearest_rank(&sorted, 95)
        } else {
            None
        };
        let sum = sorted
            .iter()
            .fold(0u128, |acc, v| acc.saturating_add(u128::from(*v)));
        Some(Self {
            n,
            min: *sorted.first()?,
            p50,
            p95,
            max: *sorted.last()?,
            mean: u64::try_from(sum / n as u128).unwrap_or(u64::MAX),
            samples,
        })
    }

    /// 判定に使う値：p95（`n` が [`MIN_FOR_P95`] 以上）か、少なければ最大。
    pub fn judged(&self) -> u64 {
        self.p95.unwrap_or(self.max)
    }

    /// `limit` を超えた回数。
    pub fn over(&self, limit: u64) -> usize {
        self.samples.iter().filter(|v| **v > limit).count()
    }
}

/// `warmup` 回（測らない）のあと `n` 回 `f` を呼び、1 回ごとの時間を `clock`（単調に増える時刻）の差で測る。`f` には
/// 慣らしを含めた通しの回の番号（0 から）を渡す（スライダーの値を毎回変えるなど）。`f` の結果は時計を読んだあとに捨てる
/// （大きな値を落とす時間を入れない）。`f` のエラーはそのまま返す。`n` が 0 なら `Internal`。
pub fn measure<T>(
    clock: &dyn Fn() -> u64,
    warmup: usize,
    n: usize,
    mut f: impl FnMut(usize) -> Result<T, EngineError>,
) -> Result<Stats, EngineError> {
    if n == 0 {
        return Err(EngineError::Internal("測る回数が 0 です".into()));
    }
    for i in 0..warmup {
        drop(f(i)?);
    }
    let mut samples = Vec::with_capacity(n);
    for i in warmup..warmup + n {
        let start = clock();
        let out = f(i)?;
        let end = clock();
        drop(out);
        samples.push(end.saturating_sub(start));
    }
    Stats::from_samples(samples).ok_or_else(|| EngineError::Internal("測った値がありません".into()))
}

/// [`measure`] と同じだが、1 回ごとに測らない準備 `prepare` を先に呼ぶ（塗りつぶしを取り消して足し直すなど、測る区間に
/// 入れない手順）。準備と測る手順は同じ状態 `state`（エディタの経路など）を受ける。どちらにも通しの回の番号を渡す。
pub fn measure_prepared<C, T>(
    clock: &dyn Fn() -> u64,
    warmup: usize,
    n: usize,
    state: &mut C,
    mut prepare: impl FnMut(&mut C, usize) -> Result<(), EngineError>,
    mut f: impl FnMut(&mut C, usize) -> Result<T, EngineError>,
) -> Result<Stats, EngineError> {
    if n == 0 {
        return Err(EngineError::Internal("測る回数が 0 です".into()));
    }
    for i in 0..warmup {
        prepare(state, i)?;
        drop(f(state, i)?);
    }
    let mut samples = Vec::with_capacity(n);
    for i in warmup..warmup + n {
        prepare(state, i)?;
        let start = clock();
        let out = f(state, i)?;
        let end = clock();
        drop(out);
        samples.push(end.saturating_sub(start));
    }
    Stats::from_samples(samples).ok_or_else(|| EngineError::Internal("測った値がありません".into()))
}

/// 写真のプレビューを縮小の段の計画にできず、等倍（段 0）で描いたときに表に出す注（記録だけにする）。
pub const PHOTO_FALLBACK: &str = "縮小の段の計画にできず、等倍で描いた（記録だけ）";

/// NFR-12 の目標の範囲の筆の太さの上限（px。requirements.md の NFR-12・ADR-154）。これより太い筆は測っても判定しない
/// （範囲外の記録）。
pub const STROKE_SIZE_LIMIT: u8 = 32;

/// 範囲外の筆（[`STROKE_SIZE_LIMIT`] より太い）の注。
pub const OUT_OF_SCOPE_STROKE: &str = "範囲外（NFR-12 は太さ 32px まで。ADR-154）";

/// 範囲外の減色のプレビュー（NFR-11）の注。
pub const OUT_OF_SCOPE_REDUCE: &str = "範囲外（NFR-11 の対象外。減色のプレビューは古い結果を出したまま、描けたら差し替える。ADR-154）";

/// 筆の太さ `size`（px）が NFR-12 の目標の範囲か。
pub fn stroke_in_scope(size: u8) -> bool {
    size <= STROKE_SIZE_LIMIT
}

/// 何で判定するか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judge {
    /// p95（少なければ最大。[`Stats::judged`]）が上限以下。
    P95,
    /// 中央値が上限以下。
    Median,
    /// 通しの時間（何回か通したものの最大）が上限以下。
    Total,
    /// 経路・スレッドによらず値が一致する（時間では判定しない）。
    Match,
    /// ピークのメモリが予算以下で、超える入力は落ちずに Limit（時間では判定しない）。
    Budget,
}

/// 目標（NFR-11〜17。requirements.md と editing.md §16）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// NFR の番号（11〜17）。
    pub nfr: u8,
    /// ケースの名前（JSON のレコードの `case`）。
    pub case: &'static str,
    /// 何を測るか。
    pub item: &'static str,
    pub judge: Judge,
    /// 時間の上限（ナノ秒）。時間で判定しないものは `None`。
    pub limit_ns: Option<u64>,
}

const MS: u64 = 1_000_000;

/// NFR-11〜17 の目標の表。NFR-11 はエンジンの中（編集を受けてから、プレビューの値と変わったタイルの一覧ができるまで。
/// ダイジェスト・エンコード・IPC・WebKit を含めない）の 25 ms。全体の 50 ms は M12 で測る。
pub const TARGETS: [Target; 8] = [
    Target {
        nfr: 11,
        case: "preview",
        item: "引数を変えてからプレビューの値ができるまで（エンジンの中。24MP は縮小の段、2048² までのドット絵は等倍）",
        judge: Judge::P95,
        limit_ns: Some(25 * MS),
    },
    Target {
        nfr: 12,
        case: "stroke",
        item: "筆 1 本（500 点まで、太さ 32px まで、4096² まで）の確定",
        judge: Judge::P95,
        limit_ns: Some(8 * MS),
    },
    Target {
        nfr: 12,
        case: "fill",
        item: "全面の塗りつぶし（4096²）",
        judge: Judge::P95,
        limit_ns: Some(30 * MS),
    },
    Target {
        nfr: 13,
        case: "save",
        item: "保存：24MP の JPEG → 線形光で 50% → JPEG",
        judge: Judge::Median,
        limit_ns: Some(500 * MS),
    },
    Target {
        nfr: 14,
        case: "bulk",
        item: "一括：ドット絵の PNG 1,000 件（トリム・4 倍）",
        judge: Judge::Total,
        limit_ns: Some(5_000 * MS),
    },
    Target {
        nfr: 15,
        case: "analysis",
        item: "5 分の曲の解析（波形・ラウドネス・トゥルーピーク）",
        judge: Judge::Median,
        limit_ns: Some(2_000 * MS),
    },
    Target {
        nfr: 16,
        case: "determinism",
        item: "スレッド数・チャンクの切り方・経路（エディタ／一括）によらず同じ結果",
        judge: Judge::Match,
        limit_ns: None,
    },
    Target {
        nfr: 17,
        case: "memory",
        item: "編集のメモリが予算（既定は物理メモリの 1/4、上限 4 GiB）を超えない。超える入力は落ちずにエラー",
        judge: Judge::Budget,
        limit_ns: None,
    },
];

/// 目標の表から引く。
pub fn target(nfr: u8, case: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|t| t.nfr == nfr && t.case == case)
}

impl Target {
    /// 時間の判定：上限以下なら真。時間で判定しないもの（一致・予算）は `None`。`stats` の単位はナノ秒。
    pub fn verdict(&self, stats: &Stats) -> Option<bool> {
        let limit = self.limit_ns?;
        let v = match self.judge {
            Judge::P95 => stats.judged(),
            Judge::Median => stats.p50,
            Judge::Total => stats.max,
            Judge::Match | Judge::Budget => return None,
        };
        Some(v <= limit)
    }
}

/// 24MP の写真らしい合成（NFR-11・13 の入力）。
pub fn photo(seed: u64) -> InputGen {
    InputGen::Photo {
        w: PHOTO_W,
        h: PHOTO_H,
        seed,
    }
}

/// 一括のドット絵（NFR-14 の入力）：`count` 件のスプライト。大きさは 16〜256 px（縦横それぞれ）、透明な余白つき、Rgba8 と
/// Indexed8 を交互に（半分ずつ）。大きさと各件の種は `seed` の ChaCha8 で決める。
pub fn sprite_batch(count: usize, seed: u64) -> Vec<InputGen> {
    let mut r = determinism::rng_stream(seed, 0x5052);
    (0..count)
        .map(|i| {
            let w = 16 + r.next_u32() % 241;
            let h = 16 + r.next_u32() % 241;
            let item = u64::from(r.next_u32()) << 32 | u64::from(r.next_u32());
            InputGen::PixelSprite {
                w,
                h,
                fmt: if i % 2 == 0 {
                    PixelFormat::Rgba8
                } else {
                    PixelFormat::Indexed8
                },
                seed: item,
            }
        })
        .collect()
}

/// 筆の点の列（NFR-12）：`w × h` の画像の中で、種から決めた点から始めて、1 歩ごとに縦横それぞれ −`step`〜`step` 画素動く
/// ランダムウォーク（ChaCha8。画像の外に出たら縁で止める）。点は `points` 個（0 なら空）。
pub fn walk_stroke(seed: u64, points: usize, w: u32, h: u32, step: u32) -> Vec<IPoint> {
    let mut r = determinism::rng_stream(seed, 0x5354);
    let (w, h) = (i64::from(w.max(1)), i64::from(h.max(1)));
    let span = u64::from(step) * 2 + 1;
    let mut at = IPoint {
        x: i64::from(r.next_u32()) % w,
        y: i64::from(r.next_u32()) % h,
    };
    let mut out = Vec::with_capacity(points);
    for i in 0..points {
        if i > 0 {
            let dx = (u64::from(r.next_u32()) % span) as i64 - i64::from(step);
            let dy = (u64::from(r.next_u32()) % span) as i64 - i64::from(step);
            at = IPoint {
                x: (at.x + dx).clamp(0, w - 1),
                y: (at.y + dy).clamp(0, h - 1),
            };
        }
        out.push(at);
    }
    out
}

/// ペンの筆 1 本（RGBA の画像に重ねる。筆は円、大きさ `size` 画素）。
pub fn pen(points: Vec<IPoint>, size: u8, pixel_perfect: bool, color: [u8; 4]) -> PaintAction {
    PaintAction::Stroke {
        tool: Tool::Pen,
        color: Some(PaintColor::Rgba(color)),
        blend: Blend::Over,
        brush: Brush {
            shape: BrushShape::Circle,
            size,
        },
        pixel_perfect,
        frame: 0,
        points: PointList(points),
    }
}

/// 音楽らしいステレオの信号（NFR-15 の入力。段 8 の tests/bench_audio_analysis.rs から移したもの）：低域に寄せた雑音と
/// 220 Hz の正弦を混ぜ、1 秒ごとに大きさ（−40〜−6 dBFS）を変える。最初と最後の 1 秒は無音。
pub fn program_audio(rate: u32, secs: usize) -> Result<Value, EngineError> {
    let frames = (rate as usize)
        .checked_mul(secs)
        .ok_or_else(|| EngineError::Limit("音声が長すぎます".into()))?;
    let mut env = determinism::rng(1);
    let levels: Vec<f64> = (0..secs)
        .map(|_| {
            let u = f64::from(env.next_u32()) / f64::from(u32::MAX);
            math::db_to_gain(-40.0 + 34.0 * u)
        })
        .collect();
    let step = 2.0 * std::f64::consts::PI * 220.0 / f64::from(rate.max(1));
    let planes = (0..2u64)
        .map(|c| {
            let mut r = determinism::rng_stream(2, c);
            let mut lp = 0f64;
            Arc::new(
                (0..frames)
                    .map(|i| {
                        let sec = i / (rate.max(1) as usize);
                        if sec == 0 || sec + 1 == secs {
                            return 0.0;
                        }
                        let u = f64::from(r.next_u32()) / f64::from(u32::MAX) * 2.0 - 1.0;
                        lp = 0.7 * lp + 0.3 * u;
                        let level = levels.get(sec).copied().unwrap_or(0.0);
                        (level * (0.6 * lp + 0.4 * math::sin(step * i as f64))) as f32
                    })
                    .collect::<Vec<f32>>(),
            )
        })
        .collect();
    Ok(Value::Audio(Arc::new(Audio::new(
        rate,
        planes,
        AudioMeta::default(),
    )?)))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn nearest_rank_follows_the_definition() {
        assert_eq!(nearest_rank(&[], 95), None);
        assert_eq!(nearest_rank(&[7], 95), Some(7));
        assert_eq!(nearest_rank(&[7], 50), Some(7));
        let n19: Vec<u64> = (1..=19).collect();
        // ⌈0.95·19⌉ = 19、⌈0.5·19⌉ = 10
        assert_eq!(nearest_rank(&n19, 95), Some(19));
        assert_eq!(nearest_rank(&n19, 50), Some(10));
        let n20: Vec<u64> = (1..=20).collect();
        assert_eq!(nearest_rank(&n20, 95), Some(19));
        assert_eq!(nearest_rank(&n20, 50), Some(10));
        let n200: Vec<u64> = (1..=200).collect();
        assert_eq!(nearest_rank(&n200, 95), Some(190));
        assert_eq!(nearest_rank(&n200, 50), Some(100));
        assert_eq!(nearest_rank(&n200, 100), Some(200));
        assert_eq!(nearest_rank(&n200, 0), Some(1));
    }

    #[test]
    fn stats_use_p95_only_from_twenty_samples() {
        let s = Stats::from_samples(vec![5]).unwrap();
        assert_eq!(
            (s.n, s.min, s.p50, s.p95, s.max, s.mean),
            (1, 5, 5, None, 5, 5)
        );
        assert_eq!(s.judged(), 5);
        // 19 回は最大で判定する
        let s = Stats::from_samples((1..=19).rev().collect()).unwrap();
        assert_eq!((s.p95, s.judged(), s.p50, s.mean), (None, 19, 10, 10));
        assert_eq!(s.samples[0], 19);
        let s = Stats::from_samples((1..=20).collect()).unwrap();
        assert_eq!((s.p95, s.judged()), (Some(19), 19));
        let s = Stats::from_samples((1..=200).collect()).unwrap();
        assert_eq!(
            (s.p95, s.p50, s.max, s.over(190)),
            (Some(190), 100, 200, 10)
        );
        // 同じ値が並ぶとき
        let mut same = vec![3u64; 30];
        same.extend([9, 9]);
        let s = Stats::from_samples(same).unwrap();
        assert_eq!(
            (s.min, s.p50, s.p95, s.max, s.over(3)),
            (3, 3, Some(9), 9, 2)
        );
        assert_eq!(Stats::from_samples(Vec::new()), None);
        // 平均はあふれない
        let s = Stats::from_samples(vec![u64::MAX, u64::MAX]).unwrap();
        assert_eq!(s.mean, u64::MAX);
    }

    /// 慣らしは測らず、時計の差を 1 回ずつ取る。f には通しの番号を渡す。
    #[test]
    fn measure_skips_the_warmup_and_reads_the_clock() {
        let now = Cell::new(0u64);
        let clock = || {
            let t = now.get();
            now.set(t + 1);
            t
        };
        let seen = std::cell::RefCell::new(Vec::new());
        let s = measure(&clock, 3, 5, |i| {
            seen.borrow_mut().push(i);
            // 1 回ごとに i だけ時計を進める
            now.set(now.get() + i as u64);
            Ok(i)
        })
        .unwrap();
        assert_eq!(*seen.borrow(), (0..8).collect::<Vec<_>>());
        // 測る回は、始めの読みと終わりの読みの差 = 1（読み）+ i
        assert_eq!(s.samples, vec![4, 5, 6, 7, 8]);
        assert!(matches!(
            measure(&clock, 0, 0, |_| Ok(())),
            Err(EngineError::Internal(_))
        ));
        assert!(matches!(
            measure(&clock, 0, 3, |_| -> Result<(), _> {
                Err(EngineError::Canceled)
            }),
            Err(EngineError::Canceled)
        ));
    }

    /// 準備は測らず、準備と測る手順が同じ状態を受ける。
    #[test]
    fn measure_prepared_keeps_the_preparation_out() {
        let now = Cell::new(0u64);
        let clock = || now.get();
        let mut seen: Vec<(char, usize)> = Vec::new();
        let s = measure_prepared(
            &clock,
            2,
            3,
            &mut seen,
            |seen, i| {
                // 準備は時計を大きく進める（測った値に入らない）
                now.set(now.get() + 1_000);
                seen.push(('p', i));
                Ok(())
            },
            |seen, i| {
                now.set(now.get() + i as u64);
                seen.push(('f', i));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(s.samples, vec![2, 3, 4]);
        let want: Vec<(char, usize)> = (0..5).flat_map(|i| [('p', i), ('f', i)]).collect();
        assert_eq!(seen, want);
        assert!(matches!(
            measure_prepared(&clock, 0, 0, &mut (), |_, _| Ok(()), |_, _| Ok(())),
            Err(EngineError::Internal(_))
        ));
        assert!(matches!(
            measure_prepared(
                &clock,
                0,
                2,
                &mut (),
                |_, _| Err(EngineError::Canceled),
                |_, _| Ok(())
            ),
            Err(EngineError::Canceled)
        ));
    }

    #[test]
    fn targets_follow_the_requirements() {
        let t = target(11, "preview").unwrap();
        assert_eq!(t.limit_ns, Some(25 * MS));
        let fast = Stats::from_samples(vec![10 * MS; 20]).unwrap();
        let slow = Stats::from_samples(vec![30 * MS; 20]).unwrap();
        assert_eq!(t.verdict(&fast), Some(true));
        assert_eq!(t.verdict(&slow), Some(false));
        assert_eq!(target(12, "stroke").unwrap().limit_ns, Some(8 * MS));
        assert_eq!(target(12, "fill").unwrap().limit_ns, Some(30 * MS));
        assert_eq!(target(13, "save").unwrap().judge, Judge::Median);
        assert_eq!(target(14, "bulk").unwrap().limit_ns, Some(5_000 * MS));
        assert_eq!(target(15, "analysis").unwrap().limit_ns, Some(2_000 * MS));
        assert_eq!(target(16, "determinism").unwrap().verdict(&fast), None);
        assert_eq!(target(17, "memory").unwrap().verdict(&fast), None);
        assert!(target(18, "x").is_none());
        let nfrs: Vec<u8> = TARGETS.iter().map(|t| t.nfr).collect();
        assert_eq!(nfrs, [11, 12, 12, 13, 14, 15, 16, 17]);
        // 決めた範囲（ADR-154）：筆は太さ 32px まで
        assert!(
            target(12, "stroke")
                .unwrap()
                .item
                .contains("太さ 32px まで")
        );
        assert!(stroke_in_scope(1) && stroke_in_scope(32));
        assert!(!stroke_in_scope(33) && !stroke_in_scope(64));
    }

    /// ワークロードの入力は決定的（同じ種なら同じ。スレッド数によらない）で、記述どおりの形。
    #[test]
    fn workloads_are_deterministic() {
        let a = sprite_batch(40, 3);
        assert_eq!(a, sprite_batch(40, 3));
        assert_ne!(a, sprite_batch(40, 4));
        for (i, g) in a.iter().enumerate() {
            let InputGen::PixelSprite { w, h, fmt, .. } = g else {
                panic!("{g:?}")
            };
            assert!((16..=256).contains(w) && (16..=256).contains(h));
            let want = if i % 2 == 0 {
                PixelFormat::Rgba8
            } else {
                PixelFormat::Indexed8
            };
            assert_eq!(*fmt, want);
        }
        let s = walk_stroke(5, 500, 4096, 4096, 12);
        assert_eq!(s, walk_stroke(5, 500, 4096, 4096, 12));
        assert_eq!(s.len(), 500);
        assert!(
            s.windows(2)
                .all(|p| (p[0].x - p[1].x).abs() <= 12 && (p[0].y - p[1].y).abs() <= 12)
        );
        assert!(
            s.iter()
                .all(|p| (0..4096).contains(&p.x) && (0..4096).contains(&p.y))
        );
        assert!(walk_stroke(5, 0, 10, 10, 3).is_empty());
        // 小さな画像では縁で止まる
        let tiny = walk_stroke(9, 200, 3, 2, 50);
        assert!(
            tiny.iter()
                .all(|p| (0..3).contains(&p.x) && (0..2).contains(&p.y))
        );
        let PaintAction::Stroke { points, brush, .. } = pen(s.clone(), 8, false, [1, 2, 3, 255])
        else {
            panic!()
        };
        assert_eq!((points.0.len(), brush.size), (500, 8));
        let audio = program_audio(8_000, 4).unwrap();
        // 超越関数は crate::math なので、どの機械でも同じ値（凍結する）
        assert_eq!(
            audio.digest().unwrap().to_hex(),
            "e16cc7f3e2a2ab3d35484acdec20d06504c89555127608a1c1b96993b522a19e"
        );
        let Value::Audio(a) = &audio else { panic!() };
        assert_eq!((a.channels(), a.frames()), (2, 32_000));
        // 最初と最後の 1 秒は無音、途中は音がある
        assert!(a.planes()[0][..8_000].iter().all(|v| *v == 0.0));
        assert!(a.planes()[1][8_000..24_000].iter().any(|v| *v != 0.0));
    }
}
