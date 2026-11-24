//! 内部のポート（ノードの入力 0・出力）を区間で流す口と、その指紋（M17 段 4。docs/editing.md §18.4）。
//!
//! スペクトルの「前と後」（イコライザのノードの入力・出力）のように、計画の出力でないポートを窓ごとに流す。窓の描き方は
//! 出力の流れ（[`Engine::stream_audio`]）と同じ `stream_port`（範囲の描画を前の値なしで。依存の外を読んだ窓は全体で描き直す）
//! で、値はそのノードまでの文書を `render` した値と同じ。量子化はかけない（出力の量子化は出力だけのもの）。
//!
//! - ポートに届く申告した解析がキャッシュにないときは、流さずに `Unsupported` で断る（窓を描くと描画の中の流し解きで入力の
//!   全体を流し直すため。錠の中で全体を解かない）。呼び手は先に [`Engine::missing_port_solves`] の仕事を
//!   `PresolveJob`（`render::presolve`）で解いてから流す。判定は `missing_solves` と同じ走査（`missing_from`）。

use std::collections::BTreeMap;

use super::area::{PassCx, Presolved};
use super::audio::StreamStats;
use super::cache::{Fingerprint, FingerprintedInput, NodeCache};
use super::region::Spans;
use super::{Engine, Plan, bad_input, lookup};
use crate::audio::stream::AudioSink;
use crate::doc::{InputId, NodeId, PortRef};
use crate::error::EngineError;
use crate::monitor::Monitor;
use crate::shape::{Dim, Shape};

/// ノードのどちらのポートか。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PortSide {
    /// 入力 0（ノードに入る値）。
    Input,
    /// 出力（ノードの指紋の値。外したノードは入力 0 と同じ値・同じ指紋）。
    Output,
}

impl PortSide {
    fn name(self) -> &'static str {
        match self {
            Self::Input => "入力",
            Self::Output => "出力",
        }
    }
}

/// ノード `node` の `side` のポート（計画にないノード・入力のないノードの入力は `BadInput`）。
pub(super) fn node_port(plan: &Plan, node: NodeId, side: PortSide) -> Result<PortRef, EngineError> {
    let Some(n) = plan.nodes.iter().find(|n| n.id == node) else {
        return Err(bad_input(format!("ノード {} はありません", node.0)));
    };
    match side {
        PortSide::Output => Ok(PortRef::Node { id: node }),
        PortSide::Input => n
            .inputs
            .first()
            .copied()
            .ok_or_else(|| bad_input(format!("ノード {} に入力がありません", node.0))),
    }
}

impl Plan {
    /// ノード `node` の `side` のポート（入力 0 はそのノードの最初の入力。計画にないノード・入力のないノードの入力は
    /// `BadInput`）。見積もりの `EstimateSpec::port_area` に渡す。
    pub fn node_port(&self, node: NodeId, side: PortSide) -> Result<PortRef, EngineError> {
        node_port(self, node, side)
    }
}

impl Engine {
    /// ノード `node` の `side` のポートの指紋（[`Engine::fingerprints`] の `nodes` の値。入力 0 は、その先のノードの指紋か計画の
    /// 入力の指紋）。同じ指紋のポートは同じ値（外したノードの出力は入力と同じ指紋）。
    pub fn port_fingerprint(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        node: NodeId,
        side: PortSide,
    ) -> Result<Fingerprint, EngineError> {
        // ほかの口（`stream_node_port`・`missing_port_solves`）と同じく、別のエンジンの計画・入力の数の違いを断る（指紋は
        // 登録表に縛られないので、別の登録表の指紋を鍵に使わない）
        self.check_solve_plan(plan, inputs)?;
        let port = node_port(plan, node, side)?;
        let fps = self.fingerprints(plan, inputs)?;
        let input_fps: BTreeMap<InputId, Fingerprint> =
            inputs.iter().map(|(k, v)| (*k, v.fingerprint)).collect();
        lookup(&fps.nodes, &input_fps, &port).ok_or_else(|| bad_input("ポートの指紋がありません"))
    }

    /// ノード `node` の `side` のポート（入力 0・出力）の値を、`spans` に掛かるチャンク（None は全体）の窓ごとに描いて `sink` に
    /// 渡す（M17 段 4。スペクトルの「前と後」）。確かめ・窓の描き方は [`Engine::stream_audio`] と同じで、値はそのノードまでの
    /// 文書を `render` した値と同じ。量子化はかけない。ポートに届く申告した解析がキャッシュにない（窓を描くと入力の全体を
    /// 流し直す）ときは流さずに `Unsupported`（先に [`Engine::missing_port_solves`] の仕事を解く）。長さの分からない音声・
    /// 音声でないポート・縮小の段の計画も `Unsupported`。
    #[allow(clippy::too_many_arguments)]
    pub fn stream_node_port(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        node: NodeId,
        side: PortSide,
        spans: Option<&Spans>,
        sink: &mut dyn AudioSink,
        mon: &dyn Monitor,
    ) -> Result<StreamStats, EngineError> {
        self.check_stream_plan(plan, inputs, cache)?;
        let port = node_port(plan, node, side)?;
        let cx = PassCx::new(self, plan, inputs)?;
        let shape = cx
            .shapes
            .port(&port)
            .ok_or_else(|| EngineError::Internal("流すポートの形がありません".into()))?;
        if !matches!(
            shape,
            Shape::Audio {
                frames: Dim::Exact(_),
                ..
            }
        ) {
            return Err(EngineError::Unsupported(
                "長さの分からない音声・音声でない値は流せません".into(),
            ));
        }
        let missing = self.missing_from(&cx, cache, port)?;
        if missing.unstreamable || !missing.tasks.is_empty() {
            return Err(EngineError::Unsupported(format!(
                "ノード {} の{}を描くのに要る解析がまだ解けていません（先に流し解きの仕事で解きます）",
                node.0,
                side.name()
            )));
        }
        // 要る結果をこの流れの表に入れる（窓を描く間に全体で描いたノードがキャッシュに入って追い出しても、窓の中で解き
        // 直さない。`PresolveJob::seed` と同じ）
        let mut presolved = Presolved::new();
        for (key, _) in &missing.needed {
            if let Some(out) = cache.peek_analysis(key) {
                presolved.insert(*key, out);
            }
        }
        let mut stats = StreamStats::default();
        self.stream_port(
            &cx,
            cache,
            &port,
            &shape,
            None,
            None,
            spans,
            sink,
            &mut presolved,
            &mut stats.area,
            &mut stats.windows,
            None,
            mon,
        )?;
        Ok(stats)
    }
}
