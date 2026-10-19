//! エディタの表示の経路（docs/editing.md §12。段 10）：[`Engine::render_view`]。
//!
//! 1 手ごとに、前回の表示（[`View`]）の計画から今の計画への変わりうる範囲を推論し（`Engine::delta`）、`Engine::render_area` で
//! 表示する範囲を描く。前の値は前回の表示の値、変わりうる範囲は推論で、前の値のタイルのうち推論に掛からない有効なものは共有し、
//! ほかのタイルだけを描き直す（継ぎはぎ）。最後に前回の表示の値とタイルの実体（`Arc`）を比べて、変わったタイルの一覧を作る
//! （`delta::refine`。フロントはこのタイルだけを取り直す。docs/editing.md §12.8・§12.13）。
//!
//! - 値：表示する範囲のタイルは、`render` の同じタイルとビット単位で同じ（範囲の描画と継ぎはぎの約束。推論は保守的）。
//! - 範囲の描画を持たないノード（描画・塗りつぶし・trim など）は、`render_cached` の外れの道と同じに描く（キャッシュの当たり・
//!   差分の適用・解析のキャッシュ・由来 Exact で入れる）。筆を足すと、描画のノードは前の出力に足した分だけを描き、後ろの
//!   範囲を持つノード（最近傍・canvas・convert など）は変わったタイルだけを描く。等倍の全体の値は作らない。
//! - 表示の値は配信にだけ使う（由来 Patched・Partial・Scaled。継ぎはぎをしなかった全体の描画だけ Exact）。保存・一括・解析・
//!   スポイト・ダイジェストは `render`・`render_cached`・`spoit` で。
//! - 計画は等倍のほか、プレビューの段の計画（`Engine::plan_level`）でもよい。段・出力・エンジン・元が前回と違えば全体を描く。
//! - 変わったタイルの一覧は、中身の変わったタイルを含み（実体を共有していないタイルと、前回は無効だったタイルを数える）、
//!   推論の範囲と「前回は無効で今回描いたタイル」（表示する範囲を広げた・動かした）の中にある。
//! - 配信に要るもの（パレット・メタデータ・フレームの表示時間・形式）は `View::value()` の `PartialValue` の口で取る。

use std::collections::BTreeMap;

use super::area::{AreaRequest, AreaStats};
use super::cache::{CacheStats, Fingerprint, FingerprintedInput, NodeCache};
use super::delta::{OutputChange, infer, refine, same_meta, stamps};
use super::region::{Area, Base, PartialValue, frames_of};
use super::{Engine, Plan};
use crate::digest::Digest;
use crate::doc::{InputId, NodeId, OutputId};
use crate::error::{EngineError, Issue};
use crate::monitor::Monitor;
use crate::value::Value;

/// 表示の要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRequest {
    /// 表示する出力。
    pub output: OutputId,
    /// 表示する範囲（キャンバスの座標。全体は `Area::All`）。
    pub area: Area,
    /// 編集中のノード（その入力と出力をキャッシュに固定する。`RenderOptions::focus` と同じ）。
    pub focus: Option<NodeId>,
}

impl ViewRequest {
    /// 出力 `output` の全体を表示する要求。
    pub fn whole(output: OutputId, focus: Option<NodeId>) -> Self {
        Self {
            output,
            area: Area::All,
            focus,
        }
    }
}

/// 表示の状態（前回の表示。次の [`Engine::render_view`] に渡す）。計画と値を共有して持ち（値は `Arc` のタイル）、入力は
/// 指紋と元だけを持つ（入力の値を保たない。`render_cached` の前回と同じ）。
#[derive(Clone)]
pub struct View {
    output: OutputId,
    plan: Plan,
    /// 入力の（指紋, 元）。推論（`delta::infer`）に使う。
    inputs: BTreeMap<InputId, (Fingerprint, Digest)>,
    value: PartialValue,
}

impl View {
    /// 表示した出力。
    pub fn output(&self) -> OutputId {
        self.output
    }

    /// 表示の値（表示する範囲のタイルが有効。配信にだけ使う）。
    pub fn value(&self) -> &PartialValue {
        &self.value
    }

    /// 描いた計画。
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    /// 解像度の段（0 は等倍）。
    pub fn level(&self) -> u8 {
        self.plan.level()
    }
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("View")
            .field("output", &self.output)
            .field("level", &self.plan.level())
            .field("origin", &self.value.origin())
            .finish()
    }
}

