//! ラウドネスの問い合わせ（M13 段 4。`edit_measure`）。
//!
//! - 計るのは今の rev の出力（量子化のあと）で、結果は出力の指紋（`samplesId`）ごとに覚える（[`RESULTS`] 件の LRU）。同じ標本
//!   なら `samplesId` が同じなので、取り消しで戻った・仮の値と同じ確定の値は計り直さない。
//! - 短い音声：`edit_measure` の中で、キャッシュを引いて全体を描き（`render_cached`。全体の値なので解析に使える）、
//!   `audio.loudness` v1 を解いて、すぐ `Ready` を返す。「短い」は出力の大きさだけでなく、全体を描く見積もり（上流を含む）で
//!   決める（`whole_is_short`：出力と、全体を描くときに作るどのノードの値も `WHOLE_VIEW_BYTES` 以下）。長い元を gain → trim
//!   で短くした文書は、表示は区間だけを描けても、全体を描くと上流に長い値ができるので長い音声と同じ扱い。範囲の描画・
//!   継ぎはぎの表示の値は解析に使わない。
//! - 長い音声（`whole_is_short` が偽。M13 段 4B）：`edit_measure` は計りを予約して `Measuring { progress: 0 }` を返す（同じ
//!   出力を計っている途中ならその進み）。計りは裏の仕事（`edit_view { work: true }` の区切り。別のスレッドは使わない）で、
//!   今の出力を窓（[`MEASURE_WINDOW_CHUNKS`] チャンクまで。締め切りが近ければ窓の途中まで：`slice`）ごとに頭から
//!   `Engine::stream_audio` で流し（読みは源の LRU に入れない：`read_through`）、流れの解析の扇（`AnalysisFan`。
//!   `audio.loudness` v1）に渡して解く。窓の前に、その区間だけの見積もり（`.area`）で `admit_preview` する。結果は全体の値に
//!   当てた解析とビット単位で同じ（E35・E39。A16。窓の切り方によらない）で、結果の LRU に入れる。
//!   - 順と区切りの予算：1 回の区切り（`WORK_SLICE`）は波形の段を先に進め、残った時間で計りを進める（波形は画面の主で、
//!     どちらも同じ出力を全体に流すので、順に済ませたほうがどちらも早く済む。段 4B の決定）。
//!   - 操作中（`interacting`）は進めない。計りかけを捨てるのは、操作中でない確定の表示（仮の値がない）で今の出力の
//!     `samplesId` が計っている出力と違うときと、`edit_measure` がほかの出力を予約したときだけ（ドラッグ中・仮の値の表示では
//!     残し、CancelPreview で同じ出力に戻れば続きから計る。段 4B の検証の直し）。確定の手で標本が替わったあと取り消しで
//!     戻っても、計り直しは `edit_measure` から。計っている間は表示の情報の `work` を真にする。裏の仕事は `edit_view
//!     { work: true }` でしか進まないので、フロントは `Measuring` の間 `work: true` の表示を頼み続ける。
//! - `Latest` の世代を払い出さない（表示を止めない）。止まるのは閉じたときだけ。表示の情報の `loudness` にも、覚えている状態を
//!   毎回載せる（計っている途中なら `Measuring`、覚えていなければ `Idle`）。

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use kura_engine::analysis::audio::loudness::{LoudnessAnalysis, LoudnessReport, Unmeasured};
use kura_engine::analysis::output::json_from_canonical;
use kura_engine::analysis::stream::AnalysisFan;
use kura_engine::analysis::{Analysis, AnalysisRequest, Analyzed, NoParams, Reasoned};
use kura_engine::audio::source::read_through;
use kura_engine::audio::stream::{AudioSink, StreamEnd, StreamInfo};
use kura_engine::monitor::Ticket;
use kura_engine::render::cache::RenderOptions;
use kura_engine::render::estimate::{EstimateSpec, Source};
use kura_engine::render::region::{Area, Spans};
use kura_engine::value::{AUDIO_CHUNK, AudioMeta};
use kura_engine::{EngineError, Monitor, NoMonitor};

use super::super::{Editor, EditorEnv, EditorError, ProblemCode, State, editor_of, settle};
use super::slice::{Limit, Pace, WorkProgress};
use super::{WHOLE_VIEW_BYTES, audio_shape, is_short, samples_id};

/// 覚える結果の数。
pub const RESULTS: usize = 8;
/// 長い音声の計りで 1 度に流す窓のチャンクの数の上限（262,144 フレーム。48 kHz ステレオで 1 窓 10 ms ほど）。窓の途中でも
/// チャンクごとに締め切りを見る（`slice`。段 7）。値に効かない。
pub const MEASURE_WINDOW_CHUNKS: u64 = 4;

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

/// 長い音声の計りかけ（裏の仕事で窓ごとに進める）。
struct MeasureRun {
    /// 計っている出力の `samplesId`。
    samples_id: String,
    /// 出力の長さ（フレーム）。
    frames: u64,
    /// 流れの解析の扇（`Sync` でないので錠で包む。触るのは状態の錠の中だけで、取り合わない）。
    fan: Mutex<AnalysisFan>,
    /// 流し終えたフレーム（頭から。チャンクに揃う。次の窓はここから）。
    pos: u64,
    /// 出力のメタデータ（最初の窓の `begin` で覚える。流れの終わりに渡す）。
    meta: Option<AudioMeta>,
}

