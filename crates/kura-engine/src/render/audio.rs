//! 音声の範囲の描画（M13 段 1。docs/editing.md §12.5）：`render_area`（area.rs）の音声の部品。
//!
//! - 音声の範囲を持つノード（[`Engine::audio_regional`]）：有効で、出力と入力 0 が音声で、`Op::audio_reach`（核が入力 0 の
//!   どこを読むか）が `Global` でないもの。申告した解析（normalize のラウドネス）は先に解く（キャッシュ・入力が完全な値なら
//!   そのまま当てる）。
//! - 要る区間は出力のチャンク（`AUDIO_CHUNK`）に揃え（[`round_to_chunks`]）、`audio_reach` で入力 0 へ写す。前から描くときは、
//!   揃えた区間に掛かるチャンクだけを核で描き（`audio::render_chunks`。ずらした写しは入力の実体を共有）、ほかのチャンクは
//!   前の値のもの（継ぎはぎ）か無効にする。
//! - 部分の値はキャッシュに入れない。全体を描いたノードは `render_cached` の外れの道と同じ（由来 Exact で入れる）。
//! - 申告した解析の入力が部分の値でキャッシュにもなければ、入力のポートを窓ごとに描いて流れの解析（`Analysis::stream`）で
//!   解く（流し解き。結果はキャッシュに入れる）。
//! - 流れ（[`Engine::stream_audio`]）：出力を `STREAM_CHUNKS` チャンクの窓ごとに描いて `AudioSink` に渡す。
//!   [`Engine::digest_audio_output`] はチャンネルの数だけ流してダイジェストを取る。

use std::collections::BTreeMap;
use std::sync::Arc;

use super::area::{AreaStats, PassCx, PassError, Presolved};
use super::cache::{
    FingerprintedInput, NodeCache, analysis_fingerprint, insert_analysis, node_input_fingerprints,
    solve_cached,
};
use super::region::{Area, Reach, Spans};
use super::{Engine, Plan, PlannedNode, apply_node, bad_input, node_seed, op_shapes};
use crate::analysis::{AnalyzeCx, AnyOutput, AudioStream, DynAnalysis, Need};
use crate::audio::stream::{AudioSink, PcmDigest, StreamInfo, stream_windows};
use crate::digest::Digest;
use crate::doc::{InputId, OutputId, PortRef, Quantize};
use crate::error::{EngineError, Issue};
use crate::monitor::Monitor;
use crate::op::{Applied, ApplyCx};
use crate::shape::{Dim, Shape};
use crate::value::{AUDIO_CHUNK, Audio, AudioMeta, ChunkSet, Value};

/// 要る範囲を、出力の音声のチャンクの格子に広げる：区間 [a, b) を [⌊a/C⌋·C, ⌈b/C⌉·C) ∩ [0, n) に（核はチャンクごとに描くので、
/// 入力に要る区間はチャンクの区間から逆写しする）。`All` は長さが決まっていれば全区間に開く。画素の範囲は `All`。
pub(super) fn round_to_chunks(area: &Area, shape: &Shape) -> Area {
    let Shape::Audio { frames, .. } = shape else {
        return area.clone();
    };
    let n = frames.max();
    let area = match frames {
        Dim::Exact(_) => area.expand(shape),
        Dim::AtMost(_) => area.clone(),
    };
    let c = AUDIO_CHUNK as u64;
    match &area {
        Area::Samples(spans) => {
            let mut out = Spans::new();
            for &(a, b) in spans.spans() {
                let (a, b) = (a.min(n), b.min(n));
                if a < b {
                    out.push(a / c * c, b.div_ceil(c).saturating_mul(c).min(n));
                }
            }
            Area::samples(out)
        }
        Area::Pixels(_) => Area::All,
        other => other.clone(),
    }
}

/// 長さ `frames` の音声で、範囲 `area` に掛かるチャンクの番号（`All`・画素の範囲はすべて、空は空）。
pub fn chunks_covering(area: &Area, frames: usize) -> ChunkSet {
    let count = frames.div_ceil(AUDIO_CHUNK);
    match area {
        Area::Empty => ChunkSet::empty(count),
        Area::All | Area::Pixels(_) => ChunkSet::all(count),
        Area::Samples(spans) => {
            let mut set = ChunkSet::empty(count);
            let (n, c) = (frames as u64, AUDIO_CHUNK as u64);
            for &(a, b) in spans.spans() {
                let b = b.min(n);
                if a >= b {
                    continue;
                }
                for k in a / c..b.div_ceil(c) {
                    set.insert(usize::try_from(k).unwrap_or(usize::MAX));
                }
            }
            set
        }
    }
}

