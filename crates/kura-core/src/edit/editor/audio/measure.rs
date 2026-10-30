//! ラウドネスの問い合わせ（M13 段 4。`edit_measure`）。
//!
//! - 計るのは今の rev の出力（量子化のあと）で、結果は出力の指紋（`samplesId`）ごとに覚える（[`RESULTS`] 件の LRU）。同じ標本
//!   なら `samplesId` が同じなので、取り消しで戻った・仮の値と同じ確定の値は計り直さない。
//! - 短い音声（出力の値が `WHOLE_VIEW_BYTES` 以下）：`edit_measure` の中で、キャッシュを引いて全体を描き（表示の範囲が全体なので
//!   ノードは当たる）、`audio.loudness` v1 を解いて、すぐ `Ready` を返す。
//! - 長い音声：段 2 の流れの解析が入るまで `Failed`（「この長さのラウドネスは、まだ計れません」）。段 4B（F12）で、裏の仕事で
//!   窓ごとに流して計る（`Measuring { progress }`）。
//! - `Latest` の世代を払い出さない（表示を止めない）。止まるのは閉じたときだけ。表示の情報の `loudness` にも、覚えている状態を
//!   毎回載せる（覚えていなければ `Idle`）。

use kura_engine::analysis::NoParams;
use kura_engine::analysis::Reasoned;
use kura_engine::analysis::audio::loudness::{LoudnessAnalysis, LoudnessReport, Unmeasured};
use kura_engine::render::cache::RenderOptions;
use kura_engine::render::estimate::{EstimateSpec, Source};

use super::super::{EditorEnv, EditorError, ProblemCode, State, editor_of, settle};
use super::{is_short, samples_id};

/// 覚える結果の数。
pub const RESULTS: usize = 8;

/// ラウドネスの値（M11 の素材の事実と同じ意味。計れない値は None で、積分の理由は `loudness_reason`）。
#[derive(Debug, Clone, PartialEq)]
pub struct LoudnessValues {
    /// 積分ラウドネス（LUFS）。
    pub loudness: Option<f64>,
    /// 積分を計れなかった理由（`tooShort`・`silent`・`belowGate`・`lowRate`・`nonFinite`）。
    pub loudness_reason: Option<String>,
    /// ラウドネスレンジ（LU）。
    pub loudness_range: Option<f64>,
    /// トゥルーピーク（dBTP）。
    pub true_peak: Option<f64>,
    /// サンプルピーク（dBFS）。
    pub sample_peak: Option<f64>,
}

/// ラウドネスの状態。
#[derive(Debug, Clone, PartialEq)]
pub enum LoudnessState {
    /// まだ計っていない。
    Idle,
    /// 計っている（裏の仕事。段 4B）。
    Measuring {
        samples_id: String,
        progress: f64,
    },
    Ready {
        samples_id: String,
        values: LoudnessValues,
    },
    Failed {
        samples_id: String,
        message: String,
    },
}

/// 理由の名前（`analysis::facts` の事実の `loudness_reason` と同じ綴り）。
fn reason_name(r: Unmeasured) -> &'static str {
    match r {
        Unmeasured::TooShort => "tooShort",
        Unmeasured::Silent => "silent",
        Unmeasured::BelowGate => "belowGate",
        Unmeasured::LowRate => "lowRate",
        Unmeasured::NonFinite => "nonFinite",
    }
}

fn level(l: &Reasoned<f64, Unmeasured>) -> Option<f64> {
    l.value().copied().filter(|v| v.is_finite())
}

/// 解析の結果を値にする（`analysis::facts::project` と同じ写し方）。
pub fn values_of(r: &LoudnessReport) -> LoudnessValues {
    LoudnessValues {
        loudness: level(&r.integrated),
        loudness_reason: r.integrated.reason().map(|x| reason_name(*x).to_string()),
        loudness_range: level(&r.range),
        true_peak: level(&r.true_peak),
        sample_peak: level(&r.sample_peak),
    }
}

/// 計りの結果の LRU（`AudioState` が持つ）。
#[derive(Debug, Default)]
pub(crate) struct MeasureJob {
    /// (samplesId, 状態)。古い順。
    results: Vec<(String, LoudnessState)>,
}

impl MeasureJob {
    /// 覚えている状態（なければ `Idle`）。
    pub(crate) fn state(&self, id: &str) -> LoudnessState {
        self.results
            .iter()
            .find(|(k, _)| k == id)
            .map_or(LoudnessState::Idle, |(_, s)| s.clone())
    }

