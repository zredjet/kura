//! プレビューの段（docs/editing.md §12。段 10。E32）：[`choose_level`]・[`downscale`]・[`Engine::plan_level`]。
//!
//! - 段 k（0〜[`MAX_LEVEL`]）の値は、等倍の大きさを各軸 ⌈n/2^k⌉（[`level_size`]）にした近似。縮め方は `imaging::resample` の
//!   box（線形光・乗算済み。形式は `smooth_format`）で、等倍から段ごとに直接作る（[`downscale`]）。縮める処理は
//!   `Reach::Resample` なので、等倍の値の変わったタイルから、縮め直す段のタイルが求まる（[`rescale_tiles`]・[`downscale_changed`]）。
//! - 段の計画（[`Engine::plan_level`]）：線形の鎖で、最後の「等倍でしか描けない操作」（`Preview::FullOnly`）までを等倍のまま描き
//!   （計画のノードをそのまま共有するので、キャッシュの指紋も値も等倍の計画と同じ）、その出力を縮めるノード（継ぎ目。
//!   [`SEAM_NODE`]）を挟み、後ろを、引数を段に縮めて（`Op::preview_params`）段の形で推論し直したノードで描く。種は元の source
//!   のまま（計画の主の入力も同じ）。
//! - 段 0 に落とす（[`LevelFallback`]）：線形の鎖でない、継ぎ目か後ろの値が 1 枚の画像でない（フレームの列・音声）か Indexed8、
//!   後ろのノードが解析を申告した、段の形で推論が通らない（今の登録表の操作では解析・推論の 2 つには届かず、試験用の操作で
//!   確かめる：tests/preview.rs の `fallback_reasons_of_level_nodes`。`palette.replace` は Indexed8 を受けるので、先に
//!   Indexed8 で落ちる）。大きさが中身で決まる値（trim の出力）は、上限の大きさで推論して段にする（等倍で描けるなら段でも
//!   描ける：縮めた矩形は ⌈n/2^k⌉ に収まる）。
//! - 継ぎ目の値（段の入力）は派生の指紋（[`level_fingerprint`]：`Fingerprint::derive("level", 継ぎ目の指紋, [段, 規則])`）で
//!   キャッシュに入る（ほかの項目と同じく数え、使った順で追い出す。`register_external` は使わない：外の値にすると、継ぎ目が
//!   変わるたびに呼び手が古い縮小を外す（`unregister_external`）手間が要り、外し忘れると数えたまま残る）。継ぎ目までのノードの
//!   指紋は段 0（ノードの段。`PlannedNode::level`）なので、等倍の計画と同じ項目に当たる。
//! - 段の値は近似で、保存・一括・スポイト・解析・ダイジェストに使わない：`render`・`render_with_solver` とダイジェストを取る
//!   `render_cached` と `spoit` は段の計画を断り、範囲の描画の結果は由来 `Origin::Scaled`、段の計画の後ろには解析を申告する
//!   ノードを置かない。ダイジェストを取らない `render_cached` の段の出力は `Value` のまま返るので、保存の道に渡さないのは
//!   呼び手の約束（保存・一括は `render` を通す。docs/editing.md §12.7）。同じ文書・入力・段なら、スレッドの数によらず
//!   同じ値。版をまたいで保つ約束はしない（ゴールデンに入れず、縮め方を変えても操作の版は上げない）。
//! - メタデータ：継ぎ目の縮小は格子・ピボット・スライスを段の座標に写し（割り切れない格子は消す）、`Preview::Skip` の操作
//!   （meta.*）は画素を描かずに、メタデータを段の座標で書く（段で書けなければ外す）。段の警告は約束しない（等倍の
//!   計画・描画の警告が正。継ぎ目の縮小は警告を出さない）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::cache::Fingerprint;
use super::region::{Area, FrameTiles, Reach, Rects, Tiles, base_image, exact_image_size, pick_of};
use super::{Engine, NodeParts, Plan, PlannedNode, release_schedule};
use crate::digest::{Digest, Hasher};
use crate::doc::{NodeId, PortRef};
use crate::error::{EngineError, Issues};
use crate::geom::{IPoint, IRect, Ratio, Size, div_round_half_even};
use crate::imaging::resample::{
    Filter, resize_smooth, resize_smooth_picked, smooth_format, smooth_work,
};
use crate::mapping::{SpaceMap, TimeMap, map_image_meta_for};
use crate::monitor::Monitor;
use crate::op::{
    AnyParams, Applied, ApplyCx, Example, InferCx, Inferred, Lift, MetaAccess, Numeric, Op,
    OpParams, ParamKind, ParamSchema, PortSpec, Signature, Unit, Work, spec,
};
use crate::shape::{Dim, Shape, ValueKind};
use crate::value::{Image, PixelFormat, Raster, TILE, Value};

/// 段の上限（段 k の大きさは各軸 ⌈n/2^k⌉。24MP の 1/8 で 750×500）。
pub const MAX_LEVEL: u8 = 3;

/// 継ぎ目の縮小のノードの ID（文書のノードは `next_id` より小さいので、u32::MAX にはならない）。
pub const SEAM_NODE: NodeId = NodeId(u32::MAX);

/// 継ぎ目の縮小の操作の ID（登録表には入れない。段の計画の中にだけある）。
pub const DOWNSCALE_ID: &str = "kura.preview.downscale";