/// 長さ `frames` の音声で、範囲 `area` に掛かるチャンクの長さの和（[`chunks_covering`] の和を、集合を作らずに求める。
/// 見積もりで、長さの決まっていない音声の上限にも使う）。
pub(super) fn covered_frames(area: &Area, frames: u64) -> u64 {
    let c = AUDIO_CHUNK as u64;
    match area {
        Area::Empty => 0,
        Area::All | Area::Pixels(_) => frames,
        Area::Samples(spans) => {
            // 区間は小さい順・重ならないが、隣の区間と同じチャンクに掛かりうる（そのチャンクは 1 度だけ数える）
            let mut next = 0u64;
            let mut sum = 0u64;
            for &(a, b) in spans.spans() {
                let b = b.min(frames);
                if a >= b {
                    continue;
                }
                let lo = (a / c).max(next);
                let hi = b.div_ceil(c);
                if lo < hi {
                    let start = lo.saturating_mul(c);
                    let end = hi.saturating_mul(c).min(frames);
                    sum = sum.saturating_add(end.saturating_sub(start));
                    next = hi;
                }
            }
            sum
        }
    }
}

/// チャンクの集合の区間（隣り合うものは 1 つに。長さ `frames` で切る）。
pub fn chunk_spans(set: &ChunkSet, frames: usize) -> Spans {
    let mut out = Spans::new();
    let (n, c) = (frames as u64, AUDIO_CHUNK as u64);
    for k in set.iter() {
        let a = (k as u64).saturating_mul(c);
        out.push(a.min(n), a.saturating_add(c).min(n));
    }
    out
}

/// 音声の範囲を持つノード 1 つを描いた結果。
pub(super) struct AudioRun {
    pub value: Value,
    pub apply_warnings: Vec<Issue>,
    pub map_warnings: Vec<Issue>,
    /// 描いたチャンクの数（番号の数）。
    pub drawn: u64,
    /// 入力の実体を共有したチャンクの数（ずらした写し）。
    pub shared: u64,
}

impl Engine {
    /// 音声の範囲を持つノードか（有効・持ち上げない・出力と入力 0 が音声・`audio_reach` が `Global` でない）。申告した解析が
    /// あってもよい（描く前に解く）。
    pub(super) fn audio_regional(
        &self,
        node: &PlannedNode,
        in_shapes: &[Shape],
    ) -> Result<bool, EngineError> {
        if !node.enabled
            || node.lifted
            || !matches!(node.inferred.shape, Shape::Audio { .. })
            || !matches!(in_shapes.first(), Some(Shape::Audio { .. }))
        {
            return Ok(false);
        }
        let reach = node
            .op
            .audio_reach(node.params.as_ref(), &op_shapes(node, in_shapes))?;
        Ok(reach != Reach::Global)
    }