    fn remember(&mut self, id: &str, state: LoudnessState) {
        self.results.retain(|(k, _)| k != id);
        self.results.push((id.to_string(), state));
        while self.results.len() > RESULTS {
            self.results.remove(0);
        }
    }
}

/// 今の rev の出力の `samplesId`。
fn current_id(editor: &super::super::Editor, st: &State) -> Result<String, EditorError> {
    let fps = editor
        .runtime
        .preview(|e| e.fingerprints(&st.plan, &st.inputs))
        .map_err(|e| EditorError::from_engine("出力の指紋を求められません", e))?;
    fps.outputs
        .iter()
        .find(|(o, _)| *o == st.output)
        .map(|(_, fp)| samples_id(fp))
        .ok_or_else(|| EditorError::Failed("出力の指紋がありません".into()))
}

/// 今の rev の出力のラウドネス（モジュールの説明）。
pub fn measure(env: &EditorEnv<'_>, session: &str) -> Result<LoudnessState, EditorError> {
    let editor = editor_of(env, session)?;
    let mut guard = editor.lock_state();
    let st = guard.as_mut().ok_or(EditorError::NoSession)?;
    if st.plan_rev != st.session.rev() {
        return Err(EditorError::Invalid {
            message: "今の文書の計画を作れません".into(),
            issues: Vec::new(),
        });
    }
    let full = st
        .plan
        .output_shapes()
        .into_iter()
        .find(|(id, _)| *id == st.output)
        .map(|(_, s)| s)
        .ok_or_else(|| EditorError::Failed("文書に出力がありません".into()))?;
    if st.audio.is_none() {
        return Err(EditorError::Invalid {
            message: "音声ではありません".into(),
            issues: Vec::new(),
        });
    }
    let id = current_id(&editor, st)?;
    if let Some(a) = st.audio.as_ref() {
        let known = a.measure.state(&id);
        if !matches!(known, LoudnessState::Idle) {
            return Ok(known);
        }
    }
    if !is_short(&full) {
        return Ok(LoudnessState::Failed {
            samples_id: id,
            message: "この長さのラウドネスは、まだ計れません".into(),
        });
    }
    let plan = st.plan.clone();
    let spec = EstimateSpec::new(0).all_inputs(&plan, Source::Held);
    if let Err(e) = editor.runtime.admit_preview(
        &plan,
        spec.clone(),
        &st.inputs,
        &mut st.cache,
        &mut st.lease,
    ) {
        let problem = editor.admit_problem(&plan, spec, st, e);
        settle(st);
        return Err(match problem.code {
            ProblemCode::TooLarge => EditorError::TooLarge(problem.message),
            _ => EditorError::Busy(problem.message),
        });
    }
    let (inputs, cache, mon) = (&st.inputs, &mut st.cache, editor.cancel.as_ref());
    let output = st.output;
    let r = editor.runtime.preview(|e| {
        let rendered = e.render_cached(&plan, inputs, cache, &RenderOptions::default(), mon)?;
        let value = rendered
            .outputs
            .into_iter()
            .find(|o| o.id == output)
            .map(|o| o.value)
            .ok_or_else(|| kura_engine::EngineError::Internal("出力がありません".into()))?;
        e.analyze::<LoudnessAnalysis>(&value, &NoParams {}, mon)
    });
    settle(st);
    let state = match r {
        Ok(report) => LoudnessState::Ready {
            samples_id: id.clone(),
            values: values_of(&report),
        },
        Err(e) if matches!(e.root(), kura_engine::EngineError::Canceled) => {
            return Err(EditorError::Canceled);
        }
        Err(e) => LoudnessState::Failed {
            samples_id: id.clone(),
            message: EditorError::from_engine("ラウドネスを計れません", e).to_string(),
        },
    };
    if let Some(a) = st.audio.as_mut() {
        a.measure.remember(&id, state.clone());
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_kept_by_samples_id() {
        let mut job = MeasureJob::default();
        assert_eq!(job.state("a"), LoudnessState::Idle);
        for i in 0..10 {
            job.remember(
                &format!("{i}"),
                LoudnessState::Failed {
                    samples_id: format!("{i}"),
                    message: String::new(),
                },
            );
        }
        assert_eq!(job.results.len(), RESULTS);
        assert_eq!(job.state("0"), LoudnessState::Idle, "古いものから捨てる");
        assert!(matches!(job.state("9"), LoudnessState::Failed { .. }));
    }
}