/// プレビューの段での操作の扱い（`Op::preview`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    /// 等倍でしか描けない（既定）。鎖の継ぎ目になる：ここまでを等倍で描き、出力を縮めてから後ろを段で描く。
    FullOnly,
    /// 引数を段に縮めて（`Op::preview_params`）、段の値で描ける。段の形で推論が通らなければ、計画を段 0 に落とす。
    Scaled,
    /// 画素は描かない（入力の画素をそのまま通す）。メタデータは `Op::preview_params` で段の座標に写して書く。段の形で推論が
    /// 通らなければ、このノードを外したものとして描く（メタデータは段では書かない）。
    Skip,
}

/// 段 `level` の長さ：⌈n / 2^level⌉（n ≥ 1 なら 1 以上）。
pub fn level_size(n: u64, level: u8) -> u64 {
    let d = 1u64.checked_shl(u32::from(level)).unwrap_or(u64::MAX);
    n.div_ceil(d)
}

/// 引数の長さ（画素）を段 `level` に縮める：⌈n / 2^level⌉（n ≥ 1 なら 1 以上）。
pub fn scale_len(n: u32, level: u8) -> u32 {
    u32::try_from(level_size(u64::from(n), level)).unwrap_or(u32::MAX)
}

/// 座標を段に縮める（床）。
fn floor_shift(v: i64, level: u8) -> i64 {
    v >> u32::from(level.min(62))
}

/// 座標を段に縮める（天井）：⌈v / 2^level⌉ = ⌊(v + 2^level − 1) / 2^level⌋。
fn ceil_shift(v: i64, level: u8) -> i64 {
    let k = u32::from(level.min(62));
    v.saturating_add((1i64 << k) - 1) >> k
}

/// 矩形を段 `level` に縮める：始まりは床、終わりは天井（元の矩形を覆う。幅・高さが 1 以上なら 1 以上のまま）。
pub fn scale_rect(r: IRect, level: u8) -> IRect {
    let (x0, y0) = (floor_shift(r.x, level), floor_shift(r.y, level));
    let (x1, y1) = (ceil_shift(r.right(), level), ceil_shift(r.bottom(), level));
    let len = |a: i64, b: i64, orig: u32| -> u32 {
        if orig == 0 {
            return 0;
        }
        u32::try_from(b.saturating_sub(a))
            .unwrap_or(u32::MAX)
            .max(1)
    };
    IRect::new(x0, y0, len(x0, x1, r.w), len(y0, y1, r.h))
}

/// 矩形を大きさ `w × h` の画像の中に収める（幅・高さは 1 以上。`w`・`h` が 0 ならそのまま）。入力の中に収める必要のある操作
/// （crop）の `preview_params` が使う（段の入力に収める約束。docs/editing.md §7.1）。
pub fn clamp_rect(r: IRect, w: u32, h: u32) -> IRect {
    if w == 0 || h == 0 {
        return r;
    }
    let (w, h) = (i64::from(w), i64::from(h));
    let x0 = r.x.clamp(0, w - 1);
    let y0 = r.y.clamp(0, h - 1);
    let x1 = r.right().clamp(x0 + 1, w);
    let y1 = r.bottom().clamp(y0 + 1, h);
    IRect::new(
        x0,
        y0,
        u32::try_from(x1 - x0).unwrap_or(1),
        u32::try_from(y1 - y0).unwrap_or(1),
    )
}

/// 点を段 `level` に縮める（x / 2^level を偶数への丸めで）。
pub fn scale_point(p: IPoint, level: u8) -> IPoint {
    let d = 1i128 << u32::from(level.min(62));
    let s = |v: i64| div_round_half_even(i128::from(v), d).unwrap_or(floor_shift(v, level));
    IPoint {
        x: s(p.x),
        y: s(p.y),
    }
}

/// 1 枚の画像の形の大きさの上限（大きさが決まっていなければ取りうる最大。画像でなければ None）。段の入力に収める
/// `preview_params` が使う。
pub fn image_bounds(s: &Shape) -> Option<(u32, u32)> {
    match *s {
        Shape::Image { w, h, .. } => Some((
            u32::try_from(w.max()).unwrap_or(u32::MAX),
            u32::try_from(h.max()).unwrap_or(u32::MAX),
        )),
        _ => None,
    }
}

/// 長さを段 `level` に割り切れるときだけ縮める（2^level で割り切れなければ None）。格子を段の座標に写すのに使う
/// （`mapping::map_grid` の倍率の写し方と同じく、割り切れない格子は写さない）。
pub fn scale_exact(n: u32, level: u8) -> Option<u32> {
    let d = 1u32.checked_shl(u32::from(level))?;
    n.is_multiple_of(d).then_some(n / d)
}

/// 形を段 `level` に縮める（画像・フレームの列の大きさを各軸 [`level_size`] に。音声はそのまま）。
pub fn level_shape(s: &Shape, level: u8) -> Shape {
    let dim = |d: Dim| match d {
        Dim::Exact(n) => Dim::Exact(level_size(n, level)),
        Dim::AtMost(n) => Dim::AtMost(level_size(n, level)),
    };
    match *s {
        Shape::Image { w, h, fmt } => Shape::Image {
            w: dim(w),
            h: dim(h),
            fmt,
        },
        Shape::Frames {
            count,
            w,
            h,
            fmt,
            full,
        } => Shape::Frames {
            count,
            w: dim(w),
            h: dim(h),
            fmt,
            full,
        },
        Shape::Audio { .. } => *s,
    }
}

/// 段の選び方の手がかり（呼び手が渡す。エンジンは中身を見て判定しない）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LevelHint {
    /// ドット絵として扱う（M11 の判定。等倍で描く）。
    pub pixel_art: bool,
}

