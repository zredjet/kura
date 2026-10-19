//! 文書のファズの、セッションの道の照合（段 10）：`EditSession` → `Engine::compile_snapshot` の計画が `compile` と同じか。
//! 追記だけの配列を持つノードは、前半で開いて後半を追記し、差分の計画と差分の適用（`render_cached`）が全体と同じかも照らす。

use std::collections::BTreeMap;

use crate::digest::Digest;
use crate::doc::{Graph, InputId};
use crate::error::{EngineError, Issues};
use crate::monitor::NoMonitor;
use crate::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use crate::render::{Engine, InputValue, Plan};
use crate::session::{CommitMode, Edit, EditSession, HistoryLimits};
use crate::testgen::InputGen;

/// 計画の照らす中身（各ノードの推論・申告・鍵、出力の形、最後の使い手、手間）が同じか。違えば何が違うかの文言。
fn plan_difference(a: &Plan, b: &Plan) -> Option<String> {
    let ids = a.node_ids();
    if ids != b.node_ids() {
        return Some("ノードの並び".into());
    }
    for (i, id) in ids.iter().enumerate() {
        if a.node_inferred(*id) != b.node_inferred(*id) {
            return Some(format!("ノード {} の推論", id.0));
        }
        if a.node_needs(*id) != b.node_needs(*id) {
            return Some(format!("ノード {} の申告", id.0));
        }
        if a.node_key(*id) != b.node_key(*id) {
            return Some(format!("ノード {} の鍵", id.0));
        }
        if a.released_after(i) != b.released_after(i) {
            return Some(format!("ノード {} の最後の使い手", id.0));
        }
    }
    if a.output_shapes() != b.output_shapes() || a.cost() != b.cost() {
        return Some("出力の形か手間".into());
    }
    None
}

/// 差分の計画と全体の計画が同じか（作れたか・指摘・中身）。違えば `Internal`。
fn same_plans(
    what: &str,
    snap: &Result<Plan, Issues>,
    full: &Result<Plan, Issues>,
) -> Result<(), EngineError> {
    let why = match (snap, full) {
        (Ok(a), Ok(b)) => plan_difference(a, b),
        (Err(a), Err(b)) if a == b => None,
        (Err(_), Err(_)) => Some("指摘".into()),
        _ => Some("作れたかどうか".into()),
    };
    match why {
        None => Ok(()),
        Some(why) => Err(EngineError::Internal(format!(
            "セッションの道の計画（{what}）が compile と違います：{why}"
        ))),
    }
}

/// セッションの道の照合。開けない文書（形の誤りなど）は照らさない。
pub(crate) fn check_snapshot(
    engine: &Engine,
    g: &Graph,
    inputs: &[InputGen],
) -> Result<(), EngineError> {
    let Ok(session) = EditSession::open(engine, g, Digest::default(), HistoryLimits::default())
    else {
        return Ok(());
    };
    let mut values = Vec::with_capacity(inputs.len());
    for input in inputs {
        let value = input.generate()?;
        let shapes = BTreeMap::from([(InputId(0), value.shape())]);
        same_plans(
            "開いたまま",
            &engine.compile_snapshot(session.current(), &shapes, None),
            &engine.compile(g, &shapes),
        )?;
        values.push(value);
    }
    for (k, node) in g.nodes.iter().enumerate() {
        let Some(field) = engine.registry().append_field(&node.op, node.v) else {
            continue;
        };
        let Some(items) = node.params.get(field).and_then(|a| a.as_array()) else {
            continue;
        };
        if items.len() < 2 {
            continue;
        }
        let (front, back) = items.split_at(items.len() / 2);
        let mut head = g.clone();
        if let Some(p) = head.nodes.get_mut(k).and_then(|n| n.params.get_mut(field)) {
            *p = serde_json::Value::Array(front.to_vec());
        }
        for value in &values {
            let shapes = BTreeMap::from([(InputId(0), value.shape())]);
            let Ok(mut s) =
                EditSession::open(engine, &head, Digest::default(), HistoryLimits::default())
            else {
                return Ok(());
            };
            let Ok(first) = engine.compile_snapshot(s.current(), &shapes, None) else {
                continue;
            };
            // 手間の大きな計画は描かない（1 回を速くする）
            if first.cost() > 1 << 22 {
                continue;
            }
            let fp = BTreeMap::from([(
                InputId(0),
                FingerprintedInput::new(value.clone(), Digest::default())?,
            )]);
            let opts = RenderOptions {
                digests: true,
                focus: Some(node.id),
                verify_hits: false,
            };
            let mut cache = NodeCache::new(1 << 30);
            let before = engine.render_cached(&first, &fp, &mut cache, &opts, &NoMonitor);
            let edit = Edit::AppendItems {
                node: node.id,
                field: field.to_string(),
                items: back.to_vec(),
            };
            if s.commit(engine, edit, CommitMode::Step).is_err() {
                continue;
            }
            let full = engine.compile(g, &shapes);
            let next = engine.compile_snapshot(s.current(), &shapes, Some(&first));
            same_plans("追記", &next, &full)?;
            let (Ok(next), Ok(full), Ok(_)) = (next, full, before) else {
                continue;
            };
            if full.cost() > 1 << 22 {
                continue;
            }
            let plain = BTreeMap::from([(
                InputId(0),
                InputValue {
                    value: value.clone(),
                    source: Digest::default(),
                },
            )]);
            let want = engine.render(&full, &plain, &NoMonitor);
            let got = engine.render_cached(&next, &fp, &mut cache, &opts, &NoMonitor);
            let same = match (&want, &got) {
                (Ok(w), Ok(c)) => {
                    w.warnings == c.warnings
                        && w.outputs
                            .iter()
                            .zip(&c.outputs)
                            .all(|(a, b)| Some(a.digest) == b.digest)
                }
                (Err(_), Err(EngineError::Internal(_))) => false,
                (Err(_), Err(_)) => true,
                _ => false,
            };
            if !same {
                return Err(EngineError::Internal(format!(
                    "{}：差分の計画と差分の適用で描いた値が、全体を描いたものと違います（全体 {}・差分 {}）",
                    node.op,
                    want.is_ok(),
                    got.is_ok()
                )));
            }
        }
    }
    Ok(())
}
