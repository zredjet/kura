//! エディタの表示の決め方と、フロントへ返す形（純粋な関数。docs/editing.md §12.7・§12.8・§12.13）。
//!
//! - 段（[`view_level`]）：Rust が `choose_level` で決める。要る装置の画素は「等倍の大きさ × 1 画素あたりの装置の画素」。
//!   操作中（ドラッグ・ズームの途中）は 1 段粗く（その形で許される最も粗い段まで）、止まったら細かい段。ドット絵（DB の実効値）・
//!   Indexed8・フレームの列は `choose_level` が段 0 にする。
//! - 範囲（[`view_area`]）：ビューポートの矩形（等倍のキャンバスの座標）を段の座標に直す（始まりは床・終わりは天井）。矩形が
//!   なければ全体（フレームの列は見ているフレームの全体）。
//! - 文書の差分（[`doc_delta`]）：前にフロントへ返した文書から変わったノードだけを返す（追記の配列は数だけ。
//!   `ParamsView::Log`）。
//! - 表示の情報（[`view_info`]）：段・大きさ・フレームの矩形・変わったタイル（1 辺 [`VIEW_TILE`]。フレームの画像の座標）。

use kura_engine::doc::{NodeId, OutputSpec};
use kura_engine::geom::{IRect, Size};
use kura_engine::render::delta::OutputChange;
use kura_engine::render::preview::{LevelFallback, LevelHint, MAX_LEVEL, choose_level};
use kura_engine::render::region::{Area, PartialValue, VIEW_TILE};
use kura_engine::session::{NodeChange, ParamsView, Snapshot};
use kura_engine::{Dim, Issue, Shape};
use serde_json::Value as Json;

/// 表示の要求（ビューポート）。座標はいつも等倍のキャンバスの座標。
#[derive(Debug, Clone, PartialEq)]
pub struct Viewport {
    /// 表示するフレーム（1 枚の画像は 0）。
    pub frame: u32,
    /// 表示する範囲（等倍のキャンバスの座標）。None は全体。
    pub rect: Option<IRect>,
    /// 等倍の 1 画素あたりの装置の画素（拡大率 × devicePixelRatio）。0 以下・有限でなければ 1。
    pub scale: f64,
    /// 操作中（ドラッグ・ズームの途中）なら 1 段粗く描く。
    pub interacting: bool,
    /// 編集中のノード（その入力と出力をキャッシュに固定する）。
    pub focus: Option<NodeId>,
}

impl Viewport {
    /// 全体を等倍で表示する要求（テスト・最初の表示）。
    pub fn whole() -> Self {
        Self {
            frame: 0,
            rect: None,
            scale: 1.0,
            interacting: false,
            focus: None,
        }
    }
}

/// 形が 1 枚の画像で大きさが決まっていれば、その大きさ。
fn exact_image(full: &Shape) -> Option<(u64, u64)> {
    match *full {
        Shape::Image {
            w: Dim::Exact(w),
            h: Dim::Exact(h),
            ..
        } => Some((w, h)),
        _ => None,
    }
}

/// 表示する段（0〜[`MAX_LEVEL`]）：`full` は等倍の出力の形。各軸が「等倍の大きさ × `scale`」を下回らない最も粗い段
/// （`choose_level`）。操作中は 1 段粗く（その形で許される最も粗い段まで）。ドット絵・Indexed8・フレームの列・音声は 0。
pub fn view_level(full: &Shape, scale: f64, interacting: bool, pixel_art: bool) -> u8 {
    let hint = LevelHint { pixel_art };
    let Some((w, h)) = exact_image(full) else {
        return 0;
    };
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    // 要る装置の画素（1 以上、u32 で頭打ち）
    let need = |n: u64| -> u32 {
        let v = (n as f64 * s).ceil();
        if v >= f64::from(u32::MAX) {
            u32::MAX
        } else if v < 1.0 {
            1
        } else {
            v as u32
        }
    };
    let fine = choose_level(
        full,
        Size {
            w: need(w),
            h: need(h),
        },
        hint,
    );
    if !interacting {
        return fine;
    }
    let coarsest = choose_level(full, Size { w: 1, h: 1 }, hint);
    fine.saturating_add(1).min(coarsest).min(MAX_LEVEL)
}