impl MeasureRun {
    fn progress(&self) -> f64 {
        if self.frames == 0 {
            1.0
        } else {
            self.pos as f64 / self.frames as f64
        }
    }
}

/// 計りの結果の LRU と、長い音声の計りかけ（`AudioState` が持つ）。
#[derive(Default)]
pub(crate) struct MeasureJob {
    /// (samplesId, 状態)。古い順。
    results: Vec<(String, LoudnessState)>,
    run: Option<MeasureRun>,
    /// 裏の仕事で 1 チャンクを流して計る時間の見積もり（区切りの中で流す数を決める。段 7）。
    pub(crate) pace: Pace,
}

impl MeasureJob {
    /// 覚えている状態（計っている途中なら `Measuring`、なければ `Idle`）。
    pub(crate) fn state(&self, id: &str) -> LoudnessState {
        if let Some(r) = self.run.as_ref().filter(|r| r.samples_id == id) {
            return LoudnessState::Measuring {
                samples_id: id.to_string(),
                progress: r.progress(),
            };
        }
        self.results
            .iter()
            .find(|(k, _)| k == id)
            .map_or(LoudnessState::Idle, |(_, s)| s.clone())
    }

    /// 出力 `id` を計っている途中か（裏の仕事が残っている）。
    pub(crate) fn running(&self, id: &str) -> bool {
        self.run.as_ref().is_some_and(|r| r.samples_id == id)
    }

    /// 操作中でない確定の表示の出力が `id` になった：ほかの出力の計りかけを捨てる（仮の表示では呼ばない）。
    pub(crate) fn retarget(&mut self, id: &str) {
        if self.run.as_ref().is_some_and(|r| r.samples_id != id) {
            self.run = None;
        }
    }

    fn remember(&mut self, id: &str, state: LoudnessState) {
        self.results.retain(|(k, _)| k != id);
        self.results.push((id.to_string(), state));
        while self.results.len() > RESULTS {
            self.results.remove(0);
        }
    }

    /// 計りかけの出力・流し終えたチャンクの数・チャンクの数（テスト）。
    #[cfg(test)]
    pub(crate) fn run_position(&self) -> Option<(String, usize, usize)> {
        let c = AUDIO_CHUNK as u64;
        self.run.as_ref().map(|r| {
            (
                r.samples_id.clone(),
                r.pos.div_ceil(c) as usize,
                r.frames.div_ceil(c) as usize,
            )
        })
    }
}

/// 窓の流れを扇に渡す包み（`begin` のメタデータを覚え、渡せたかを記す。`finish` は何もしない：窓ごとに `stream_audio` を
/// 呼ぶので、流れの終わりは呼び手が最後の窓のあとに渡す）。
struct FanFeed<'a> {
    fan: &'a mut AnalysisFan,
    meta: &'a mut Option<AudioMeta>,
    fed: bool,
}

impl AudioSink for FanFeed<'_> {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        if self.meta.is_none() {
            *self.meta = Some(info.meta.clone());
        }
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        self.fan.feed(start, planes, mon)?;
        self.fed = true;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// 扇の結果（要求は `audio.loudness` だけ）を状態にする。
fn finished(id: &str, results: Vec<Result<Analyzed, EngineError>>) -> LoudnessState {
    let report = results
        .into_iter()
        .next()
        .ok_or_else(|| EngineError::Internal("ラウドネスの結果がありません".into()))
        .and_then(|r| r)
        .and_then(|a| json_from_canonical::<LoudnessReport>(&a.output));
    match report {
        Ok(report) => LoudnessState::Ready {
            samples_id: id.to_string(),
            values: values_of(&report),
        },
        Err(e) => LoudnessState::Failed {
            samples_id: id.to_string(),
            message: EditorError::from_engine("ラウドネスを計れません", e).to_string(),
        },
    }
}

