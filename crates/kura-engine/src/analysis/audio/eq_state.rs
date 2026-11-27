//! `audio.eq_state` v1（内部用）：イコライザ（`audio.eq` v1）の状態の表（M17 段 2。docs/editing.md §18.1・§18.5）。
//!
//! - 引数はバンドの列だけ（状態に効く引数。出力の利得と、段を作らないバンド（オフ・0 dB）は入れないので、それらを
//!   動かしても表を解き直さない）。推論の扱い（ナイキスト・チャンネル）は操作と同じ `ops::audio::eq::resolve` を入力の
//!   形で当てる。
//! - 結果は状態の表（`audio::state::StateTable`。正規の形 `kura-state` 版 1、格子 `STATE_GRID`）。表は頭から順に流して
//!   作る（`run` と `stream` が同じ実装）。操作が申告するので凍結する（`goldens-analysis/audio.eq_state/`）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::analysis::{Analysis, AnalysisExample, AnalyzeCx, AudioStream, Estimate};
use crate::audio::state::{STATE_GRID, StateTable, TableBuilder, table_estimate, table_stream};
use crate::error::EngineError;
use crate::op::{Numeric, OpParams, ParamKind, ParamSchema, Unit, spec};
use crate::ops::audio::eq::{Band, EqMachine, EqParams, Phase, resolve};
use crate::shape::{Shape, ValueKind};
use crate::value::Value;

/// 引数（バンドの列）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EqStateParams {
    pub bands: Vec<Band>,
}

impl EqStateParams {
    /// 操作の引数から（出力の利得・位相と、段を作らないバンドを除く。順は保つ）。
    pub fn of(p: &EqParams) -> Self {
        Self {
            bands: p.bands.iter().filter(|b| !b.is_inert()).cloned().collect(),
        }
    }

    fn eq(&self) -> EqParams {
        EqParams {
            bands: self.bands.clone(),
            phase: Phase::Minimum,
            gain_db: 0.0,
        }
    }
}

impl OpParams for EqStateParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec("bands", ParamKind::EqBands, Unit::None, json!([]))],
        }
    }
}

/// 処理を作る（推論で断る引数・音声でない形は誤り）。
fn machine(p: &EqStateParams, rate: u32, channels: usize) -> Result<EqMachine, EngineError> {
    let r = resolve(&p.eq(), rate, channels).map_err(EngineError::Invalid)?;
    Ok(EqMachine::new(&r, channels, 1.0))
}

/// イコライザの状態の表（内部用）。
pub struct EqState;

impl Analysis for EqState {
    const ID: &'static str = "audio.eq_state";
    const VERSION: u16 = 1;
    const ACCEPTS: &'static [ValueKind] = &[ValueKind::Audio];
    const NUMERIC: Numeric = Numeric::Float;
    const INTERNAL: bool = true;
    type Params = EqStateParams;
    type Output = StateTable;

    fn run(p: &EqStateParams, v: &Value, cx: &AnalyzeCx<'_>) -> Result<StateTable, EngineError> {
        let Value::Audio(a) = v else {
            return Err(EngineError::Internal(format!(
                "{} の入力が音声ではありません",
                Self::ID
            )));
        };
        let m = machine(p, a.rate(), a.channels())?;
        TableBuilder::solve(Arc::new(m), STATE_GRID, v, cx)
    }

    fn stream(p: &EqStateParams, input: &Shape) -> Option<Box<dyn AudioStream>> {
        let Shape::Audio { rate, channels, .. } = *input else {
            return None;
        };
        let m = machine(p, rate, usize::from(channels)).ok()?;
        table_stream(Arc::new(m), STATE_GRID, input)
    }

    fn examples() -> Vec<AnalysisExample<EqStateParams>> {
        use crate::op::Op;
        crate::ops::audio::eq::Eq::examples()
            .into_iter()
            // 段のない例と、表を申告しない直線位相の例（M17 段 7）は除く
            .filter(|e| {
                e.name != "identity" && e.name != "gain-only" && e.params.phase == Phase::Minimum
            })
            .map(|e| AnalysisExample {
                name: e.name,
                params: EqStateParams::of(&e.params),
                input: e.inputs[0].clone(),
            })
            .collect()
    }

    /// 段 2 の実測の式（docs/editing.md §18.5 の「手ごとに正で描く境」）：12 + S·(1.5 + S/16) ns / フレーム（S は推論のあとの
    /// 2 次の段の数。M/S の組（幅 2）は 2 倍に数える）。
    fn solve_ns_per_frame(p: &EqStateParams, input: &Shape) -> f64 {
        let Shape::Audio { rate, channels, .. } = *input else {
            return crate::analysis::DEFAULT_SOLVE_NS_PER_FRAME;
        };
        let Ok(r) = resolve(&p.eq(), rate, usize::from(channels)) else {
            return crate::analysis::DEFAULT_SOLVE_NS_PER_FRAME;
        };
        let sections: usize = r.bands.iter().map(|b| b.sections.len()).sum();
        let width = if r.has_ms() && channels == 2 {
            2.0
        } else {
            1.0
        };
        let s = sections as f64 * width;
        crate::analysis::DEFAULT_SOLVE_NS_PER_FRAME + s * (1.5 + s / 16.0)
    }

    fn estimate(p: &EqStateParams, input: &Shape) -> Estimate {
        let Shape::Audio { rate, channels, .. } = *input else {
            return Estimate::default();
        };
        let ch = usize::from(channels);
        match machine(p, rate, ch) {
            Ok(m) => {
                use crate::audio::state::Stateful;
                let w = m.lane_width().max(1);
                table_estimate(
                    STATE_GRID,
                    input,
                    (ch / w) as u64,
                    m.state_len() as u64,
                    (m.state_len() as u64).max(1),
                )
            }
            Err(_) => Estimate::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::audio::eq::{BandType, band};

    /// 段を作らないバンド（オフ・0 dB）は表の引数に入らない（それを動かしても表を解き直さない）。順は保つ。
    #[test]
    fn inert_bands_do_not_reach_the_table() {
        let mut off = band(BandType::Bell, 500.0, 6.0, 1.0);
        off.enabled = false;
        let p = |off_freq: f64, flat_q: f64, gain: f64| {
            let mut o = off.clone();
            o.freq = off_freq;
            EqParams {
                bands: vec![
                    band(BandType::LowCut, 40.0, 0.0, 0.7),
                    o,
                    band(BandType::Bell, 2_000.0, 0.0, flat_q),
                    band(BandType::HighShelf, 8_000.0, -3.0, 0.7),
                ],
                phase: Phase::Minimum,
                gain_db: gain,
            }
        };
        let a = EqStateParams::of(&p(500.0, 1.0, 0.0));
        assert_eq!(a, EqStateParams::of(&p(900.0, 4.0, -6.0)));
        let kinds: Vec<_> = a.bands.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, vec![BandType::LowCut, BandType::HighShelf]);
    }
}