/// `v / 2^level` の床（負の座標も床）。
fn floor_shift(v: i64, level: u8) -> i64 {
    v >> u32::from(level.min(62))
}

/// `v / 2^level` の天井。
fn ceil_shift(v: i64, level: u8) -> i64 {
    let k = u32::from(level.min(62));
    let d = 1i64 << k;
    // 天井 = 床((v + d − 1) / d)。あふれは頭打ち
    v.saturating_add(d - 1) >> k
}

/// 表示する範囲（段 `level` のキャンバスの座標）：ビューポートの矩形を段に直す（始まりは床・終わりは天井）。矩形が
/// なければ全体（フレームの列は見ているフレームの全体）。
pub fn view_area(full: &Shape, vp: &Viewport, level: u8) -> Area {
    let Some(r) = vp.rect else {
        return match full {
            Shape::Frames { .. } => Area::whole_frame(vp.frame),
            _ => Area::All,
        };
    };
    let x0 = floor_shift(r.x, level);
    let y0 = floor_shift(r.y, level);
    let x1 = ceil_shift(r.x.saturating_add(i64::from(r.w)), level);
    let y1 = ceil_shift(r.y.saturating_add(i64::from(r.h)), level);
    let len = |a: i64, b: i64| u32::try_from(b.saturating_sub(a).max(0)).unwrap_or(u32::MAX);
    Area::rect(vp.frame, IRect::new(x0, y0, len(x0, x1), len(y0, y1)))
}

/// 追記だけの配列の引数の要約（要素は送らず数だけ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogSummary {
    pub field: String,
    pub count: usize,
}

/// ノード 1 つの要約（エディタの操作のスタックに出すもの）。
#[derive(Debug, Clone, PartialEq)]
pub struct NodeSummary {
    pub id: NodeId,
    pub op: String,
    pub v: u16,
    pub enabled: bool,
    /// 正規の形の引数（追記だけの配列は `[]` にしたもの。要素の数は `log`）。
    pub params: Json,
    pub log: Option<LogSummary>,
}

/// 前にフロントへ返した文書からの差分。
#[derive(Debug, Clone, PartialEq)]
pub struct DocDelta {
    /// 差分の元の文書の番号（None は全体：`nodes` にすべてのノードがある）。
    pub base: Option<u64>,
    /// 今のノードの並び（ID）。ここにないノードは消えた。
    pub order: Vec<NodeId>,
    /// 足した・変わったノード（全体のときはすべて）。並びは `order` の順。
    pub nodes: Vec<NodeSummary>,
    /// 出力（変わったときと全体のときだけ）。
    pub outputs: Option<Vec<OutputSpec>>,
}

impl DocDelta {
    /// 何も変わっていなければ true。
    pub fn is_empty(&self) -> bool {
        self.base.is_some() && self.nodes.is_empty() && self.outputs.is_none()
    }
}

fn summarize(n: &kura_engine::session::NodeView<'_>) -> NodeSummary {
    let (params, log) = match n.params {
        ParamsView::Json(v) => (v.clone(), None),
        ParamsView::Log { shell, field, log } => (
            shell.clone(),
            Some(LogSummary {
                field: field.to_string(),
                count: log.len(),
            }),
        ),
    };
    NodeSummary {
        id: n.id,
        op: n.op.to_string(),
        v: n.v,
        enabled: n.enabled,
        params,
        log,
    }
}