impl Editor {
    /// 長い音声の計りを裏の仕事で進める（モジュールの説明）。今の出力 `id` の計りかけがあれば、区切り `limit` の中で窓
    /// （[`MEASURE_WINDOW_CHUNKS`] まで。締め切りが近ければ窓の途中まで）を 1 つずつ頭から流し（窓の前にその区間だけの見積もりで
    /// 確保する。空いていなければあとにする）、流し終えたら解いて結果の LRU に入れる。中止（新しい要求・閉じる）は窓の境で
    /// 止め、計りかけを残す（途中で中止した窓は次の区切りで流し直す）。進みは `progress` に足す。
    pub(super) fn advance_measure(
        &self,
        st: &mut State,
        id: &str,
        limit: &mut Limit,
        progress: &mut WorkProgress,
        ticket: &Ticket,
    ) {
        if !st.audio.as_ref().is_some_and(|a| a.measure.running(id)) {
            return;
        }
        let plan = st.plan.clone();
        let output = st.output;
        let c = AUDIO_CHUNK as u64;
        loop {
            let Some(job) = st.audio.as_mut().map(|a| &mut a.measure) else {
                return;
            };
            let pace = job.pace;
            let Some(run) = job.run.as_mut() else {
                return;
            };
            if run.pos >= run.frames {
                break;
            }
            if ticket.canceled() {
                return;
            }
            // 窓の途中でも、締め切りまでに流せるチャンクの数だけにする（続きは次の区切り）
            let n = limit.grant(
                &pace,
                (run.frames - run.pos)
                    .div_ceil(c)
                    .min(MEASURE_WINDOW_CHUNKS),
            );
            if n == 0 {
                return;
            }
            let (a, b) = (run.pos, run.pos.saturating_add(n * c).min(run.frames));
            let started = Instant::now();
            let spans = Spans::one(a, b);
            let spec = EstimateSpec::new(0)
                .all_inputs(&plan, Source::Held)
                .area(output, Area::samples(spans.clone()));
            if let Err(err) =
                self.runtime
                    .admit_preview(&plan, spec, &st.inputs, &mut st.cache, &mut st.lease)
            {
                tracing::debug!(%err, "ラウドネスを計る予算が空いていないので、あとにします");
                settle(st);
                progress.blocked = true;
                return;
            }
            let Some(run) = st.audio.as_mut().and_then(|a| a.measure.run.as_mut()) else {
                settle(st);
                return;
            };
            let (inputs, cache) = (&st.inputs, &mut st.cache);
            let mut feed = FanFeed {
                fan: run.fan.get_mut().unwrap_or_else(PoisonError::into_inner),
                meta: &mut run.meta,
                fed: false,
            };
            // 流して読む読みは源の LRU に入れない（見えている区間のチャンクを追い出さない。段 7）
            let r = self.runtime.preview(|e| {
                read_through(|| {
                    e.stream_audio(
                        &plan,
                        inputs,
                        cache,
                        output,
                        Some(&spans),
                        &mut feed,
                        ticket,
                    )
                })
            });
            if feed.fed {
                run.pos = b;
            }
            settle(st);
            match r {
                Ok(_) => {
                    progress.moved += n;
                    if let Some(a) = st.audio.as_mut() {
                        a.measure.pace.note(n, started.elapsed());
                    }
                }
                Err(err) if matches!(err.root(), EngineError::Canceled) => return,
                Err(err) => {
                    let state = LoudnessState::Failed {
                        samples_id: id.to_string(),
                        message: EditorError::from_engine("ラウドネスを計れません", err)
                            .to_string(),
                    };
                    if let Some(a) = st.audio.as_mut() {
                        a.measure.run = None;
                        a.measure.remember(id, state);
                    }
                    progress.blocked = true;
                    return;
                }
            }
        }
        // 流し終えた：解く（短い計算なので中止を見ない。新しい要求に取って代わられても計りかけを無駄にしない）
        let Some(a) = st.audio.as_mut() else {
            return;
        };
        let Some(run) = a.measure.run.take() else {
            return;
        };
        let MeasureRun {
            fan, frames, meta, ..
        } = run;
        let mut fan = fan.into_inner().unwrap_or_else(PoisonError::into_inner);
        fan.set_end(&StreamEnd {
            frames,
            meta: meta.unwrap_or_default(),
        });
        let state = finished(id, fan.finish(&NoMonitor));
        a.measure.remember(id, state);
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

/// 全体を描いても長い値を作らないか：出力が短く（[`is_short`]）、上流を含む全体の描画の見積もりで、どのノードが新しく作る
/// 値も [`WHOLE_VIEW_BYTES`] 以下（モジュールの説明）。
fn whole_is_short(
    editor: &super::super::Editor,
    st: &State,
    full: &kura_engine::Shape,
) -> Result<bool, EditorError> {
    if !is_short(full) {
        return Ok(false);
    }
    let spec = EstimateSpec::new(0).all_inputs(&st.plan, Source::Held);
    let est = editor
        .runtime
        .preview(|e| e.estimate(&st.plan, &spec, None))
        .map_err(|e| EditorError::from_engine("全体の描画を見積もれません", e))?;
    Ok(est.nodes.iter().all(|n| n.value <= WHOLE_VIEW_BYTES))
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
    if !whole_is_short(&editor, st, &full)? {
        // 長い音声：計りを予約する（裏の仕事で窓ごとに流して解く）
        let frames = audio_shape(&full)
            .map(|(_, _, n)| n)
            .ok_or_else(|| EditorError::Failed("長さの分からない音声は計れません".into()))?;
        let req = AnalysisRequest {
            id: LoudnessAnalysis::ID.into(),
            v: Some(LoudnessAnalysis::VERSION),
            params: serde_json::json!({}),
        };
        let fan = editor
            .runtime
            .preview(|e| e.analyze_stream(std::slice::from_ref(&req), &full));
        if let Some(a) = st.audio.as_mut() {
            a.measure.run = Some(MeasureRun {
                samples_id: id.clone(),
                frames,
                fan: Mutex::new(fan),
                pos: 0,
                meta: None,
            });
            return Ok(a.measure.state(&id));
        }
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