/// 段を選ぶ（純粋な関数）：`full` は等倍の出力の形、`need` は表示に要る装置の画素（CSS px × DPR × 拡大率）。ドット絵の指定・
/// Indexed8・フレームの列・音声・大きさの決まっていない形は 0。ほかは、各軸が `need` を下回らない最も粗い段（上限
/// [`MAX_LEVEL`]）。方針（ドラッグ中は粗い段、止まったら細かい段か段 0 の範囲）は呼び手が持つ。
pub fn choose_level(full: &Shape, need: Size, hint: LevelHint) -> u8 {
    if hint.pixel_art {
        return 0;
    }
    let Shape::Image {
        w: Dim::Exact(w),
        h: Dim::Exact(h),
        fmt,
    } = *full
    else {
        return 0;
    };
    if fmt == PixelFormat::Indexed8 {
        return 0;
    }
    (0..=MAX_LEVEL)
        .rev()
        .find(|k| level_size(w, *k) >= u64::from(need.w) && level_size(h, *k) >= u64::from(need.h))
        .unwrap_or(0)
}

/// 等倍の画素を段 `level` に縮める（`resize_smooth` の box・線形光。大きさは各軸 ⌈n/2^level⌉、形式は `smooth_format`）。
/// `region` があれば、そのタイル（段の値のタイル）だけを描き、ほかは前の段の値のタイルを共有するか無効なタイルにする
/// （描いたタイルは全体を縮めたものと同じ値。同じ閉包）。
pub fn downscale(
    px: &Raster,
    level: u8,
    mon: &dyn Monitor,
    region: Option<(&Tiles, Option<&Raster>)>,
) -> Result<Raster, EngineError> {
    let (w, h) = level_dims(px.width(), px.height(), level);
    match region {
        None => resize_smooth(px, w, h, Filter::Box, mon),
        Some((pick, base)) => resize_smooth_picked(px, w, h, Filter::Box, mon, pick, base),
    }
}

/// 段の大きさ（各軸 ⌈n/2^level⌉）。
fn level_dims(w: u32, h: u32, level: u8) -> (u32, u32) {
    (scale_len(w, level), scale_len(h, level))
}

/// 等倍の値が `prev` から `next` に変わったとき、縮め直す段 `level` のタイル（値のタイル [`TILE`]）。中身の変わりうる等倍の
/// タイルは、タイルの実体の同一性（`Raster::shares_tile`）で求め、`Reach::Resample` で段へ写す。大きさ・形式・パレットが
/// 違えば None（全体を縮め直す）。
pub fn rescale_tiles(prev: &Raster, next: &Raster, level: u8) -> Option<Tiles> {
    if prev.width() != next.width()
        || prev.height() != next.height()
        || prev.format() != next.format()
        || prev.palette() != next.palette()
    {
        return None;
    }
    let in_size = Size {
        w: next.width(),
        h: next.height(),
    };
    let (lw, lh) = level_dims(in_size.w, in_size.h, level);
    let mut changed = Rects::new();
    for ty in 0..next.tiles_y() {
        for tx in 0..next.tiles_x() {
            if !next.shares_tile(prev, tx, ty) {
                changed.push(next.tile_rect(tx, ty));
            }
        }
    }
    let reach = Reach::Resample {
        filter: Filter::Box,
        in_size,
        out_size: Size { w: lw, h: lh },
    };
    let area = reach.forward(&Area::frame(0, changed));
    Some(match area.frame_rects(0) {
        Some(r) => Tiles::covering(lw, lh, TILE, &r, IPoint { x: 0, y: 0 }),
        None if area.is_empty() => Tiles::none(lw, lh, TILE),
        None => Tiles::all(lw, lh, TILE),
    })
}

/// 等倍の値が `prev_full` から `next_full` に変わったとき、前の段の値 `prev_level`（`prev_full` を縮めたもの）から、変わった
/// タイルだけを縮め直す（[`rescale_tiles`]。ほかのタイルは `prev_level` と共有する）。値は `downscale(next_full)` と同じ。
/// 形が違えば全体を縮め直す。
pub fn downscale_changed(
    prev_full: &Raster,
    prev_level: &Raster,
    next_full: &Raster,
    level: u8,
    mon: &dyn Monitor,
) -> Result<Raster, EngineError> {
    let (lw, lh) = level_dims(next_full.width(), next_full.height(), level);
    let same_level = prev_level.width() == lw
        && prev_level.height() == lh
        && prev_level.format() == smooth_format(next_full.format())
        && prev_level.palette().is_none();
    match rescale_tiles(prev_full, next_full, level) {
        Some(pick) if same_level => {
            downscale(next_full, level, mon, Some((&pick, Some(prev_level))))
        }
        _ => downscale(next_full, level, mon, None),
    }
}

/// 継ぎ目の縮小の値の指紋（段の入力。`Fingerprint::derive("level", 継ぎ目の指紋, [段, 規則])`）。規則はメタデータの写し方を選ぶ。
pub fn level_fingerprint(seam: Fingerprint, level: u8, rules: u32) -> Fingerprint {
    let mut args = vec![level];
    args.extend_from_slice(&rules.to_le_bytes());
    Fingerprint::derive("level", seam, &args)
}

/// 継ぎ目の縮小のノードの鍵（段ごとに決まる。指紋は [`level_fingerprint`] で求める）。
pub(crate) fn seam_key(level: u8) -> Digest {
    Hasher::new("kura-preview-seam-v1").u8(level).finish()
}

/// 継ぎ目の縮小の引数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DownscaleParams {
    pub(crate) level: u8,
}