    /// 申告した解析を、範囲の描画の前に解けるか（キャッシュにあるか、入力のポートが完全な値（計画の入力・キャッシュに当たる
    /// ノード）か、入力を流して解ける（`Analysis::stream`。流し解き）か）。解けなければ、そのノードは全体で描く。
    pub(super) fn needs_ready(
        &self,
        cx: &PassCx<'_>,
        cache: &NodeCache,
        node: &PlannedNode,
    ) -> Result<bool, EngineError> {
        if node.needs.is_empty() {
            return Ok(true);
        }
        let in_fps = node_input_fingerprints(node, &cx.fps.nodes, &cx.input_fps)?;
        for need in &node.needs {
            let (Some(r), Some(fp)) = (node.inputs.get(need.input), in_fps.get(need.input)) else {
                return Ok(false);
            };
            let key = analysis_fingerprint(*fp, need.id, need.v, &need.params)?;
            if cx.port_complete(cache, r) || cache.contains_analysis(&key) {
                continue;
            }
            let streamable = match cx.shapes.port(r) {
                Some(shape) => self
                    .solvable(need)?
                    .stream_any(&need.params, &shape)?
                    .is_some(),
                None => false,
            };
            if !streamable {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 申告した解析がどれも、入力の形で流して解けるか（キャッシュを見ない見積もりの判定。流せる解析は、範囲の描画で
    /// いつも解ける）。
    pub(super) fn needs_streamable(
        &self,
        shapes: &super::area::Shapes,
        node: &PlannedNode,
    ) -> Result<bool, EngineError> {
        for need in &node.needs {
            let Some(shape) = node.inputs.get(need.input).and_then(|r| shapes.port(r)) else {
                return Ok(false);
            };
            if self
                .solvable(need)?
                .stream_any(&need.params, &shape)?
                .is_none()
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 音声の範囲を持つノード 1 つを、`rounded`（出力のチャンクに揃えた区間）のチャンクだけ描く。全体の描画と同じ `apply_node`
    /// を通る（写し方の確かめ・メタデータの写しは同じ）。申告した解析は先に解く（流し解きした結果 → キャッシュ → 入力が完全な
    /// 値なら解いて入れる）。核のメタデータ（`meta()`、なければ入力 0 のもの）を付けて描き、写し方は実行器が当て直す。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_audio_region(
        &self,
        cx: &PassCx<'_>,
        cache: &mut NodeCache,
        node: &PlannedNode,
        ins: &[Value],
        rounded: &Area,
        base: Option<&Value>,
        presolved: &mut Presolved,
        stats: &mut AreaStats,
        mon: &dyn Monitor,
    ) -> Result<AudioRun, EngineError> {
        let mut acx = ApplyCx::new(
            mon,
            &self.limits,
            node_seed(node, &cx.source),
            cx.plan.rules,
        );
        if !node.needs.is_empty() {
            let in_fps = node_input_fingerprints(node, &cx.fps.nodes, &cx.input_fps)?;
            for need in &node.needs {
                let a = self.solvable(need)?;
                let (Some(v), Some(fp)) = (ins.get(need.input), in_fps.get(need.input)) else {
                    return Err(EngineError::Internal("解析する入力の値がありません".into()));
                };
                let key = analysis_fingerprint(*fp, need.id, need.v, &need.params)?;
                let out = if let Some(out) = presolved.get(&key) {
                    out.clone()
                } else if v.is_complete() {
                    let acx = AnalyzeCx {
                        mon,
                        limits: &self.limits,
                    };
                    solve_cached(cache, &in_fps, need, v, a.as_ref(), &acx, false)
                        .map_err(|e| e.at(node.id))?
                } else if let Some(out) = cache.get_analysis(&key) {
                    out
                } else {
                    // 入力が部分の値：入力のポートを窓ごとに流して解く（流し解き）
                    let r = node.inputs.get(need.input).ok_or_else(|| {
                        EngineError::Internal("解析する入力のポートがありません".into())
                    })?;
                    let out = self
                        .presolve(cx, cache, r, need, a.as_ref(), presolved, stats, mon)
                        .map_err(|e| e.at(node.id))?;
                    presolved.insert(key, out.clone());
                    out
                };
                acx.preload(need, v, out)?;
            }
        }
        let input0 = match ins.first() {
            Some(Value::Audio(a)) => Some(a.clone()),
            _ => None,
        };
        let base = match base {
            Some(Value::Audio(b)) => Some(b.clone()),
            Some(_) => {
                return Err(EngineError::Internal(
                    "範囲の描画の前の値が音声ではありません".into(),
                ));
            }
            None => None,
        };
        let (mut drawn, mut shared) = (0u64, 0u64);
        let params = node.params.as_ref();
        let (value, map_warnings) =
            apply_node(cx.plan.rules, node, ins, &mut acx, &mut |refs, _, kcx| {
                let k = node.op.audio_kernel(params, refs, kcx).ok_or_else(|| {
                    EngineError::Internal(format!(
                        "{} は音声を出すのに核を返しませんでした",
                        node.op.id()
                    ))
                })??;
                let meta = k.meta().unwrap_or_else(|| {
                    input0
                        .as_ref()
                        .map_or_else(AudioMeta::default, |a| a.meta.clone())
                });
                let frames = usize::try_from(k.frames())
                    .map_err(|_| EngineError::Limit("音声が長すぎます".into()))?;
                let picks = chunks_covering(rounded, frames);
                drawn = picks.count() as u64;
                let (a, s) = crate::audio::render_chunks(
                    k.as_ref(),
                    input0.as_deref(),
                    &picks,
                    base.as_deref(),
                    meta,
                    kcx.mon,
                )?;
                shared = s;
                Ok(Applied::value(Value::Audio(Arc::new(a))))
            })
            .map_err(|e| e.at(node.id))?;
        if acx.analyses_solved_in_place() != 0 {
            return Err(EngineError::Internal(format!(
                "{} が先に解いていない解析を使いました（needs と核の食い違い）",
                node.op.id()
            )));
        }
        Ok(AudioRun {
            value,
            apply_warnings: acx.warnings,
            map_warnings,
            drawn,
            shared,
        })
    }
}

/// 流れの数（[`Engine::stream_audio`] の 1 回の分）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    /// 描いて渡した窓の数。
    pub windows: u64,
    /// 窓の範囲の描画の数の和（流し解きの入れ子を含む）。
    pub area: AreaStats,
}

/// 流れを解析に渡す包み（流し解き）。
struct AnalysisFeed<'a> {
    stream: &'a mut Box<dyn AudioStream>,
    limits: &'a crate::limits::Limits,
}

impl AudioSink for AnalysisFeed<'_> {
    fn begin(&mut self, _info: &StreamInfo) -> Result<(), EngineError> {
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let cx = AnalyzeCx {
            mon,
            limits: self.limits,
        };
        self.stream.feed(start, planes, &cx)
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// チャンネル 1 つだけをダイジェストに足す包み（[`Engine::digest_audio_output`]）。
struct ChannelDigest<'a> {
    digest: &'a mut PcmDigest,
    ch: usize,
    meta: &'a mut Option<AudioMeta>,
}

impl AudioSink for ChannelDigest<'_> {
    fn begin(&mut self, info: &StreamInfo) -> Result<(), EngineError> {
        *self.meta = Some(info.meta.clone());
        Ok(())
    }

    fn feed(
        &mut self,
        start: u64,
        planes: &[&[f32]],
        _mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let p = planes
            .get(self.ch)
            .ok_or_else(|| EngineError::Internal("流れのチャンネルが足りません".into()))?;
        self.digest.push(self.ch, start, p)
    }

    fn finish(&mut self) -> Result<(), EngineError> {
        Ok(())
    }
}

impl Engine {
    /// 申告した解析 `need` を、入力のポート `r` を窓ごとに流して解く（流し解き。入力が部分の値でキャッシュにもないとき）。
    /// 結果はキャッシュに入れる（大きさは解析の見積もり）。
    #[allow(clippy::too_many_arguments)]
    fn presolve(
        &self,
        cx: &PassCx<'_>,
        cache: &mut NodeCache,
        r: &PortRef,
        need: &Need,
        a: &dyn DynAnalysis,
        presolved: &mut Presolved,
        stats: &mut AreaStats,
        mon: &dyn Monitor,
    ) -> Result<AnyOutput, EngineError> {
        let shape = cx
            .shapes
            .port(r)
            .ok_or_else(|| EngineError::Internal("解析する入力の形がありません".into()))?;
        let mut stream = a.stream_any(&need.params, &shape)?.ok_or_else(|| {
            EngineError::Internal(format!(
                "解析 {} は入力を流して解けません（範囲の描画の判定の誤り）",
                need.id
            ))
        })?;
        let mut feed = AnalysisFeed {
            stream: &mut stream,
            limits: &self.limits,
        };
        let mut windows = 0;
        self.stream_port(
            cx,
            cache,
            r,
            &shape,
            None,
            None,
            &mut feed,
            presolved,
            stats,
            &mut windows,
            mon,
        )?;
        let acx = AnalyzeCx {
            mon,
            limits: &self.limits,
        };
        let out = stream.finish(&acx)?;
        let in_fps = match r {
            PortRef::Input { id } => cx.input_fps.get(id).copied(),
            PortRef::Node { id } => cx.fps.nodes.get(id).copied(),
        }
        .ok_or_else(|| EngineError::Internal("解析する入力の指紋がありません".into()))?;
        let key = analysis_fingerprint(in_fps, need.id, need.v, &need.params)?;
        let bytes = a.estimate(&need.params, &shape).map_or(0, |e| e.bytes);
        insert_analysis(cache, key, out.clone(), bytes);
        stats.presolved += 1;
        Ok(out)
    }