/// 文書の差分：`prev`（前に返した文書とその番号）から `now` への違い。`prev` がないか、文書の入力・規則・スキーマが違えば全体。
pub fn doc_delta(prev: Option<(&Snapshot, u64)>, now: &Snapshot) -> DocDelta {
    let order: Vec<NodeId> = now.nodes().map(|n| n.id).collect();
    let full = || DocDelta {
        base: None,
        order: order.clone(),
        nodes: now.nodes().map(|n| summarize(&n)).collect(),
        outputs: Some(now.outputs().to_vec()),
    };
    let Some((old, rev)) = prev else {
        return full();
    };
    let diff = now.diff(old);
    if diff.head_changed {
        return full();
    }
    let changed: Vec<NodeId> = diff
        .nodes
        .iter()
        .filter(|(_, c)| !matches!(c, NodeChange::Same | NodeChange::Removed))
        .map(|(id, _)| *id)
        .collect();
    DocDelta {
        base: Some(rev),
        order: order.clone(),
        nodes: now
            .nodes()
            .filter(|n| changed.contains(&n.id))
            .map(|n| summarize(&n))
            .collect(),
        outputs: diff.outputs_changed.then(|| now.outputs().to_vec()),
    }
}

/// フレームの矩形と表示時間（段の座標）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub index: u32,
    pub rect: IRect,
    /// フレームの列の表示時間（1 枚の画像は None）。
    pub duration_ms: Option<u32>,
}

/// 変わったタイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedTiles {
    /// すべて取り直す（最初の表示・段や形が変わった）。
    All,
    /// フレームごとの変わったタイル（`[tx, ty]`。フレームの画像の座標、1 辺 [`VIEW_TILE`]）。空なら何も変わっていない。
    Tiles(Vec<(u32, Vec<[u32; 2]>)>),
}

/// 表示した結果（kura:// の `/edit/{session}/{rev}/{level}/{frame}/{tx}/{ty}` で、この rev・段のタイルを取る）。
#[derive(Debug, Clone, PartialEq)]
pub struct ViewInfo {
    pub rev: u64,
    /// 描いた段（0 は等倍）。
    pub level: u8,
    /// 求めた段から段 0 に落ちた理由（落ちなければ None）。
    pub fallback: Option<String>,
    /// 等倍の出力の大きさ（大きさの決まっていない軸は上限）。
    pub full: (u64, u64),
    /// 段のキャンバスの大きさ。
    pub canvas: (u64, u64),
    /// フレーム（段の座標）。1 枚の画像はフレーム 0 だけ。
    pub frames: Vec<FrameInfo>,
    /// 画素の形式（`rgba8` など。タイルは形式によらず RGBA8 で配る）。
    pub format: Option<String>,
    /// タイルの 1 辺。
    pub tile: u32,
    pub changed: ChangedTiles,
    /// メタデータ・フレームの表示時間・繰り返しが変わった。
    pub meta_changed: bool,
    /// 描いたノード・当たったノードの警告（全体の警告は保存のときに出す）。
    pub warnings: Vec<Issue>,
    /// 描いたノードの数。
    pub drawn_nodes: u64,
    /// 描くのにかかった時間（確保と表示の経路。ミリ秒）。
    pub ms: f64,
}

/// 形の大きさ（決まっていない軸は上限）。
pub fn shape_size(s: &Shape) -> (u64, u64) {
    match *s {
        Shape::Image { w, h, .. } | Shape::Frames { w, h, .. } => (w.max(), h.max()),
        Shape::Audio { .. } => (0, 0),
    }
}

/// 表示の値のフレームの矩形と表示時間。
pub fn frames_of(value: &PartialValue) -> Vec<FrameInfo> {
    (0..value.frame_count())
        .filter_map(|i| {
            let index = u32::try_from(i).ok()?;
            Some(FrameInfo {
                index,
                rect: value.frame_rect(index)?,
                duration_ms: value.frame_duration_ms(index),
            })
        })
        .collect()
}

