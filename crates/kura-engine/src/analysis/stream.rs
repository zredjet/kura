//! 流れの解析の扇（M13 段 2。docs/editing.md §11.4）：1 つの音声の流れ（デコードの窓・値の窓）を、複数の解析の流れ
//! （`Analysis::stream`）へ同時に渡し、`analyze_many` と同じ形の結果を返す。解析のジョブが、音声の値を丸ごと持たずに
//! 解く口（`Engine::analyze_stream` → `codec::stream::DecodeSink`）。
//!
//! - 要求ごとの確かめは `Engine::analyze_dyn` の `prepare` と同じ順（引数を正規の形に・受け付ける種類・`Limits::check`・結果の
//!   見積もり）で、値の代わりに形（長さの上限の形を含む）で行う。流せない解析は `Unsupported`。
//! - `begin` で流れを作り直す（デコーダが次の段へ回ったら、前の試みで受けたものを捨てる）。
//! - `feed` は窓を各流れに渡す。1 つの流れの誤りはその要求に覚えて、ほかは続ける。中止（`Canceled`）は全体を止める。
//! - `finish` は `finish(end)` → `encode_any` → `Analyzed`（結果の大きさを `max_value_bytes` で確かめる）。結果は値に
//!   `analyze_many` したものとビット単位で同じ（E35）。

use std::sync::Arc;

use crate::analysis::{AnalysisRequest, AnalyzeCx, Analyzed, AudioStream, DynAnalysis};
use crate::audio::stream::StreamEnd;
use crate::error::{EngineError, Issue, IssueCode, Issues};
use crate::limits::Limits;
use crate::monitor::Monitor;
use crate::render::Engine;
use crate::shape::{Shape, ValueKind};

/// 要求 1 つの状態。
enum State {
    /// 流している。
    Live(Box<dyn AudioStream>),
    /// 誤りで止めた（確かめで断った・流れが誤りを返した）。
    Failed(EngineError),
}

/// 要求 1 つ。
struct Slot {
    /// 引いた解析と正規の引数（引けない・引数が読めないものは None で、`State::Failed`）。
    analysis: Option<(Arc<dyn DynAnalysis>, serde_json::Value)>,
    state: State,
}

/// 流れの解析の扇（[`Engine::analyze_stream`]）。
pub struct AnalysisFan {
    limits: Limits,
    slots: Vec<Slot>,
    pos: u64,
    end: Option<StreamEnd>,
}

/// 誤りの写し（`EngineError` は `Clone` でないので、作り直すときに同じ種類・文言にする）。
fn copy_error(e: &EngineError) -> EngineError {
    match e {
        EngineError::Invalid(i) => EngineError::Invalid(i.clone()),
        EngineError::Limit(m) => EngineError::Limit(m.clone()),
        EngineError::Unsupported(m) => EngineError::Unsupported(m.clone()),
        EngineError::Canceled => EngineError::Canceled,
        other => EngineError::Internal(other.to_string()),
    }
}

impl Engine {
    /// 形 `input` の音声の流れを、要求 `reqs` の解析へ同時に渡す扇を作る（M13 段 2）。窓は `AnalysisFan::feed`（か、
    /// `codec::stream::DecodeSink` としてデコーダから）で渡し、`finish` で要求の順の結果を得る。
    pub fn analyze_stream(&self, reqs: &[AnalysisRequest], input: &Shape) -> AnalysisFan {
        let slots = reqs
            .iter()
            .map(|req| {
                let resolved = self
                    .analyses()
                    .resolve(&req.id, req.v)
                    .map_err(|i| EngineError::Invalid(Issues::one(i)))
                    .and_then(|a| {
                        let params = a.check_params(&req.params).map_err(EngineError::Invalid)?;
                        Ok((a, params))
                    });
                match resolved {
                    Ok(a) => Slot {
                        analysis: Some(a),
                        state: State::Failed(EngineError::Internal(
                            "流れがまだ始まっていません".into(),
                        )),
                    },
                    Err(e) => Slot {
                        analysis: None,
                        state: State::Failed(e),
                    },
                }
            })
            .collect();
        let mut fan = AnalysisFan {
            limits: self.limits().clone(),
            slots,
            pos: 0,
            end: None,
        };
        fan.restart(input);
        fan
    }
}

impl AnalysisFan {
    /// 形 `input` で流れを作り直す（前に受けたものは捨てる）。確かめは `prepare` と同じ順。
    pub fn restart(&mut self, input: &Shape) {
        self.pos = 0;
        self.end = None;
        let limits = &self.limits;
        for slot in &mut self.slots {
            let Some((a, params)) = &slot.analysis else {
                if let State::Failed(e) = &slot.state {
                    slot.state = State::Failed(copy_error(e));
                }
                continue;
            };
            slot.state = match open(a.as_ref(), params, input, limits) {
                Ok(s) => State::Live(s),
                Err(e) => State::Failed(e),
            };
        }
    }