impl OpParams for DownscaleParams {
    fn schema() -> ParamSchema {
        ParamSchema {
            params: vec![spec(
                "level",
                ParamKind::Int {
                    min: 0,
                    max: i64::from(MAX_LEVEL),
                    step: 1,
                },
                Unit::None,
                json!(1),
            )],
        }
    }
}

/// 継ぎ目の縮小（段の計画の中にだけ置く操作。登録表には入れない）：等倍の画像を段に縮め、メタデータを段の座標に写す
/// （写せない格子・はみ出した点は消す。段の警告は約束しないので出さない）。
pub(crate) struct Downscale;

/// 縮小の写し方（入力から段の大きさへの倍率）。
fn downscale_space(w: u32, h: u32, lw: u32, lh: u32) -> SpaceMap {
    SpaceMap::Scale {
        sx: Ratio { num: lw, den: w },
        sy: Ratio { num: lh, den: h },
        out: Size { w: lw, h: lh },
    }
}

/// 縮小の本体（全体か、`region` のタイルだけ）。
fn downscale_value(
    p: &DownscaleParams,
    inputs: &[&Value],
    cx: &mut ApplyCx<'_>,
    region: Option<(&FrameTiles, Option<&Value>)>,
) -> Result<Applied, EngineError> {
    cx.check()?;
    let Some(Value::Image(img)) = inputs.first() else {
        return Err(EngineError::Internal(
            "継ぎ目の縮小の入力が画像ではありません".into(),
        ));
    };
    let (w, h) = (img.px.width(), img.px.height());
    let (lw, lh) = level_dims(w, h, p.level);
    let px = match region {
        None => downscale(&img.px, p.level, cx.mon, None)?,
        Some((tiles, base)) => {
            let pick = pick_of(tiles, 0, lw, lh);
            downscale(&img.px, p.level, cx.mon, Some((&pick, base_image(base)?)))?
        }
    };
    let space = downscale_space(w, h, lw, lh);
    // 段の警告は約束しない（等倍の計画・描画の警告が正）ので、写したときの警告は出さない
    let (meta, _warnings) = map_image_meta_for(cx.rules, &img.meta, Size { w, h }, &space)?;
    Ok(Applied {
        value: Value::Image(Arc::new(Image { px, meta })),
        space: Some(space),
        time: None,
    })
}

impl Op for Downscale {
    const ID: &'static str = DOWNSCALE_ID;
    const VERSION: u16 = 1;
    type Params = DownscaleParams;

    fn signature() -> Signature {
        Signature {
            inputs: &[PortSpec {
                name: "image",
                accepts: &[ValueKind::Image],
            }],
            lift: Lift::None,
            numeric: Numeric::Float,
            meta: MetaAccess::Write,
        }
    }

    fn infer(p: &DownscaleParams, inputs: &[Shape], _cx: &InferCx<'_>) -> Result<Inferred, Issues> {
        let Some(Shape::Image { w, h, fmt }) = inputs.first().copied() else {
            return Err(Issues::one(crate::error::Issue::error(
                crate::error::IssueCode::BadInput,
                "継ぎ目の縮小は 1 枚の画像にだけ当てられます",
            )));
        };
        let dim = |d: Dim| match d {
            Dim::Exact(n) => Dim::Exact(level_size(n, p.level)),
            Dim::AtMost(n) => Dim::AtMost(level_size(n, p.level)),
        };
        // 大きさが中身で決まる値（trim の出力）は、写し方を実行で決める
        let space = match exact_image_size(&inputs[0]) {
            Some(size) => {
                let (lw, lh) = level_dims(size.w, size.h, p.level);
                downscale_space(size.w, size.h, lw, lh)
            }
            None => SpaceMap::Unknown,
        };
        Ok(Inferred {
            shape: Shape::Image {
                w: dim(w),
                h: dim(h),
                fmt: smooth_format(fmt),
            },
            space,
            time: TimeMap::Identity,
            cost: w.max().saturating_mul(h.max()),
        })
    }

    fn apply(
        p: &DownscaleParams,
        inputs: &[&Value],
        cx: &mut ApplyCx<'_>,
    ) -> Result<Applied, EngineError> {
        downscale_value(p, inputs, cx, None)
    }

    fn work_bytes(p: &DownscaleParams, inputs: &[Shape], _out: &Shape) -> Work {
        let Some((size, fmt)) = inputs
            .first()
            .and_then(|s| Some((exact_image_size(s)?, image_format(s)?)))
        else {
            return Work::ZERO;
        };
        let (lw, lh) = level_dims(size.w, size.h, p.level);
        let (shared, per_task) = smooth_work(
            u64::from(size.w),
            u64::from(size.h),
            fmt.bytes_per_pixel() as u64,
            u64::from(lw),
            u64::from(lh),
            Filter::Box,
        );
        Work { shared, per_task }
    }

    fn examples() -> Vec<Example<DownscaleParams>> {
        // 登録表に入れないので、適合テスト・ゴールデンの例は持たない（preview.rs のテストで照らす）
        Vec::new()
    }

    const REGION: bool = true;

    fn reach(p: &DownscaleParams, inputs: &[Shape]) -> Reach {
        match inputs.first().and_then(exact_image_size) {
            Some(in_size) => {
                let (lw, lh) = level_dims(in_size.w, in_size.h, p.level);
                Reach::Resample {
                    filter: Filter::Box,
                    in_size,
                    out_size: Size { w: lw, h: lh },
                }
            }
            None => Reach::Global,
        }
    }