    /// ポート `port`（形 `shape`。長さが決まっていること）の値を、`spans` に掛かるチャンク（None は全体）の窓ごとに描いて
    /// `sink` に渡す。窓は `port_pass`（前の値なし）で描き、依存の外を読んだ窓は全体で描き直す。`quantize` があれば窓ごとに
    /// 量子化する（乱数は絶対位置なので全体と同じ）。メタデータは最初の窓（窓がなければ空の範囲の描画）の値のもの。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn stream_port(
        &self,
        cx: &PassCx<'_>,
        cache: &mut NodeCache,
        port: &PortRef,
        shape: &Shape,
        quantize: Option<&Quantize>,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        presolved: &mut Presolved,
        stats: &mut AreaStats,
        windows: &mut u64,
        mon: &dyn Monitor,
    ) -> Result<(), EngineError> {
        let Shape::Audio {
            rate,
            channels,
            frames: Dim::Exact(frames),
        } = *shape
        else {
            return Err(EngineError::Unsupported(
                "長さの分からない音声・音声でない値は流せません".into(),
            ));
        };
        let list = stream_windows(frames, spans);
        let mut begun = false;
        let first = list
            .first()
            .map_or(Area::Empty, |(s, e)| Area::span(*s, *e));
        if list.is_empty() {
            let a = self.window_value(
                cx, cache, port, shape, quantize, first, presolved, stats, mon,
            )?;
            sink.begin(&StreamInfo {
                rate,
                channels: usize::from(channels),
                frames,
                meta: a.meta.clone(),
            })?;
            begun = true;
        }
        for (s, e) in list {
            if mon.canceled() {
                return Err(EngineError::Canceled);
            }
            let a = self.window_value(
                cx,
                cache,
                port,
                shape,
                quantize,
                Area::span(s, e),
                presolved,
                stats,
                mon,
            )?;
            if !begun {
                sink.begin(&StreamInfo {
                    rate,
                    channels: usize::from(channels),
                    frames,
                    meta: a.meta.clone(),
                })?;
                begun = true;
            }
            let planes = a.read_planes(s as usize, e as usize)?;
            drop(a);
            let views: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
            sink.feed(s, &views, mon)?;
            *windows += 1;
        }
        sink.finish()
    }

