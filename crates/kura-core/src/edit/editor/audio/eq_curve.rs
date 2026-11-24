//! イコライザの曲線（M17 段 2。`edit_eq_curve`。docs/editing.md §18.3・§18.5）：引数とレートから、描かずに、対数の周波数の
//! 格子のバンドごと・合成の利得（dB）と段の係数を返す。式は kura-engine（`ops::audio::eq::curve`）だけにあり、フロントは
//! 返った点を描くだけ。セッションも描画の錠も待たない（ドラッグの間、点・曲線を毎回の描画で追うため）。

use kura_engine::Issue;
use kura_engine::op::OpParams;
use kura_engine::ops::audio::eq::{EqParams, curve};

use super::super::EditorError;

/// 点の数の上限。
pub const MAX_POINTS: u32 = 2_048;
/// 格子の下端（Hz）。
pub const LOW_HZ: f64 = 20.0;

/// 曲線。
#[derive(Debug, Clone, PartialEq)]
pub struct EqCurve {
    /// 周波数（Hz。20 Hz〜ナイキストの対数の格子）。
    pub freqs: Vec<f64>,
    /// 合成（全バンドの和 ＋ 出力の利得。dB）。
    pub total: Vec<f64>,
    pub bands: Vec<EqBandCurve>,
}

/// バンド 1 つの曲線。
#[derive(Debug, Clone, PartialEq)]
pub struct EqBandCurve {
    /// 引数のバンドの番号（外した・段のないバンドは出さない）。
    pub index: usize,
    pub db: Vec<f64>,
    /// 段の係数 `[b0, b1, b2, a1, a2]`。
    pub sections: Vec<[f64; 5]>,
}

fn invalid(message: impl Into<String>, issues: Vec<Issue>) -> EditorError {
    EditorError::Invalid {
        message: message.into(),
        issues,
    }
}

/// 引数 `params`（`audio.eq` の引数の JSON）・レート・チャンネルの数で、`points` 点（2〜2,048）の曲線を求める。引数が読めない・
/// 範囲の外・推論で断る（ナイキスト以上のローカットなど）なら `Invalid`。
pub fn eq_curve(
    params: &serde_json::Value,
    rate: u32,
    channels: u16,
    points: u32,
) -> Result<EqCurve, EditorError> {
    if rate == 0 || channels == 0 {
        return Err(invalid("レートとチャンネルの数は 1 以上です", Vec::new()));
    }
    let ranges = EqParams::schema().check_ranges(params);
    if !ranges.is_empty() {
        return Err(invalid("イコライザの引数が範囲の外です", ranges.0));
    }
    let p: EqParams = serde_json::from_value(params.clone())
        .map_err(|e| invalid(format!("イコライザの引数が読めません：{e}"), Vec::new()))?;
    let n = points.clamp(2, MAX_POINTS) as usize;
    let top = f64::from(rate) * 0.5;
    let ratio = (top / LOW_HZ).max(1.0);
    let freqs: Vec<f64> = (0..n)
        .map(|k| LOW_HZ * kura_engine::math::pow(ratio, k as f64 / (n - 1) as f64))
        .collect();
    let c = curve(&p, rate, usize::from(channels), &freqs)
        .map_err(|issues| invalid("イコライザの引数を当てられません", issues.0))?;
    Ok(EqCurve {
        freqs,
        total: c.total,
        bands: c
            .bands
            .into_iter()
            .map(|b| EqBandCurve {
                index: b.index,
                db: b.db,
                sections: b
                    .sections
                    .iter()
                    .map(|s| [s.b0, s.b1, s.b2, s.a1, s.a2])
                    .collect(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 対数の格子（20 Hz〜ナイキスト）で、ベルの中心で利得、合成は出力の利得を足す。範囲の外・ナイキスト以上のローカット・
    /// 読めない引数は Invalid。
    #[test]
    fn curve_follows_the_parameters() {
        let p = json!({
            "bands": [
                { "type": "bell", "freq": 1000.0, "gainDb": 6.0, "q": 1.0 },
                { "type": "lowCut", "freq": 30.0, "slope": 24 }
            ],
            "phase": "minimum",
            "gainDb": -2.0
        });
        let c = eq_curve(&p, 48_000, 2, 512).unwrap();
        assert_eq!(c.freqs.len(), 512);
        assert!((c.freqs[0] - 20.0).abs() < 1e-9);
        assert!((c.freqs[511] - 24_000.0).abs() < 1e-6);
        assert_eq!(c.bands.len(), 2);
        assert_eq!(c.bands[1].sections.len(), 2);
        let k = c.freqs.iter().position(|f| *f >= 1_000.0).unwrap();
        assert!((c.bands[0].db[k] - 6.0).abs() < 0.2);
        for (i, t) in c.total.iter().enumerate() {
            let sum: f64 = c.bands.iter().map(|b| b.db[i]).sum();
            assert!((t - (sum - 2.0)).abs() < 1e-9);
        }
        assert_eq!(eq_curve(&p, 48_000, 2, 100_000).unwrap().freqs.len(), 2_048);
        let bad = json!({ "bands": [{ "type": "bell", "freq": 5.0 }] });
        assert!(matches!(
            eq_curve(&bad, 48_000, 2, 64),
            Err(EditorError::Invalid { .. })
        ));
        let above = json!({ "bands": [{ "type": "lowCut", "freq": 5_000.0 }] });
        assert!(matches!(
            eq_curve(&above, 8_000, 2, 64),
            Err(EditorError::Invalid { .. })
        ));
        let unknown = json!({ "bands": [{ "type": "wobble", "freq": 100.0 }] });
        assert!(matches!(
            eq_curve(&unknown, 48_000, 2, 64),
            Err(EditorError::Invalid { .. })
        ));
        assert!(eq_curve(&p, 0, 2, 64).is_err());
    }
}
