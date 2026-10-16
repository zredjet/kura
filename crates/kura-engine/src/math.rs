//! 超越関数（docs/editing.md §8.2）。
//!
//! OS の libm は版や OS で結果が変わることがあるので、結果に効く計算はここの関数だけを使う。
//! 中身は `libm` クレート（版を `=` で固定し、CPU 固有の実装を外している）。
//! `sqrt`・`mul_add`・`round_ties_even` など IEEE で一意に決まるものは std をそのまま使ってよい。

/// x の y 乗。
pub fn pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}

/// e の x 乗。
pub fn exp(x: f64) -> f64 {
    libm::exp(x)
}

/// 自然対数。
pub fn ln(x: f64) -> f64 {
    libm::log(x)
}

/// 常用対数。
pub fn log10(x: f64) -> f64 {
    libm::log10(x)
}

/// 正弦。
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}

/// 余弦。
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// 大きいほう（IEEE 754-2019 の maximumNumber）。NaN は捨ててもう一方を返す。-0 < +0 として扱う
/// （std の `f64::max` は ±0 のどちらを返すか決まっていないので使わない）。
pub fn max(a: f64, b: f64) -> f64 {
    // 等しい（±0）なら +0 を選ぶ
    if a.is_nan() {
        b
    } else if b.is_nan() || a > b || (a == b && a.is_sign_positive()) {
        a
    } else {
        b
    }
}

/// 小さいほう（IEEE 754-2019 の minimumNumber）。NaN は捨ててもう一方を返す。-0 < +0 として扱う。
pub fn min(a: f64, b: f64) -> f64 {
    // 等しい（±0）なら -0 を選ぶ
    if a.is_nan() {
        b
    } else if b.is_nan() || a < b || (a == b && a.is_sign_negative()) {
        a
    } else {
        b
    }
}

/// `max` の f32 版。
pub fn max32(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        b
    } else if b.is_nan() || a > b || (a == b && a.is_sign_positive()) {
        a
    } else {
        b
    }
}

/// `min` の f32 版。
pub fn min32(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        b
    } else if b.is_nan() || a < b || (a == b && a.is_sign_negative()) {
        a
    } else {
        b
    }
}

/// デシベルを振幅の倍率にする（10 の dB/20 乗）。
pub fn db_to_gain(db: f64) -> f64 {
    pow(10.0, db / 20.0)
}

/// 振幅の倍率をデシベルにする（20 log10）。0 以下は負の無限大。
pub fn gain_to_db(gain: f64) -> f64 {
    if gain <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * log10(gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ±0 と NaN の扱いを決めた max・min（引数の順によらない）。
    #[test]
    fn max_min_define_signed_zero_and_nan() {
        let bits = |x: f64| x.to_bits();
        for (a, b) in [(0.0, -0.0), (-0.0, 0.0)] {
            assert_eq!(bits(max(a, b)), bits(0.0));
            assert_eq!(bits(min(a, b)), bits(-0.0));
            assert_eq!(max32(a as f32, b as f32).to_bits(), 0.0f32.to_bits());
            assert_eq!(min32(a as f32, b as f32).to_bits(), (-0.0f32).to_bits());
        }
        assert_eq!(max(f64::NAN, 1.0), 1.0);
        assert_eq!(max(1.0, f64::NAN), 1.0);
        assert_eq!(min(f64::NAN, -1.0), -1.0);
        assert!(max(f64::NAN, f64::NAN).is_nan());
        assert_eq!((max(1.0, 2.0), max(2.0, 1.0)), (2.0, 2.0));
        assert_eq!((min(1.0, 2.0), min(2.0, 1.0)), (1.0, 1.0));
        assert_eq!(max(f64::NEG_INFINITY, -1.0), -1.0);
        assert_eq!(max32(f32::NAN, 3.0), 3.0);
    }

    /// 実装を凍結する：この値が変わったら、libm の版を上げたか実装を変えたということ（ADR-136）。
    /// 値はこの実装で一度計算して書き写したもの。真の値との差が 1 ULP 以内であることも確かめる。
    #[test]
    fn frozen_bits() {
        let cases: [(&str, f64, f64, u64); 6] = [
            (
                "exp(1)",
                exp(1.0),
                std::f64::consts::E,
                0x4005_bf0a_8b14_576a,
            ),
            (
                "ln(10)",
                ln(10.0),
                std::f64::consts::LN_10,
                0x4002_6bb1_bbb5_5516,
            ),
            (
                "log10(2)",
                log10(2.0),
                std::f64::consts::LOG10_2,
                0x3fd3_4413_509f_79ff,
            ),
            (
                "sin(1)",
                sin(1.0),
                0.841_470_984_807_896_5,
                0x3fea_ed54_8f09_0cee,
            ),
            (
                "cos(1)",
                cos(1.0),
                0.540_302_305_868_139_8,
                0x3fe1_4a28_0fb5_068c,
            ),
            (
                "db_to_gain(-6)",
                db_to_gain(-6.0),
                0.501_187_233_627_272_2,
                0x3fe0_09b9_cf33_4252,
            ),
        ];
        let mut report = String::new();
        for (name, value, truth, _) in &cases {
            report.push_str(&format!("{name}: {value:?} {:#018x}\n", value.to_bits()));
            assert!(
                (value.to_bits() as i64 - truth.to_bits() as i64).abs() <= 1,
                "{name}: {value} と {truth} が 1 ULP より離れています"
            );
        }
        for (name, value, _, bits) in &cases {
            assert_eq!(value.to_bits(), *bits, "{name}\n{report}");
        }
    }

    #[test]
    fn db_round_trip_is_close() {
        for db in [-60.0, -6.0, 0.0, 3.0, 12.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-9);
        }
        assert_eq!(db_to_gain(0.0), 1.0);
        assert_eq!(gain_to_db(0.0), f64::NEG_INFINITY);
    }
}