/// 表示の結果。
#[derive(Debug, Clone)]
pub struct ViewRender {
    /// 今回の表示（次の `render_view` の前回）。値は `view.value()`。
    pub view: View,
    /// 前回の表示から変わったタイル（1 辺 `VIEW_TILE`。前回がなければ `All`）。
    pub changed: OutputChange,
    /// メタデータ・フレームの表示時間・繰り返しが変わったら true（前回がなければ true）。
    pub meta_changed: bool,
    /// 推論した変わりうる範囲（前回がなければ `All`）。
    pub inferred: Area,
    /// 描いたノード・当たったノードの警告（`render_area` と同じ。全体の警告は `render_cached` が正）。
    pub warnings: Vec<Issue>,
    /// キャッシュの数（この描画の分）。
    pub stats: CacheStats,
    /// 範囲の描画の数（この描画の分）。
    pub area: AreaStats,
}

impl ViewRender {
    /// 解像度の段（0 は等倍）。
    pub fn level(&self) -> u8 {
        self.view.level()
    }

    /// 描いたノードの数（範囲だけを描いたもの ＋ 全体を描いたもの。当たり・描かなかったものは数えない）。
    pub fn drawn_nodes(&self) -> u64 {
        self.area.region_nodes + self.area.full_nodes
    }
}

impl Engine {
    /// エディタの表示（モジュールの説明）：`prev`（前回の表示。同じ出力のときだけ使う）から変わりうる範囲を推論し、`req` の
    /// 範囲を、前回の表示の値に変わりうるタイルだけを描き直して継ぎはぎする。返す値は表示にだけ使う。描けなければ前回の表示は
    /// そのまま使える（キャッシュには描き終えたノードの値が残る）。別のエンジンの計画・キャッシュは `Internal`。
    pub fn render_view(
        &self,
        plan: &Plan,
        inputs: &BTreeMap<InputId, FingerprintedInput>,
        cache: &mut NodeCache,
        req: &ViewRequest,
        prev: Option<&View>,
        mon: &dyn Monitor,
    ) -> Result<ViewRender, EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        cache.bind_engine(self.token)?;
        let prev = prev.filter(|p| p.output == req.output);
        let now = stamps(inputs);
        // 推論が誤りになる計画（申告が引数を読めないなど）は、全体が変わったとみなす（保守的）
        let inferred = prev
            .and_then(|p| infer(&p.plan, &p.inputs, plan, &now).ok())
            .and_then(|d| {
                d.outputs
                    .into_iter()
                    .find(|(id, _)| *id == req.output)
                    .map(|(_, a)| a)
            })
            .unwrap_or(Area::All);
        let fps = self.fingerprints(plan, inputs)?;
        let input_fps: BTreeMap<InputId, Fingerprint> =
            inputs.iter().map(|(k, v)| (*k, v.fingerprint)).collect();
        let focus = cache.pin_before(plan, &fps, &input_fps, req.focus);
        let before = cache.stats();
        let base = prev.map(|p| Base {
            value: &p.value,
            changed: &inferred,
        });
        let area_req = AreaRequest {
            output: req.output,
            area: req.area.clone(),
        };
        let result = self.render_area(plan, inputs, cache, &area_req, base.as_ref(), mon);
        cache.unpin_after(plan, focus);
        let r = result?;
        let (changed, meta_changed) = match prev {
            Some(p) => {
                let (a, b) = (p.value.value(), r.value.value());
                let guess = inferred.union(&newly_valid(a, b));
                (refine(a, b, &guess), !same_meta(a, b))
            }
            None => (OutputChange::All, true),
        };
        Ok(ViewRender {
            view: View {
                output: req.output,
                plan: plan.clone(),
                inputs: now,
                value: r.value,
            },
            changed,
            meta_changed,
            inferred,
            warnings: r.warnings,
            stats: cache.stats().since(&before),
            area: r.stats,
        })
    }
}

/// 前回の表示 `prev` では無効で、今回の表示 `next` で有効なタイル（キャンバスの座標。表示する範囲を広げた・動かした分）。
/// フレームの形・位置が違えば `refine` が全体にするので、ここではフレームの全体を返す。
fn newly_valid(prev: &Value, next: &Value) -> Area {
    let before = frames_of(prev);
    let mut out = BTreeMap::new();
    for (f, px, origin) in frames_of(next) {
        let mut tiles = px.valid_tiles();
        let old = before
            .iter()
            .find(|(i, _, o)| *i == f && *o == origin)
            .map(|(_, p, _)| p.valid_tiles());
        if let Some(old) = old
            && tiles.subtract(&old).is_err()
        {
            tiles = px.valid_tiles();
        }
        if !tiles.is_empty() {
            out.insert(f, tiles.to_rects(origin));
        }
    }
    Area::pixels(out)
}