    fn apply_region(
        p: &DownscaleParams,
        inputs: &[&Value],
        tiles: &FrameTiles,
        base: Option<&Value>,
        cx: &mut ApplyCx<'_>,
    ) -> Option<Result<Applied, EngineError>> {
        Some(downscale_value(p, inputs, cx, Some((tiles, base))))
    }
}

/// 画像の形の形式。
fn image_format(s: &Shape) -> Option<PixelFormat> {
    match *s {
        Shape::Image { fmt, .. } => Some(fmt),
        _ => None,
    }
}

/// 段の計画にできず、段 0（等倍の計画）に落ちた理由。
#[derive(Debug, Clone, PartialEq)]
pub enum LevelFallback {
    /// 線形の鎖でない（主の入力 1 つ・量子化しない出力 1 つ・各ノードの入力が直前の 1 つ、でない）。
    NotLinear,
    /// 継ぎ目か後ろの値が 1 枚の画像でない（フレームの列・音声）。
    NotImage(NodeId),
    /// 継ぎ目か後ろの値が Indexed8（パレットの番号の意味を等倍と同じに保つ）。
    Indexed(NodeId),
    /// 後ろのノードが解析を申告した（段の値は解析に使わない）。
    Analysis(NodeId),
    /// 段の形で推論が通らない Scaled の操作（今の登録表の操作では届かない。`palette.replace` は Indexed8 を受けるので先に
    /// [`LevelFallback::Indexed`] で落ちる）。
    Infer { node: NodeId, issues: Issues },
}

/// 段の計画（[`Engine::plan_level`] の結果）。
#[derive(Clone)]
pub struct LevelPlan {
    /// 計画（段は `plan.level()`。段 0 に落ちたら等倍の計画の写し）。
    pub plan: Plan,
    /// 継ぎ目：等倍で描いて縮める値のノード（最後の `Preview::FullOnly`）。None は入力をそのまま縮める（段 0 も None）。
    pub seam: Option<NodeId>,
    /// 段 0 に落ちた理由（落ちなければ None。段 0 を求めたときも None）。
    pub fallback: Option<LevelFallback>,
}

impl std::fmt::Debug for LevelPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LevelPlan")
            .field("level", &self.plan.level())
            .field("seam", &self.seam)
            .field("fallback", &self.fallback)
            .finish()
    }
}

impl Engine {
    /// 等倍の計画 `plan` から、段 `level`（0〜[`MAX_LEVEL`]）のプレビューの計画を作る（描かない。モジュールの説明の規則）。
    /// 段の計画にできなければ段 0 に落とす（`fallback` に理由）。別のエンジンの計画・段の計画は `Internal`・`Unsupported`。
    pub fn plan_level(&self, plan: &Plan, level: u8) -> Result<LevelPlan, EngineError> {
        if plan.engine != self.token {
            return Err(EngineError::Internal(
                "別のエンジンで作った計画です（キャッシュの指紋が登録表に縛られないため）".into(),
            ));
        }
        if plan.level != 0 {
            return Err(EngineError::Unsupported(
                "段の計画は等倍の計画から作ります".into(),
            ));
        }
        if level > MAX_LEVEL {
            return Err(EngineError::Unsupported(format!(
                "段は 0〜{MAX_LEVEL} です（{level}）"
            )));
        }
        if level == 0 {
            return Ok(LevelPlan {
                plan: plan.clone(),
                seam: None,
                fallback: None,
            });
        }
        match self.level_plan(plan, level)? {
            Ok(lp) => Ok(lp),
            Err(fallback) => Ok(LevelPlan {
                plan: plan.clone(),
                seam: None,
                fallback: Some(fallback),
            }),
        }
    }