    /// 流れの窓 1 つ（`area`）を描いた音声（量子化を当てたもの）。
    #[allow(clippy::too_many_arguments)]
    fn window_value(
        &self,
        cx: &PassCx<'_>,
        cache: &mut NodeCache,
        port: &PortRef,
        shape: &Shape,
        quantize: Option<&Quantize>,
        area: Area,
        presolved: &mut Presolved,
        stats: &mut AreaStats,
        mon: &dyn Monitor,
    ) -> Result<Arc<Audio>, EngineError> {
        let frames = match shape {
            Shape::Audio { frames, .. } => frames.max(),
            _ => 0,
        };
        let value = match port {
            PortRef::Input { id } => cx
                .input_values
                .get(id)
                .cloned()
                .ok_or_else(|| EngineError::Internal("流す入力の値がありません".into()))?,
            PortRef::Node { id } => {
                let pass = match self.port_pass(
                    cx,
                    cache,
                    *id,
                    shape,
                    area.clone(),
                    None,
                    false,
                    stats,
                    presolved,
                    mon,
                ) {
                    Ok(p) => p,
                    // 依存の外を読んだ窓は全体で描き直す（遅いだけで正しい）
                    Err(PassError::Hole) => {
                        stats.fallbacks += 1;
                        match self.port_pass(
                            cx,
                            cache,
                            *id,
                            shape,
                            Area::All,
                            None,
                            true,
                            stats,
                            presolved,
                            mon,
                        ) {
                            Ok(p) => p,
                            Err(PassError::Engine(e)) => return Err(e),
                            Err(_) => {
                                return Err(EngineError::Internal(
                                    "流れの窓を全体で描き直せませんでした".into(),
                                ));
                            }
                        }
                    }
                    Err(PassError::Engine(e)) => return Err(e),
                    Err(PassError::Rebase) => {
                        return Err(EngineError::Internal(
                            "前の値なしの流れの窓で前の値の違いを断りました".into(),
                        ));
                    }
                };
                pass.value
            }
        };
        let Value::Audio(a) = value else {
            return Err(EngineError::Internal("流す値が音声ではありません".into()));
        };
        match quantize {
            Some(q) => {
                let usize_frames = usize::try_from(frames).unwrap_or(usize::MAX);
                let only = chunks_covering(&area, usize_frames.min(a.frames()));
                match crate::audio::quantize(&a, q, &cx.source, Some(&only), mon)? {
                    Value::Audio(q) => Ok(q),
                    _ => Err(EngineError::Internal(
                        "量子化した値が音声ではありません".into(),
                    )),
                }
            }
            None => Ok(a),
        }
    }