/// 変わったタイルの報告を、フロントへ返す形にする（音声の区間は全体として扱う）。
pub fn changed_tiles(c: &OutputChange) -> ChangedTiles {
    match c {
        OutputChange::All | OutputChange::Samples(_) => ChangedTiles::All,
        OutputChange::Tiles(map) => ChangedTiles::Tiles(
            map.iter()
                .map(|(f, tiles)| (*f, tiles.iter().map(|(x, y)| [x, y]).collect()))
                .filter(|(_, t): &(u32, Vec<[u32; 2]>)| !t.is_empty())
                .collect(),
        ),
    }
}

/// 画素の形式の名前（`rgba8` など。serde の綴り）。
pub fn format_name(value: &PartialValue) -> Option<String> {
    let fmt = value.format()?;
    serde_json::to_value(fmt)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
}

/// タイルの 1 辺（配信の単位）。
pub const TILE: u32 = VIEW_TILE;

/// 段 0 に落ちた理由の名前（`notLinear`・`notImage`・`indexed`・`analysis`・`infer`。docs/editing.md §12.7）。
pub fn fallback_name(f: &LevelFallback) -> String {
    match f {
        LevelFallback::NotLinear => "notLinear",
        LevelFallback::NotImage(_) => "notImage",
        LevelFallback::Indexed(_) => "indexed",
        LevelFallback::Analysis(_) => "analysis",
        LevelFallback::Infer { .. } => "infer",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use kura_engine::doc::Graph;
    use kura_engine::session::{CommitMode, Edit, EditSession, HistoryLimits};
    use kura_engine::value::PixelFormat;
    use kura_engine::{Digest, Engine};
    use serde_json::json;

    use super::*;

    fn image(w: u64, h: u64, fmt: PixelFormat) -> Shape {
        Shape::Image {
            w: Dim::Exact(w),
            h: Dim::Exact(h),
            fmt,
        }
    }

    /// 段：縮小して見るほど粗い段。操作中は 1 段粗い（許される最も粗い段まで）。ドット絵・Indexed8・フレームの列は 0。
    #[test]
    fn level_follows_the_zoom_and_interaction() {
        let photo = image(6000, 4000, PixelFormat::Rgba8);
        assert_eq!(view_level(&photo, 1.0, false, false), 0);
        assert_eq!(view_level(&photo, 0.5, false, false), 1);
        assert_eq!(view_level(&photo, 0.25, false, false), 2);
        assert_eq!(view_level(&photo, 0.01, false, false), MAX_LEVEL);
        assert_eq!(
            view_level(&photo, 0.3, false, false),
            1,
            "0.3 は段 2 では足りない"
        );
        assert_eq!(view_level(&photo, 1.0, true, false), 1);
        assert_eq!(view_level(&photo, 0.01, true, false), MAX_LEVEL);
        assert_eq!(view_level(&photo, f64::NAN, false, false), 0);
        assert_eq!(view_level(&photo, -1.0, true, false), 1);
        assert_eq!(view_level(&photo, 0.25, true, true), 0, "ドット絵");
        let indexed = image(6000, 4000, PixelFormat::Indexed8);
        assert_eq!(view_level(&indexed, 0.25, true, false), 0);
        let frames = Shape::Frames {
            count: Dim::Exact(4),
            w: Dim::Exact(64),
            h: Dim::Exact(64),
            fmt: PixelFormat::Rgba8,
            full: kura_engine::shape::Fullness::Full,
        };
        assert_eq!(view_level(&frames, 0.1, true, false), 0);
    }

    /// 範囲：段の座標に直す（始まりは床・終わりは天井。負の座標も床）。矩形がなければ全体。
    #[test]
    fn area_scales_the_rect_to_the_level() {
        let photo = image(6000, 4000, PixelFormat::Rgba8);
        let vp = |x: i64, y: i64, w: u32, h: u32| Viewport {
            rect: Some(IRect::new(x, y, w, h)),
            ..Viewport::whole()
        };
        assert_eq!(view_area(&photo, &Viewport::whole(), 2), Area::All);
        assert_eq!(
            view_area(&photo, &vp(100, 50, 300, 200), 0),
            Area::rect(0, IRect::new(100, 50, 300, 200))
        );
        assert_eq!(
            view_area(&photo, &vp(101, 51, 300, 201), 2),
            Area::rect(0, IRect::new(25, 12, 76, 51))
        );
        assert_eq!(
            view_area(&photo, &vp(-3, -5, 4, 4), 1),
            Area::rect(0, IRect::new(-2, -3, 3, 3))
        );
        let frames = Shape::Frames {
            count: Dim::Exact(4),
            w: Dim::Exact(64),
            h: Dim::Exact(64),
            fmt: PixelFormat::Rgba8,
            full: kura_engine::shape::Fullness::Full,
        };
        let whole = Viewport {
            frame: 2,
            ..Viewport::whole()
        };
        assert_eq!(view_area(&frames, &whole, 0), Area::whole_frame(2));
    }

    /// 文書の差分：最初は全体。追記は数だけ、引数を変えたノード・足したノードだけを返し、消したノードは並びから外れる。
    #[test]
    fn doc_delta_reports_only_what_changed() {
        let engine = Engine::new().unwrap();
        let mut g = Graph::new_single();
        g.push_linear("image.flip", 1, json!({ "axis": "vertical" }), 0)
            .unwrap();
        g.push_linear("paint.actions", 1, json!({ "actions": [] }), 0)
            .unwrap();
        let mut s =
            EditSession::open(&engine, &g, Digest::default(), HistoryLimits::default()).unwrap();
        let first = s.current().clone();
        let d = doc_delta(None, &first);
        assert_eq!(d.base, None);
        assert_eq!(d.order, vec![NodeId(1), NodeId(2)]);
        assert_eq!(d.nodes.len(), 2);
        assert_eq!(d.nodes[1].params, json!({ "actions": [] }));
        assert_eq!(
            d.nodes[1].log,
            Some(LogSummary {
                field: "actions".into(),
                count: 0
            })
        );
        assert!(d.outputs.is_some());
        assert!(!d.is_empty());
        assert!(doc_delta(Some((&first, 0)), &first).is_empty());

        let stroke = super::super::tests_support::stroke(&[(1, 1), (3, 2)], [255, 0, 0, 255]);
        let r = s.commit(
            &engine,
            Edit::AppendItems {
                node: NodeId(2),
                field: "actions".into(),
                items: vec![stroke],
            },
            CommitMode::Step,
        );
        assert!(r.is_ok(), "{r:?}");
        let d = doc_delta(Some((&first, 0)), s.current());
        assert_eq!(d.base, Some(0));
        assert_eq!(d.nodes.len(), 1);
        assert_eq!(d.nodes[0].id, NodeId(2));
        assert_eq!(d.nodes[0].log.as_ref().map(|l| l.count), Some(1));
        assert_eq!(
            d.nodes[0].params,
            json!({ "actions": [] }),
            "要素は送らない"
        );
        assert_eq!(d.outputs, None);

        let second = s.current().clone();
        s.commit(
            &engine,
            Edit::RemoveNode { node: NodeId(1) },
            CommitMode::Step,
        )
        .unwrap();
        let d = doc_delta(Some((&second, 1)), s.current());
        assert_eq!(d.order, vec![NodeId(2)]);
        assert!(d.nodes.iter().all(|n| n.id != NodeId(1)));
    }

    #[test]
    fn changed_tiles_drop_empty_frames() {
        use kura_engine::render::region::Tiles;
        let mut t = Tiles::none(600, 300, VIEW_TILE);
        t.insert(2, 1);
        let empty = Tiles::none(600, 300, VIEW_TILE);
        let c = OutputChange::Tiles([(0, t), (1, empty)].into_iter().collect());
        assert_eq!(
            changed_tiles(&c),
            ChangedTiles::Tiles(vec![(0, vec![[2, 1]])])
        );
        assert_eq!(changed_tiles(&OutputChange::All), ChangedTiles::All);
    }
}