    /// 段の計画を組む（段 0 に落ちるなら理由を返す）。
    fn level_plan(
        &self,
        plan: &Plan,
        level: u8,
    ) -> Result<Result<LevelPlan, LevelFallback>, EngineError> {
        // 線形の鎖：主の入力 1 つ・量子化しない出力 1 つ・各ノードの入力が直前の 1 つ
        let Some(primary) = plan.primary else {
            return Ok(Err(LevelFallback::NotLinear));
        };
        let [(out_id, out_from, _, quantize)] = plan.outputs.as_slice() else {
            return Ok(Err(LevelFallback::NotLinear));
        };
        if plan.inputs.len() != 1 || quantize.is_some() {
            return Ok(Err(LevelFallback::NotLinear));
        }
        let mut prev = PortRef::Input { id: primary };
        for n in &plan.nodes {
            if n.inputs.as_slice() != [prev] {
                return Ok(Err(LevelFallback::NotLinear));
            }
            prev = PortRef::Node { id: n.id };
        }
        if *out_from != prev {
            return Ok(Err(LevelFallback::NotLinear));
        }
        // 継ぎ目：最後の有効な FullOnly のノード（なければ入力）
        let mut seam_at = None;
        for (i, n) in plan.nodes.iter().enumerate() {
            if n.enabled && n.op.preview(n.params.as_ref())? == Preview::FullOnly {
                seam_at = Some(i);
            }
        }
        let (seam_port, seam_shape, seam_id) = match seam_at {
            Some(i) => {
                let n = &plan.nodes[i];
                (PortRef::Node { id: n.id }, n.inferred.shape, n.id)
            }
            None => {
                let shape = plan.inputs.get(&primary).copied().ok_or_else(|| {
                    EngineError::Internal("計画の主の入力の形がありません".into())
                })?;
                (PortRef::Input { id: primary }, shape, SEAM_NODE)
            }
        };
        if let Err(f) = level_value(&seam_shape, seam_id) {
            return Ok(Err(f));
        }
        let cx = InferCx {
            limits: &self.limits,
        };
        // 継ぎ目の縮小（派生の指紋。cache.rs の `fingerprints`）
        let op = crate::op::entry::<Downscale>();
        let params: Arc<AnyParams> = Arc::new(DownscaleParams { level });
        let inferred = match op.infer(params.as_ref(), &[seam_shape], &cx) {
            Ok(i) => i,
            Err(issues) => {
                return Ok(Err(LevelFallback::Infer {
                    node: SEAM_NODE,
                    issues,
                }));
            }
        };
        if let Err(why) = self.limits.check(&inferred.shape) {
            return Ok(Err(LevelFallback::Infer {
                node: SEAM_NODE,
                issues: Issues::one(crate::error::Issue::error(
                    crate::error::IssueCode::TooLarge,
                    why,
                )),
            }));
        }
        let mut nodes: Vec<Arc<PlannedNode>> = match seam_at {
            Some(i) => plan.nodes[..=i].to_vec(),
            None => Vec::new(),
        };
        let mut shape = inferred.shape;
        nodes.push(Arc::new(PlannedNode {
            id: SEAM_NODE,
            op,
            params,
            key: seam_key(level),
            inputs: vec![seam_port],
            seed: 0,
            enabled: true,
            lifted: false,
            inferred,
            needs: Vec::new(),
            level,
        }));
        let mut port = PortRef::Node { id: SEAM_NODE };
        let rest = seam_at.map_or(0, |i| i + 1);
        let mut full_shape = seam_shape;
        for n in &plan.nodes[rest..] {
            let node = match self.level_node(n, level, port, [shape, full_shape], &cx)? {
                Ok(node) => node,
                Err(f) => return Ok(Err(f)),
            };
            if let Err(f) = level_value(&node.inferred.shape, n.id) {
                return Ok(Err(f));
            }
            shape = node.inferred.shape;
            full_shape = n.inferred.shape;
            port = PortRef::Node { id: n.id };
            nodes.push(Arc::new(node));
        }
        let outputs = vec![(*out_id, port, shape, None)];
        let release = release_schedule(&nodes, &outputs);
        Ok(Ok(LevelPlan {
            plan: Plan {
                rules: plan.rules,
                inputs: plan.inputs.clone(),
                nodes,
                outputs,
                primary: plan.primary,
                release,
                level,
                engine: self.token,
                // 段の計画は差分の計画の土台にしない（compile_snapshot は段 0 の計画だけを使う）
                source: None,
            },
            seam: seam_at.map(|i| plan.nodes[i].id),
            fallback: None,
        }))
    }

    /// 継ぎ目の後ろのノード 1 つを段の形で作り直す：外したノードは入力をそのまま通し、Scaled は引数を縮めて推論し直す（通らな
    /// ければ段 0 に落ちる）。Skip は同じく作り直し、通らなければ外したものにする。解析を申告するなら段 0 に落ちる。
    /// `shapes` は（段の入力の形, 等倍の計画での入力の形）。
    fn level_node(
        &self,
        n: &PlannedNode,
        level: u8,
        port: PortRef,
        shapes: [Shape; 2],
        cx: &InferCx<'_>,
    ) -> Result<Result<PlannedNode, LevelFallback>, EngineError> {
        let [shape, full] = shapes;
        let pass = || PlannedNode {
            id: n.id,
            op: n.op.clone(),
            params: n.params.clone(),
            key: n.key,
            inputs: vec![port],
            seed: n.seed,
            enabled: false,
            lifted: false,
            inferred: Inferred {
                shape,
                space: SpaceMap::Identity,
                time: TimeMap::Identity,
                cost: 0,
            },
            needs: Vec::new(),
            level,
        };
        if !n.enabled {
            return Ok(Ok(pass()));
        }
        let mode = n.op.preview(n.params.as_ref())?;
        let in_shapes = [shape];
        let params: Arc<AnyParams> =
            Arc::from(n.op.preview_params(n.params.as_ref(), level, &in_shapes, &[full])?);
        let planned = self
            .infer_node(n.id, n.op.as_ref(), params.as_ref(), &in_shapes, cx)
            .and_then(|(lifted, inferred)| {
                self.finish_node(
                    NodeParts {
                        id: n.id,
                        op: n.op.clone(),
                        params,
                        // 鍵は等倍の文書のもの（段の引数は鍵・段・入力の形から決まり、指紋に段が入る）
                        key: n.key,
                        inputs: vec![port],
                        seed: n.seed,
                        enabled: true,
                    },
                    lifted,
                    inferred,
                    &in_shapes,
                )
            });
        match planned {
            Ok(mut node) => {
                if !node.needs.is_empty() {
                    return Ok(Err(LevelFallback::Analysis(n.id)));
                }
                node.level = level;
                Ok(Ok(node))
            }
            Err(_) if mode == Preview::Skip => Ok(Ok(pass())),
            Err(issues) => Ok(Err(LevelFallback::Infer { node: n.id, issues })),
        }
    }
}