    /// 計画の出力 `output` を、`spans` に掛かるチャンク（None は全体）の窓（`STREAM_CHUNKS` チャンク）ごとに描いて `sink` に
    /// 渡す（M13 段 1。段 2 の解析・段 3 の保存が乗る口）。窓は頭から順・重ならず、範囲の描画（前の値なし）で描いて渡したら
    /// 手放す（持つ量は窓 1 つ × 生きているノード ＋ 源の LRU。長さによらない）。量子化した出力は窓ごとに量子化する。
    /// メタデータはどの窓でも同じ。値は `render` の出力と同じ（E39）。長さの分からない出力は `Unsupported`。
    #[allow(clippy::too_many_arguments)]
    pub fn stream_audio(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        output: OutputId,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn Monitor,
    ) -> Result<StreamStats, EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        if plan.level != 0 {
            return Err(EngineError::Unsupported(
                "縮小の段の計画は流せません（等倍の計画で描きます）".into(),
            ));
        }
        cache.bind_engine(self.token)?;
        if inputs.len() != plan.inputs.len() {
            return Err(bad_input("入力の数が計画と違います"));
        }
        self.check_inputs(plan, |id| inputs.get(&id).map(|v| &v.value))?;
        let cx = PassCx::new(self, plan, inputs)?;
        let Some((_, from, shape, quantize)) = plan.outputs.iter().find(|o| o.0 == output) else {
            return Err(bad_input(format!("出力 {} はありません", output.0)));
        };
        let mut stats = StreamStats::default();
        let mut presolved = Presolved::new();
        self.stream_port(
            &cx,
            cache,
            from,
            shape,
            quantize.as_ref(),
            spans,
            sink,
            &mut presolved,
            &mut stats.area,
            &mut stats.windows,
            mon,
        )?;
        Ok(stats)
    }

    /// 計画の出力 `output` の値のダイジェスト（`kura-pcm-v1`。`render` の出力の `Value::digest` と同じ）を、出力を流して取る
    /// （チャンネルの数だけ流し、各回はチャンネル c だけを足す。全体を持たない）。段 1 ではテストと段 3 の保存のための口。
    pub fn digest_audio_output(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        output: OutputId,
        mon: &dyn Monitor,
    ) -> Result<Digest, EngineError> {
        let Some((_, _, shape, _)) = plan.outputs.iter().find(|o| o.0 == output) else {
            return Err(bad_input(format!("出力 {} はありません", output.0)));
        };
        let Shape::Audio {
            rate,
            channels,
            frames: Dim::Exact(frames),
        } = *shape
        else {
            return Err(EngineError::Unsupported(
                "長さの分からない音声・音声でない出力は流せません".into(),
            ));
        };
        let mut digest = PcmDigest::new(rate, usize::from(channels), frames);
        let mut meta = None;
        for ch in 0..usize::from(channels) {
            let mut sink = ChannelDigest {
                digest: &mut digest,
                ch,
                meta: &mut meta,
            };
            self.stream_audio(plan, inputs, cache, output, None, &mut sink, mon)?;
        }
        let meta =
            meta.ok_or_else(|| EngineError::Internal("流れのメタデータがありません".into()))?;
        digest.finish(&meta)
    }
}

