//! プレビューの段のテスト・ファズの補助（段 10。E32）：等倍の計画から段 1〜3 の計画を作って描き、段の計画が作れること
//! （作れなければ段 0 に落ちること）、等倍で描けるなら段でも描けること、キャッシュの道と範囲の描画で同じ値になること、
//! 継ぎ目までが等倍の計画と同じ指紋であることを照らす。
//!
//! テスト用の補助だが本番のビルドにも入る（`pub mod testing`）ので、ここでも panic しない。

use std::collections::BTreeMap;

use crate::doc::InputId;
use crate::error::EngineError;
use crate::monitor::NoMonitor;
use crate::render::area::AreaRequest;
use crate::render::cache::{FingerprintedInput, NodeCache, RenderOptions};
use crate::render::preview::{LevelFallback, MAX_LEVEL};
use crate::render::region::Area;
use crate::render::{Engine, Plan};
use crate::value::Value;

/// 段の計画の照合の数（テストが、どの道も通ったかを数える）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LevelCounts {
    /// 段 1〜3 で描いた数。
    pub drawn: u64,
    /// 段 1〜3 の計画を作ったが、描けなかった数（等倍でも描けなかったもの）。
    pub failed: u64,
    /// 段 0 に落ちた数（理由ごと）。
    pub fallbacks: BTreeMap<&'static str, u64>,
}

impl LevelCounts {
    /// 足す。
    pub fn add(&mut self, other: &LevelCounts) {
        self.drawn += other.drawn;
        self.failed += other.failed;
        for (k, v) in &other.fallbacks {
            *self.fallbacks.entry(k).or_default() += v;
        }
    }
}

/// 段 0 に落ちた理由の名前（数えるため）。
pub fn fallback_kind(f: &LevelFallback) -> &'static str {
    match f {
        LevelFallback::NotLinear => "not-linear",
        LevelFallback::NotImage(_) => "not-image",
        LevelFallback::Indexed(_) => "indexed",
        LevelFallback::Analysis(_) => "analysis",
        LevelFallback::Infer { .. } => "infer",
    }
}

fn internal(m: String) -> EngineError {
    EngineError::Internal(m)
}

/// 等倍の計画 `plan`（入力 `inputs`）から段 1〜3 の計画を作って描き、次を確かめる（破れは `Internal`）：
/// - 計画が作れる。落ちなければ段は k、落ちたら段 0 の計画。継ぎ目までのノードは等倍の計画と同じ指紋。
/// - 等倍で描けたなら（`full_ok`）、段 k でも描ける（段 k の推論が通ったなら、実行でも通る）。
/// - 冷えたキャッシュ・温まったキャッシュ（当たりを描き直して照らす）で同じ値、範囲の描画（全体と、`seed` で選んだ範囲）の
///   有効なタイルも同じ値。段の値はダイジェストを取らない描画でだけ描ける。
pub fn check_levels(
    engine: &Engine,
    plan: &Plan,
    inputs: &BTreeMap<InputId, FingerprintedInput>,
    full_ok: bool,
    seed: u64,
) -> Result<LevelCounts, EngineError> {
    let mut counts = LevelCounts::default();
    let full_fps = engine.fingerprints(plan, inputs)?;
    for level in 1..=MAX_LEVEL {
        let lp = engine.plan_level(plan, level)?;
        if let Some(f) = &lp.fallback {
            if lp.plan.level() != 0 {
                return Err(internal(format!(
                    "段 {level} の計画が段 0 に落ちた（{f:?}）のに、段が {} です",
                    lp.plan.level()
                )));
            }
            *counts.fallbacks.entry(fallback_kind(f)).or_default() += 1;
            continue;
        }
        if lp.plan.level() != level {
            return Err(internal(format!(
                "段 {level} の計画の段が {} です",
                lp.plan.level()
            )));
        }
        // 継ぎ目までは等倍の計画と同じノード・同じ指紋
        let fps = engine.fingerprints(&lp.plan, inputs)?;
        if let Some(seam) = lp.seam {
            let mut ids = Vec::new();
            for id in plan.node_ids() {
                ids.push(id);
                if id == seam {
                    break;
                }
            }
            for id in ids {
                if fps.nodes.get(&id) != full_fps.nodes.get(&id) || !lp.plan.shares_node(plan, id) {
                    return Err(internal(format!(
                        "段 {level} の計画の継ぎ目までのノード {} が等倍の計画と違います",
                        id.0
                    )));
                }
            }
        }
        let opts = RenderOptions::default();
        let verify = RenderOptions {
            verify_hits: true,
            ..opts
        };
        let mut cache = NodeCache::new(1 << 30);
        let cold = engine.render_cached(&lp.plan, inputs, &mut cache, &opts, &NoMonitor);
        let cold = match cold {
            Ok(c) => c,
            Err(EngineError::Internal(m)) => {
                return Err(internal(format!("段 {level} の描画：{m}")));
            }
            Err(e) if full_ok => {
                return Err(internal(format!(
                    "等倍では描けるのに、段 {level} では描けません（{e}）"
                )));
            }
            Err(_) => {
                counts.failed += 1;
                continue;
            }
        };
        if cold.outputs.iter().any(|o| o.digest.is_some()) {
            return Err(internal(format!(
                "段 {level} の描画がダイジェストを返しました"
            )));
        }
        let warm = engine
            .render_cached(&lp.plan, inputs, &mut cache, &verify, &NoMonitor)
            .map_err(|e| internal(format!("段 {level} の温まった描画：{e}")))?;
        let digests = |vs: Vec<&Value>| -> Result<Vec<_>, EngineError> {
            vs.into_iter().map(Value::digest).collect()
        };
        let a = digests(cold.outputs.iter().map(|o| &o.value).collect())?;
        let b = digests(warm.outputs.iter().map(|o| &o.value).collect())?;
        if a != b {
            return Err(internal(format!(
                "段 {level} の冷えた描画と温まった描画が違います"
            )));
        }
        // 範囲の描画（全体と、選んだ範囲）：有効なタイルは全体の描画と同じ
        let Some(out) = cold.outputs.first() else {
            continue;
        };
        let mut rng = crate::determinism::rng_stream(seed, u64::from(level));
        let area = super::region::random_area(&mut rng, &out.value);
        for (what, area) in [("全体", Area::All), ("選んだ範囲", area)] {
            let req = AreaRequest {
                output: out.id,
                area: area.clone(),
            };
            let mut fresh = NodeCache::new(1 << 30);
            let r = engine
                .render_area(&lp.plan, inputs, &mut fresh, &req, None, &NoMonitor)
                .map_err(|e| internal(format!("段 {level} の範囲の描画（{what}）：{e}")))?;
            if r.value.exact().is_some() {
                return Err(internal(format!(
                    "段 {level} の範囲の描画（{what}）の由来が等倍の値になっています"
                )));
            }
            super::region::check_same_inside(r.value.value(), &out.value, &area)
                .map_err(|m| internal(format!("段 {level} の範囲の描画（{what}）：{m}")))?;
        }
        counts.drawn += 1;
    }
    Ok(counts)
}