impl Engine {
    /// 継ぎ目の縮小の差分（`render_cached` の外れの道）：前回の継ぎ目の縮小（`NodeCache::last`）と同じ段・規則で、前回の継ぎ目の
    /// 値と縮めた値がキャッシュ（`Exact`）にあれば、等倍の値の変わったタイルだけを縮め直す（[`downscale_changed`]。値は全体を
    /// 縮め直したものとビット単位で同じ）。使えなければ None（全体を縮める）。`verify` なら全体を縮め直して照らす。
    pub(super) fn render_rescaled(
        &self,
        plan: &Plan,
        node: &PlannedNode,
        ins: &[Value],
        cache: &super::cache::NodeCache,
        verify: bool,
        mon: &dyn Monitor,
    ) -> Result<Option<super::NodeRun>, EngineError> {
        let Some(last) = cache.last(node.id) else {
            return Ok(None);
        };
        if last.op.id() != DOWNSCALE_ID || last.level != node.level || last.rules != plan.rules {
            return Ok(None);
        }
        let (Some(prev_in), Some(prev_out)) = (
            last.inputs.first().and_then(|fp| cache.exact_value(fp)),
            cache.exact_value(&last.output),
        ) else {
            return Ok(None);
        };
        let (Some(Value::Image(cur)), Value::Image(prev_in), Value::Image(prev_out)) =
            (ins.first(), &prev_in, &prev_out)
        else {
            return Ok(None);
        };
        let px = downscale_changed(&prev_in.px, &prev_out.px, &cur.px, node.level, mon)?;
        let (w, h) = (cur.px.width(), cur.px.height());
        let space = downscale_space(w, h, px.width(), px.height());
        let (meta, _warnings) = map_image_meta_for(plan.rules, &cur.meta, Size { w, h }, &space)?;
        let value = Value::Image(Arc::new(Image { px, meta }));
        if verify {
            let mut cx = ApplyCx::new(mon, &self.limits, 0, plan.rules);
            let refs: Vec<&Value> = ins.iter().collect();
            let fresh = node
                .op
                .apply(node.params.as_ref(), &refs, &mut cx)
                .map_err(|e| e.at(node.id))?;
            if fresh.value.digest()? != value.digest()? {
                return Err(EngineError::Internal(
                    "継ぎ目の縮小の差分が、全体を縮め直したものと違います".into(),
                )
                .at(node.id));
            }
        }
        Ok(Some(super::NodeRun {
            value,
            apply_warnings: Vec::new(),
            map_warnings: Vec::new(),
        }))
    }
}

