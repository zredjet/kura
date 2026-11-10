//! E53（docs/editing.md §7.2・§17）：引数の記述の印（`ParamSpec.origin`・`binds`）の付け忘れを機械で見張る。
//!
//! 登録表のすべての版（旧版・退役した版を含む）の記述について、引数の種類から決まる印と、結び付けられる出どころと
//! 引数の種類の組（レシピの結び付けの表。M14 段 1 の設計 §3.2）を確かめる。結び付けを置く引数は下の表に 1 行ずつ書く。

// テストの補助関数は #[test] の外にあるので、テストと同じく unwrap などを許す（clippy.toml の allow-*-in-tests が届かない）
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use kura_engine::op::{BindKind, Origin, ParamKind, ParamSpec, Unit};
use kura_engine::render::Engine;

/// 種類だけでは決まらない例外（理由つき）：`meta.anchor_pivot` の `offset` は 9 点の基準からのずれで、元の座標ではない。
const FREE_POINTS: &[(&str, &str)] = &[("meta.anchor_pivot", "offset")];

/// 結び付けを置く引数（操作, 引数, 出どころの種類）。増やすときはここに 1 行足す。
const BOUND: &[(&str, &str, &[BindKind])] = &[
    ("audio.fade", "fadeIn", &[BindKind::Time]),
    ("audio.fade", "fadeOut", &[BindKind::Time]),
    ("audio.set_loop", "loop", &[BindKind::Loop, BindKind::Time]),
    ("audio.trim", "end", &[BindKind::Time]),
    ("audio.trim", "start", &[BindKind::Time]),
    ("meta.set_grid", "grid", &[BindKind::Grid]),
    ("meta.set_pivot", "pivot", &[BindKind::Pivot]),
    ("sprite.split_grid", "durationMs", &[BindKind::FrameMs]),
    ("sprite.split_grid", "grid", &[BindKind::Grid]),
];

/// 新しい操作の担当（M14 段 1 の B）が印を付ける引数。B を合流するまでは印のない（free・結び付けなし）形も許す。
/// 合流したらこの表を消す（印が付いていれば、ほかの引数と同じく確かめる）。
const AWAITING_B: &[(&str, &str)] = &[("image.canvas", "rect"), ("meta.set_pivot", "pivot")];

/// 引数の種類から決まる印（None は「Ratio でなければよい」）。
fn expected_origin(op: &str, spec: &ParamSpec) -> Option<Origin> {
    match spec.kind {
        ParamKind::Rect
        | ParamKind::FrameRange
        | ParamKind::Strokes
        | ParamKind::PaletteEntries
        | ParamKind::Grid => Some(Origin::Source),
        ParamKind::Int { .. } if spec.unit == Unit::Samples => Some(Origin::Source),
        ParamKind::Point if FREE_POINTS.contains(&(op, spec.key)) => Some(Origin::Free),
        ParamKind::Point => Some(Origin::Source),
        ParamKind::Length | ParamKind::Margin | ParamKind::CanvasLength => Some(Origin::Ratio),
        _ => None,
    }
}

/// 出どころの種類と引数の種類の組（設計 §3.2 の表）。
fn binds_fit(kind: BindKind, spec: &ParamSpec) -> bool {
    match kind {
        BindKind::Grid => matches!(spec.kind, ParamKind::Grid),
        BindKind::Pivot => matches!(spec.kind, ParamKind::Point),
        BindKind::Loop => matches!(spec.kind, ParamKind::FrameRange),
        BindKind::FrameMs => matches!(spec.kind, ParamKind::Int { .. }) && spec.unit == Unit::Ms,
        BindKind::Time => {
            (matches!(spec.kind, ParamKind::Int { .. }) && spec.unit == Unit::Samples)
                || matches!(spec.kind, ParamKind::FrameRange)
        }
    }
}