    /// フレーム `start` からの全チャンネルの標本を、生きている流れへ渡す。1 つの流れの誤りはその要求に覚える。中止は
    /// `Canceled` を返して全体を止める。
    pub fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        if mon.canceled() {
            return Err(EngineError::Canceled);
        }
        let cx = AnalyzeCx {
            mon,
            limits: &self.limits,
        };
        for slot in &mut self.slots {
            let State::Live(s) = &mut slot.state else {
                continue;
            };
            match s.feed(start, planes, &cx) {
                Ok(()) => {}
                Err(EngineError::Canceled) => return Err(EngineError::Canceled),
                Err(e) => slot.state = State::Failed(e),
            }
        }
        self.pos = start + planes.first().map_or(0, |p| p.len()) as u64;
        Ok(())
    }

    /// 流れの終わり（長さとメタデータ）を覚える。
    pub fn set_end(&mut self, end: &StreamEnd) {
        self.end = Some(end.clone());
    }

    /// 覚えた流れの終わり（`set_end` のあと）。
    pub fn end(&self) -> Option<&StreamEnd> {
        self.end.as_ref()
    }

    /// 要求の順の結果（`analyze_many` と同じ形）。終わりがなければどれも `Internal`。
    pub fn finish(self, mon: &dyn Monitor) -> Vec<Result<Analyzed, EngineError>> {
        let cx = AnalyzeCx {
            mon,
            limits: &self.limits,
        };
        let total = self.slots.len() as u64;
        let end = self.end;
        let limits = &self.limits;
        let mut out = Vec::with_capacity(self.slots.len());
        for (i, slot) in self.slots.into_iter().enumerate() {
            let r = match (slot.state, &slot.analysis, &end) {
                (State::Failed(e), _, _) => Err(e),
                (_, _, _) if mon.canceled() => Err(EngineError::Canceled),
                (State::Live(_), _, None) => Err(EngineError::Internal(
                    "流れの解析の終わりがありません".into(),
                )),
                (State::Live(_), None, _) => {
                    Err(EngineError::Internal("流れの解析を引けません".into()))
                }
                (State::Live(s), Some((a, params)), Some(end)) => {
                    finish_one(a.as_ref(), params, s, end, &cx, limits)
                }
            };
            out.push(r);
            mon.progress(i as u64 + 1, total);
        }
        out
    }
}

/// 要求 1 つの流れを開く（`prepare` と同じ確かめを形で）。
fn open(
    a: &dyn DynAnalysis,
    params: &serde_json::Value,
    input: &Shape,
    limits: &Limits,
) -> Result<Box<dyn AudioStream>, EngineError> {
    if !a.accepts().contains(&input.kind()) || input.kind() != ValueKind::Audio {
        return Err(EngineError::Invalid(Issues::one(Issue::error(
            IssueCode::BadInput,
            format!("{} は {:?} を解析できません", a.id(), input.kind()),
        ))));
    }
    limits.check(input).map_err(EngineError::Limit)?;
    let bytes = a
        .estimate(params, input)
        .map_err(EngineError::Invalid)?
        .bytes;
    if bytes > limits.max_value_bytes {
        return Err(EngineError::Limit(format!(
            "{} の結果が上限を超える見込みです（{bytes} バイト）",
            a.id()
        )));
    }
    a.stream_any(params, input)?
        .ok_or_else(|| EngineError::Unsupported(format!("解析 {} は流して解けません", a.id())))
}

/// 要求 1 つの流れを閉じて結果にする（`analyze_dyn` と同じ確かめ）。
fn finish_one(
    a: &dyn DynAnalysis,
    params: &serde_json::Value,
    s: Box<dyn AudioStream>,
    end: &StreamEnd,
    cx: &AnalyzeCx<'_>,
    limits: &Limits,
) -> Result<Analyzed, EngineError> {
    let out = s.finish(end, cx)?;
    let output = a.encode_any(&out)?;
    if output.len() as u64 > limits.max_value_bytes {
        return Err(EngineError::Limit(format!(
            "{} の結果が大きすぎます",
            a.id()
        )));
    }
    Ok(Analyzed {
        id: a.id(),
        version: a.version(),
        params: params.clone(),
        encoding: a.encoding(),
        output,
    })
}

impl crate::codec::stream::DecodeSink for AnalysisFan {
    /// デコーダが読み始めた形で流れを作り直す（前の試みで受けたものは捨てる）。
    fn begin(&mut self, info: &crate::codec::stream::DecodeInfo) -> Result<(), EngineError> {
        let channels = u16::try_from(info.channels)
            .map_err(|_| EngineError::Limit("チャンネルが多すぎます".into()))?;
        self.restart(&Shape::Audio {
            rate: info.rate,
            channels,
            frames: info.frames,
        });
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        AnalysisFan::feed(self, start, planes, mon)
    }

    fn end(&mut self, end: &StreamEnd) -> Result<(), EngineError> {
        self.set_end(end);
        Ok(())
    }
}