/// 段で描ける値か（1 枚の画像で Indexed8 でない）。
fn level_value(shape: &Shape, node: NodeId) -> Result<(), LevelFallback> {
    match shape {
        Shape::Image { fmt, .. } if *fmt == PixelFormat::Indexed8 => {
            Err(LevelFallback::Indexed(node))
        }
        Shape::Image { .. } => Ok(()),
        Shape::Frames { .. } | Shape::Audio { .. } => Err(LevelFallback::NotImage(node)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::NoMonitor;
    use crate::testgen::InputGen;
    use crate::value::{Grid, ImageMeta};
    use rand_chacha::rand_core::Rng;

    #[test]
    fn lengths_and_rects_scale_with_floor_and_ceiling() {
        assert_eq!(level_size(0, 2), 0);
        assert_eq!(level_size(1, 3), 1);
        assert_eq!(level_size(6000, 2), 1500);
        assert_eq!(level_size(6001, 2), 1501);
        assert_eq!(level_size(7, 0), 7);
        assert_eq!(level_size(5, 70), 1);
        assert_eq!(scale_len(3, 1), 2);
        assert_eq!(
            scale_rect(IRect::new(-5, 3, 10, 1), 1),
            IRect::new(-3, 1, 6, 1)
        );
        assert_eq!(
            scale_rect(IRect::new(4, 4, 8, 8), 2),
            IRect::new(1, 1, 2, 2)
        );
        assert_eq!(scale_rect(IRect::new(0, 0, 0, 3), 1).w, 0);
        // 縮めた矩形を 2^k 倍すると元を覆い、幅・高さは ⌈n/2^k⌉ + 1 以下
        let mut r = crate::determinism::rng(10);
        for _ in 0..2_000 {
            let k = (r.next_u32() % 4) as u8;
            let x = i64::from(r.next_u32() % 2_000) - 1_000;
            let y = i64::from(r.next_u32() % 2_000) - 1_000;
            let (w, h) = (1 + r.next_u32() % 900, 1 + r.next_u32() % 900);
            let o = IRect::new(x, y, w, h);
            let s = scale_rect(o, k);
            let d = 1i64 << k;
            assert!(s.x * d <= o.x && s.y * d <= o.y, "{o:?} {s:?} {k}");
            assert!(s.right() * d >= o.right() && s.bottom() * d >= o.bottom());
            assert!(s.w <= scale_len(w, k) + 1 && s.h <= scale_len(h, k) + 1);
            assert!(s.w >= 1 && s.h >= 1);
            let c = clamp_rect(s, 1 + r.next_u32() % 300, 1 + r.next_u32() % 300);
            assert!(c.w >= 1 && c.h >= 1 && c.x >= 0 && c.y >= 0);
        }
        assert_eq!(
            clamp_rect(IRect::new(-4, 9, 30, 3), 10, 10),
            IRect::new(0, 9, 10, 1)
        );
        assert_eq!(
            clamp_rect(IRect::new(20, 20, 5, 5), 10, 10),
            IRect::new(9, 9, 1, 1)
        );
        assert_eq!(scale_point(IPoint { x: 6, y: 2 }, 2), IPoint { x: 2, y: 0 });
        assert_eq!(
            scale_point(IPoint { x: 10, y: 14 }, 2),
            IPoint { x: 2, y: 4 }
        );
        assert_eq!(scale_exact(12, 2), Some(3));
        assert_eq!(scale_exact(6, 2), None);
    }

    fn noise(w: u32, h: u32, fmt: PixelFormat, seed: u64) -> Raster {
        match (InputGen::Noise { w, h, fmt, seed }).generate().unwrap() {
            Value::Image(img) => img.px.clone(),
            _ => panic!("画像ではありません"),
        }
    }

    /// 等倍の値の一部のタイルが変わったとき、変わったタイルだけを縮め直した値は、全体を縮め直した値とビット単位で同じで、
    /// 変わらないタイルは前の段の値と共有する。形が違えば全体を縮め直す。
    #[test]
    fn downscale_changed_equals_full() {
        for (fmt, level) in [
            (PixelFormat::Rgba8, 1u8),
            (PixelFormat::Rgb8, 2),
            (PixelFormat::GrayA16, 3),
        ] {
            let prev = noise(1_100, 700, fmt, 3);
            let prev_level = downscale(&prev, level, &NoMonitor, None).unwrap();
            let mut next = prev.clone();
            assert_eq!(
                rescale_tiles(&prev, &next, level).map(|t| t.count()),
                Some(0)
            );
            for (tx, ty) in [(0, 0), (2, 1), (4, 2)] {
                for b in next.tile_mut(tx, ty).iter_mut().step_by(7) {
                    *b = b.wrapping_add(91);
                }
            }
            let got = downscale_changed(&prev, &prev_level, &next, level, &NoMonitor).unwrap();
            let want = downscale(&next, level, &NoMonitor, None).unwrap();
            assert_eq!(got.to_rows().unwrap(), want.to_rows().unwrap(), "{fmt:?}");
            let pick = rescale_tiles(&prev, &next, level).unwrap();
            assert!(pick.count() > 0, "{fmt:?} {level}");
            let mut shared = 0;
            for ty in 0..got.tiles_y() {
                for tx in 0..got.tiles_x() {
                    if !pick.contains(tx, ty) {
                        assert!(got.shares_tile(&prev_level, tx, ty));
                        shared += 1;
                    }
                }
            }
            // 段 2・3 の 1100×700 は段のタイルが 2 枚なので、どれも変わったタイルに掛かる
            assert!(shared > 0 || level >= 2, "{fmt:?} {level}");
        }
        // 形が違えば全体
        let a = noise(300, 200, PixelFormat::Rgba8, 1);
        let b = noise(301, 200, PixelFormat::Rgba8, 1);
        assert!(rescale_tiles(&a, &b, 1).is_none());
        let a_level = downscale(&a, 1, &NoMonitor, None).unwrap();
        let got = downscale_changed(&a, &a_level, &b, 1, &NoMonitor).unwrap();
        let want = downscale(&b, 1, &NoMonitor, None).unwrap();
        assert_eq!(got.to_rows().unwrap(), want.to_rows().unwrap());
    }

    /// 継ぎ目の縮小はメタデータを段の座標に写す（割り切れない格子は消す）。警告は出さない。
    #[test]
    fn downscale_maps_metadata() {
        let px = noise(64, 48, PixelFormat::Rgba8, 2);
        let meta = ImageMeta {
            grid: Some(Grid {
                cell_w: 16,
                cell_h: 8,
                offset_x: 0,
                offset_y: 8,
                gap_x: 0,
                gap_y: 0,
            }),
            pivot: Some(IPoint { x: 31, y: 47 }),
            ..ImageMeta::default()
        };
        let v = Value::Image(Arc::new(Image {
            px: px.clone(),
            meta: meta.clone(),
        }));
        let limits = crate::limits::Limits::default();
        let mut cx = ApplyCx::new(&NoMonitor, &limits, 0, crate::doc::RULES);
        let out = Downscale::apply(&DownscaleParams { level: 2 }, &[&v], &mut cx).unwrap();
        let Value::Image(img) = &out.value else {
            panic!("画像ではありません");
        };
        assert_eq!((img.px.width(), img.px.height()), (16, 12));
        assert_eq!(
            img.meta.grid,
            Some(Grid {
                cell_w: 4,
                cell_h: 2,
                offset_x: 0,
                offset_y: 2,
                gap_x: 0,
                gap_y: 0,
            })
        );
        assert_eq!(img.meta.pivot, Some(IPoint { x: 8, y: 12 }));
        assert!(cx.warnings.is_empty());
        // 割り切れない格子は消す（警告なし）
        let odd = Value::Image(Arc::new(Image {
            px,
            meta: ImageMeta {
                grid: Some(Grid {
                    cell_w: 6,
                    cell_h: 8,
                    offset_x: 0,
                    offset_y: 0,
                    gap_x: 0,
                    gap_y: 0,
                }),
                ..meta
            },
        }));
        let out = Downscale::apply(&DownscaleParams { level: 2 }, &[&odd], &mut cx).unwrap();
        let Value::Image(img) = &out.value else {
            panic!("画像ではありません");
        };
        assert_eq!(img.meta.grid, None);
        assert!(cx.warnings.is_empty());
    }

    #[test]
    fn level_fingerprints_differ_by_seam_level_and_rules() {
        let fp = |b: u8| Fingerprint(Digest([b; 32]));
        let base = level_fingerprint(fp(1), 2, 1);
        assert_eq!(base, level_fingerprint(fp(1), 2, 1));
        assert_ne!(base, level_fingerprint(fp(2), 2, 1));
        assert_ne!(base, level_fingerprint(fp(1), 1, 1));
        assert_ne!(base, level_fingerprint(fp(1), 2, 2));
        assert_ne!(base, fp(1));
        assert_ne!(seam_key(1), seam_key(2));
    }
}
