//! 音声の範囲の描画（M13 段 1。docs/editing.md §12.5）：`render_area`（area.rs）の音声の部品。
//!
//! - 音声の範囲を持つノード（[`Engine::audio_regional`]）：有効で、出力と入力 0 が音声で、`Op::audio_reach`（核が入力 0 の
//!   どこを読むか）が `Global` でないもの。申告した解析（normalize のラウドネス）は先に解く（キャッシュ・入力が完全な値なら
//!   そのまま当てる）。
//! - 要る区間は出力のチャンク（`AUDIO_CHUNK`）に揃え（[`round_to_chunks`]）、`audio_reach` で入力 0 へ写す。前から描くときは、
//!   揃えた区間に掛かるチャンクだけを核で描き（`audio::render_chunks`。ずらした写しは入力の実体を共有）、ほかのチャンクは
//!   前の値のもの（継ぎはぎ）か無効にする。
//! - 部分の値はキャッシュに入れない。全体を描いたノードは `render_cached` の外れの道と同じ（由来 Exact で入れる）。

use std::sync::Arc;

use super::area::{PassCx, Presolved};
use super::cache::{NodeCache, analysis_fingerprint, node_input_fingerprints, solve_cached};
use super::region::{Area, Reach, Spans};
use super::{Engine, PlannedNode, apply_node, node_seed, op_shapes};
use crate::analysis::AnalyzeCx;
use crate::doc::PortRef;
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
    /// ノード）か）。解けなければ、そのノードは全体で描く。
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
            if !(cx.port_complete(cache, r) || cache.contains_analysis(&key)) {
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
                    return Err(EngineError::Internal(format!(
                        "{} の解析 {} を、範囲の描画の前に解けませんでした",
                        node.op.id(),
                        need.id
                    )));
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

/// 音声の継ぎはぎで描くチャンクと前の値から共有するチャンク：`keep = valid(base) ∩ need − changed`、`draw = need − keep`
/// （M13 段 1。docs/editing.md §12.5）。音声は頼んだ区間の外の前の値のチャンクを持ち越さない（表示の値は頼んだ区間だけ）。
pub(super) fn patch_target_audio(
    base: &Audio,
    need: &ChunkSet,
    changed: &ChunkSet,
) -> (ChunkSet, ChunkSet) {
    let mut keep = base.valid_chunks();
    keep.intersect_with(need);
    keep.subtract(changed);
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
}