/// 音声の継ぎはぎで描くチャンクと前の値から共有するチャンク：`keep = valid(base) ∩ need − changed`、`draw = need − keep`
/// （M13 段 1。docs/editing.md §12.5）。音声は頼んだ区間の外の前の値のチャンクを持ち越さない（表示の値は頼んだ区間だけ）。
/// 前の値の長さが今（`frames`）と違えば、両方にあって長さが同じチャンクだけを共有する（最後の半端なチャンクは描き直す）。
pub(super) fn patch_target_audio(
    base: &Audio,
    need: &ChunkSet,
    changed: &ChunkSet,
    frames: usize,
) -> (ChunkSet, ChunkSet) {
    let mut keep = ChunkSet::empty(need.universe());
    let valid = base.valid_chunks();
    for k in need.iter() {
        let len = (frames - k * AUDIO_CHUNK).min(AUDIO_CHUNK);
        if valid.contains(k) && !changed.contains(k) && base.chunk_range(k).len() == len {
            keep.insert(k);
        }
    }
    let mut draw = need.clone();
    draw.subtract(&keep);
    (draw, keep)
}

impl PassCx<'_> {
    /// ポートの値が完全な値になるか（計画の入力か、外したノードを通り抜けた先がキャッシュに当たる有効なノード）。
    pub(super) fn port_complete(&self, cache: &NodeCache, r: &PortRef) -> bool {
        let mut at = *r;
        loop {
            match at {
                PortRef::Input { .. } => return true,
                PortRef::Node { id } => {
                    let Some(n) = self.index.get(&id).map(|i| &self.plan.nodes[*i]) else {
                        return false;
                    };
                    if n.enabled {
                        return self.fps.nodes.get(&id).is_some_and(|fp| cache.contains(fp));
                    }
                    match n.inputs.first() {
                        Some(next) => at = *next,
                        None => return false,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(frames: u64) -> Shape {
        Shape::Audio {
            rate: 8_000,
            channels: 2,
            frames: Dim::Exact(frames),
        }
    }

    #[test]
    fn rounds_to_chunks_and_covers() {
        let c = AUDIO_CHUNK as u64;
        let n = 3 * c + 10;
        let got = round_to_chunks(&Area::span(c - 1, c + 1), &audio(n));
        assert_eq!(got, Area::span(0, 2 * c));
        assert_eq!(round_to_chunks(&Area::All, &audio(n)), Area::span(0, n));
        assert_eq!(
            round_to_chunks(&Area::span(3 * c + 2, n + 50), &audio(n)),
            Area::span(3 * c, n)
        );
        assert!(round_to_chunks(&Area::Empty, &audio(n)).is_empty());
        let set = chunks_covering(&Area::span(c, c + 1), n as usize);
        assert_eq!(set.iter().collect::<Vec<_>>(), vec![1]);
        assert_eq!(chunks_covering(&Area::All, n as usize).count(), 4);
        assert_eq!(
            chunk_spans(&set, n as usize),
            Spans::one(c, 2 * c),
            "チャンクの区間"
        );
        let mut last = ChunkSet::empty(4);
        last.insert(3);
        assert_eq!(chunk_spans(&last, n as usize), Spans::one(3 * c, n));
    }

    /// 範囲に掛かるチャンクの長さの和は、`chunks_covering` の集合の長さの和と同じ（同じチャンクに掛かる隣の区間は 1 度だけ）。
    #[test]
    fn covered_frames_sums_the_covering_chunks() {
        let c = AUDIO_CHUNK as u64;
        let n = 5 * c + 10;
        let sum =
            |set: &ChunkSet| -> u64 { set.iter().map(|k| (n - k as u64 * c).min(c)).sum::<u64>() };
        let mut two = Spans::one(3, 4);
        two.push(c - 1, c + 1);
        two.push(c + 5, c + 6);
        two.push(5 * c + 2, n + 100);
        for area in [
            Area::Empty,
            Area::All,
            Area::span(0, 1),
            Area::span(c, 2 * c),
            Area::span(4 * c + 1, n),
            Area::samples(two),
            Area::span(n + 1, n + 5),
        ] {
            assert_eq!(
                covered_frames(&area, n),
                sum(&chunks_covering(&area, n as usize)),
                "{area:?}"
            );
        }
    }
}