#[test]
fn param_origins_and_binds_follow_the_kinds() {
    let engine = Engine::new().unwrap();
    let mut failures = Vec::new();
    let mut seen_bound: Vec<(String, String)> = Vec::new();
    let mut checked = 0;
    for op in engine.registry().all() {
        let id = op.id();
        for spec in op.schema().params {
            checked += 1;
            let at = format!("{id} v{} の {}", op.version(), spec.key);
            let awaiting = AWAITING_B.contains(&(id, spec.key))
                && spec.origin == Origin::Free
                && spec.binds.is_empty();
            if awaiting {
                continue;
            }
            match expected_origin(id, &spec) {
                Some(want) if spec.origin != want => {
                    failures.push(format!(
                        "{at}：origin は {want:?} のはずが {:?}",
                        spec.origin
                    ));
                }
                None if spec.origin == Origin::Ratio => {
                    failures.push(format!("{at}：この種類の引数は ratio にしません"));
                }
                _ => {}
            }
            for (i, kind) in spec.binds.iter().enumerate() {
                if !binds_fit(*kind, &spec) {
                    failures.push(format!(
                        "{at}：{kind:?} はこの種類の引数に結び付けられません"
                    ));
                }
                if spec.binds[..i].contains(kind) {
                    failures.push(format!("{at}：{kind:?} が 2 つあります"));
                }
            }
            let listed = BOUND.iter().find(|(o, k, _)| *o == id && *k == spec.key);
            match listed {
                Some((_, _, want)) if spec.binds != *want => {
                    failures.push(format!("{at}：binds は {want:?} のはずが {:?}", spec.binds));
                }
                None if !spec.binds.is_empty() => {
                    failures.push(format!(
                        "{at}：結び付けを置くなら BOUND の表に足す（{:?}）",
                        spec.binds
                    ));
                }
                _ => {}
            }
            if listed.is_some() {
                seen_bound.push((id.to_string(), spec.key.to_string()));
            }
        }
    }
    // 表の行はどれも登録表にある（消した引数の行を残さない）
    for (op, key, _) in BOUND {
        if !seen_bound.iter().any(|(o, k)| o == op && k == key) && !AWAITING_B.contains(&(op, key))
        {
            failures.push(format!("BOUND の {op} の {key} が登録表にありません"));
        }
    }
    assert!(checked > 30, "{checked}");
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// 印の確かめ自体が働くこと：種類ごとの決まりの表を、手で作った記述に当てる。
#[test]
fn the_rules_catch_missing_marks() {
    use kura_engine::op::spec;
    use serde_json::json;
    let rect = spec("rect", ParamKind::Rect, Unit::Px, json!({}));
    assert_eq!(expected_origin("x", &rect), Some(Origin::Source));
    let samples = spec(
        "start",
        ParamKind::Int {
            min: 0,
            max: 1,
            step: 1,
        },
        Unit::Samples,
        json!(0),
    );
    assert_eq!(expected_origin("x", &samples), Some(Origin::Source));
    let ms = spec(
        "durationMs",
        ParamKind::Int {
            min: 1,
            max: 2,
            step: 1,
        },
        Unit::Ms,
        json!(1),
    );
    assert_eq!(expected_origin("x", &ms), None);
    assert!(binds_fit(BindKind::FrameMs, &ms));
    assert!(!binds_fit(BindKind::Time, &ms));
    assert!(binds_fit(BindKind::Time, &samples));
    let point = spec("offset", ParamKind::Point, Unit::Px, json!(null));
    assert_eq!(
        expected_origin("meta.anchor_pivot", &point),
        Some(Origin::Free)
    );
    assert_eq!(
        expected_origin("meta.set_pivot", &point),
        Some(Origin::Source)
    );
    assert!(binds_fit(BindKind::Pivot, &point));
    assert!(!binds_fit(BindKind::Grid, &point));
    let range = spec("loop", ParamKind::FrameRange, Unit::Samples, json!(null));
    assert!(binds_fit(BindKind::Loop, &range) && binds_fit(BindKind::Time, &range));
    let margin = spec("top", ParamKind::Margin, Unit::Px, json!({"px": 0}));
    assert_eq!(expected_origin("x", &margin), Some(Origin::Ratio));
}
