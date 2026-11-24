//! `audio.eq_state` v1（内部用）：イコライザ（`audio.eq` v1）の状態の表（M17 段 2。docs/editing.md §18.1・§18.5）。
//!
//! - 引数はバンドの列だけ（状態に効く引数。出力の利得は入れないので、それだけを動かしても表を解き直さない）。推論の扱い
//!   （ナイキスト・チャンネル）は操作と同じ `ops::audio::eq::resolve` を入力の形で当てる。
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
    /// 操作の引数から（出力の利得・位相を除く）。
    pub fn of(p: &EqParams) -> Self {
        Self {
            bands: p.bands.clone(),
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
            .filter(|e| e.name != "identity" && e.name != "gain-only")
            .map(|e| AnalysisExample {
                name: e.name,
                params: EqStateParams::of(&e.params),
                input: e.inputs[0].clone(),
            })
            .collect()
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
