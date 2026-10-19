//! ベンチの文書と入力（NFR-11〜15。requirements.md・docs/editing.md §16）。`tests/bench_edit.rs` と
//! `kura-devtools bench edit` が同じものを使う。入力は ChaCha8 と整数だけで作る（機械によらず同じ）。
//!
//! - NFR-11：引数を動かすスライダー（[`Sweep`]）。i 回目の手は i ごとに違う値にする（キャッシュに当てない）。ドット絵 2048² の
//!   等倍（[`pixel_sweeps`]）、音声 5 分（[`audio_sweeps`]。正規化の解析はキャッシュに当たる）、写真 24MP（[`photo_sweeps`]。
//!   縮小の段 [`PHOTO_LEVEL`] で判定し、段 1 も記録する。`EditorPath::render_preview` が `Engine::plan_level` で段の計画にする）。
//! - NFR-12：筆・塗りつぶしの文書（[`paint_doc`]・[`prior_strokes`]・[`stroke_item`]・[`fill_item`]・[`nearest4`]）。
//! - NFR-13：保存（[`save_doc`]・[`save_jpeg`]）。NFR-14：一括（[`bulk_doc`]・[`bulk_png`]、入力は `super::sprite_batch`）。
//!   NFR-15：解析（[`analysis_requests`]、入力は `super::program_audio`）。

use std::sync::Arc;

use serde_json::{Value as J, json};

use super::{pen, program_audio, walk_stroke};
use crate::analysis::AnalysisRequest;
use crate::digest::Digest;
use crate::doc::{Graph, NodeId};
use crate::error::EngineError;
use crate::geom::IPoint;
use crate::paint::{Connectivity, PaintAction, PaintColor};
use crate::session::Edit;
use crate::testgen::InputGen;
use crate::testing::source_of;
use crate::value::{PixelFormat, Value};

/// 写真のプレビューを描く段（24MP → 1500×1000。NFR-11 の判定の段。docs/editing.md §16）。
pub const PHOTO_LEVEL: u8 = 2;

/// i 回目の手を作る（スライダーの i 段目）。
pub type EditFn = Arc<dyn Fn(usize) -> Edit + Send + Sync>;

/// NFR-11 のケース：入力 1 つの文書と、そのノードの引数を動かす手。
pub struct Sweep {
    /// ケースの名前（JSON のレコードの `case`）。
    pub case: &'static str,
    /// 何を動かすか（表の項目）。
    pub item: &'static str,
    pub graph: Graph,
    pub input: Value,
    /// 元のハッシュの代わり（乱数の種）。
    pub source: Digest,
    /// 動かすノード（編集中のノード）。
    pub node: NodeId,
    /// i 回目の手。
    pub edit: EditFn,
    /// 描く段（写真は [`PHOTO_LEVEL`]、ほかは等倍の 0）。
    pub level: u8,
    /// 判定に使うか（偽は目標の範囲の外。測っても判定しない範囲外の記録。NFR-11 の減色のプレビュー）。
    pub judged: bool,
    /// 注（表の注に出す。なければ空）。
    pub note: &'static str,
}

fn err_json(e: serde_json::Error) -> EngineError {
    EngineError::Internal(format!("ベンチの文書を作れません: {e}"))
}

/// 入力 1 つにノード 1 つの文書と、そのノード。
fn one_node(op: &str, params: J) -> Result<(Graph, NodeId), EngineError> {
    let mut g = Graph::new_single();
    let node = g.push_linear(op, 1, params, 0)?;
    Ok((g, node))
}

fn set(node: NodeId, key: &'static str, f: impl Fn(usize) -> J + Send + Sync + 'static) -> EditFn {
    Arc::new(move |i| Edit::SetParam {
        node,
        key: key.into(),
        value: f(i),
    })
}

/// 引数を i ごとに変える操作 1 つのケース。
#[allow(clippy::too_many_arguments)]
fn sweep(
    case: &'static str,
    item: &'static str,
    op: &str,
    params: J,
    input: &Value,
    source: Digest,
    key: &'static str,
    f: impl Fn(usize) -> J + Send + Sync + 'static,
) -> Result<Sweep, EngineError> {
    let (graph, node) = one_node(op, params)?;
    Ok(Sweep {
        case,
        item,
        graph,
        input: input.clone(),
        source,
        node,
        edit: set(node, key, f),
        level: 0,
        judged: true,
        note: "",
    })
}

/// ドット絵（`side`²。等倍で判定）の 6 つの操作の引数を動かすケースと、参考の減色（Indexed8 への convert）。入力は
/// `InputGen::PixelSprite`（透明な余白つき）の Rgba8 と Indexed8。
pub fn pixel_sweeps(side: u32) -> Result<Vec<Sweep>, EngineError> {
    let side = side.max(16);
    let sprite = |fmt, seed| InputGen::PixelSprite {
        w: side,
        h: side,
        fmt,
        seed,
    };
    let (rgba_gen, indexed_gen) = (
        sprite(PixelFormat::Rgba8, 11),
        sprite(PixelFormat::Indexed8, 12),
    );
    let (rgba, indexed) = (rgba_gen.generate()?, indexed_gen.generate()?);
    let (rgba_src, indexed_src) = (source_of(&[rgba_gen])?, source_of(&[indexed_gen])?);
    let cell = (side / 32).max(8);
    let grid = move |c: u32| json!({ "cellW": c, "cellH": c, "offsetX": 0, "offsetY": 0, "gapX": 0, "gapY": 0 });
    let mut out = vec![
        sweep(
            "pixel-palette-replace",
            "パレットの色の差し替え（Indexed8）",
            "palette.replace",
            json!({ "entries": [{ "index": 1, "color": [0, 0, 0, 255] }] }),
            &indexed,
            indexed_src,
            "entries",
            |i| {
                json!([{
                    "index": 1 + i % 15,
                    "color": [i % 256, (i / 256) % 256, 77, 255],
                }])
            },
        )?,
        sweep(
            "pixel-convert",
            "形式の変換（Rgba8 → Rgb8 の下地の色）",
            "image.convert",
            json!({ "format": "rgb8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
            &rgba,
            rgba_src,
            "matte",
            |i| json!([i % 256, (i / 256) % 256, 200]),
        )?,
        sweep(
            "pixel-canvas",
            "キャンバスの大きさ（幅と高さを 1 ずつ）",
            "image.canvas",
            json!({ "rect": { "x": 0, "y": 0, "w": side, "h": side }, "fill": null }),
            &rgba,
            rgba_src,
            "rect",
            move |i| json!({ "x": 0, "y": 0, "w": side as usize + 1 + i, "h": side as usize + 1 + i }),
        )?,
        sweep(
            "pixel-nearest",
            "最近傍の拡大（2 倍前後の幅）",
            "image.resize",
            json!({ "width": { "px": side * 2 }, "height": "auto", "filter": "nearest" }),
            &rgba,
            rgba_src,
            "width",
            move |i| json!({ "px": (side as usize * 2).saturating_sub(1 + i % side as usize).max(1) }),
        )?,
        sweep(
            "pixel-trim",
            "透明な縁のトリム（しきい値）",
            "image.trim_alpha",
            json!({ "threshold": 0, "mode": "union" }),
            &rgba,
            rgba_src,
            "threshold",
            |i| json!(1 + i % 255),
        )?,
        sweep(
            "pixel-split",
            "格子の分割（セルの大きさ）",
            "sprite.split_grid",
            json!({ "grid": grid(cell), "durationMs": 100, "skipEmpty": false }),
            &rgba,
            rgba_src,
            "grid",
            move |i| grid(cell + 1 + (i as u32) % cell.max(1)),
        )?,
    ];
    let mut reduce = sweep(
        "pixel-convert-indexed8",
        "減色（Rgba8 → Indexed8、色の数）",
        "image.convert",
        json!({ "format": "indexed8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
        &rgba,
        rgba_src,
        "colors",
        |i| json!(255 - i % 200),
    )?;
    reduce.judged = false;
    reduce.note = super::OUT_OF_SCOPE_REDUCE;
    out.push(reduce);
    Ok(out)
}

/// 音声（`rate` Hz ステレオ `secs` 秒。`super::program_audio`）の利得・フェード・正規化の引数を動かすケース。正規化の
/// ラウドネスの解析は入力が同じなのでキャッシュに当たる（2 回目から解かない）。
pub fn audio_sweeps(rate: u32, secs: usize) -> Result<Vec<Sweep>, EngineError> {
    let audio = program_audio(rate, secs.max(2))?;
    let source = Digest([0x15; 32]);
    let rate_n = u64::from(rate);
    let mut normalize = sweep(
        "audio-normalize",
        "正規化の目標（解析はキャッシュに当たる）",
        "audio.normalize",
        json!({ "target": -16.0, "peakLimit": null }),
        &audio,
        source,
        "target",
        |i| json!(-16.0 - 0.1 * (i + 1) as f64),
    )?;
    normalize.note = "ラウドネスの解析はキャッシュに当たる";
    Ok(vec![
        sweep(
            "audio-gain",
            "利得（0.1 dB ずつ）",
            "audio.gain",
            json!({ "db": 0.0 }),
            &audio,
            source,
            "db",
            |i| json!(-0.1 * (i + 1) as f64),
        )?,
        sweep(
            "audio-fade",
            "フェードインの長さ（100 フレームずつ）",
            "audio.fade",
            json!({ "fadeIn": rate_n, "fadeOut": rate_n, "curve": "linear" }),
            &audio,
            source,
            "fadeIn",
            move |i| json!(rate_n + 100 * (i as u64 + 1)),
        )?,
        normalize,
    ])
}

/// 写真（`photo`。合成の `super::photo` か、bench edit の `--photo` の実写真）の切り抜き・Lanczos3 の縮小・鎖（切り抜き →
/// 縮小 → グレー）の途中の切り抜きを動かすケース。縮小の段 [`PHOTO_LEVEL`] で描く（`EditorPath::render_preview`）。
pub fn photo_sweeps(photo: Value) -> Result<Vec<Sweep>, EngineError> {
    let (w, h) = match &photo {
        Value::Image(img) => (img.px.width(), img.px.height()),
        _ => {
            return Err(EngineError::Internal("写真のベンチの入力は画像です".into()));
        }
    };
    let source = Digest([0x24; 32]);
    let m = (w / 4).min(h / 4).clamp(1, 64);
    let crop =
        move |i: usize| json!({ "x": (i as u32 + 1) % (m + 1), "y": 0, "w": w - m, "h": h - m });
    let mut out = vec![
        sweep(
            "photo-crop",
            "切り抜きの位置（1 画素ずつ）",
            "image.crop",
            json!({ "rect": crop(m as usize) }),
            &photo,
            source,
            "rect",
            crop,
        )?,
        sweep(
            "photo-resize",
            "Lanczos3 の縮小（40〜60%）",
            "image.resize",
            json!({ "width": { "px": w / 2 }, "height": "auto", "filter": "lanczos3" }),
            &photo,
            source,
            "width",
            move |i| json!({ "px": (w as usize * 2 / 5 + i % (w as usize / 5).max(1)).max(1) }),
        )?,
    ];
    let mut g = Graph::new_single();
    let node = g.push_linear("image.crop", 1, json!({ "rect": crop(m as usize) }), 0)?;
    g.push_linear(
        "image.resize",
        1,
        json!({ "width": { "scale": { "num": 1, "den": 2 } }, "height": "auto", "filter": "lanczos3" }),
        0,
    )?;
    g.push_linear(
        "image.convert",
        1,
        json!({ "format": "gray8", "colors": 256, "dither": "none", "matte": [255, 255, 255] }),
        0,
    )?;
    out.push(Sweep {
        case: "photo-chain",
        item: "鎖（切り抜き → Lanczos3 50% → グレー）の切り抜き",
        graph: g,
        input: photo.clone(),
        source,
        node,
        edit: set(node, "rect", crop),
        level: 0,
        judged: true,
        note: "",
    });
    for s in &mut out {
        s.level = PHOTO_LEVEL;
    }
    Ok(out)
}

/// 単色の `side`² の RGBA（全面がつながった色。塗りつぶしの最悪に近い。NFR-12 のキャンバス）。
pub fn flat_canvas(side: u32) -> Result<Value, EngineError> {
    InputGen::Checker {
        w: side,
        h: side,
        fmt: PixelFormat::Rgba8,
        cell: 1 << 15,
    }
    .generate()
}

/// 既にある筆 `n` 本（50 点・1 歩 ±12px・円 4px。5 万本でもノードの手間の上限に収まる大きさ）。
pub fn prior_strokes(n: usize, side: u32) -> Result<Vec<J>, EngineError> {
    (0..n)
        .map(|i| {
            let pts = walk_stroke(1_000 + i as u64, 50, side, side, 12);
            serde_json::to_value(pen(pts, 4, false, [(i % 251) as u8, 40, 200, 255]))
                .map_err(err_json)
        })
        .collect()
}

/// 足す筆 1 本（500 点・1 歩 ±12px。大きさ 1 はピクセルパーフェクト、ほかは円）。
pub fn stroke_item(seed: u64, size: u8, side: u32) -> Result<J, EngineError> {
    let pts = walk_stroke(seed, 500, side, side, 12);
    serde_json::to_value(pen(
        pts,
        size,
        size == 1,
        [200, (seed % 256) as u8, 30, 255],
    ))
    .map_err(err_json)
}

/// 全面の塗りつぶし 1 つ（左上から、4 連結。`contiguous` が偽なら画像全体の同じ色。色は i ごとに違う）。
pub fn fill_item(i: usize, tolerance: u8, contiguous: bool) -> Result<J, EngineError> {
    serde_json::to_value(PaintAction::Fill {
        at: IPoint { x: 0, y: 0 },
        color: PaintColor::Rgba([(i % 256) as u8, (i / 256 % 256) as u8, 3, 255]),
        tolerance,
        connectivity: Connectivity::Four,
        contiguous,
        frame: 0,
    })
    .map_err(err_json)
}

/// 筆の文書：入力 → 描画（`prior` の動作）→ `tail`（後ろの操作。なければ描画が最後）。描画のノードを返す。
pub fn paint_doc(prior: Vec<J>, tail: Option<(&str, J)>) -> Result<(Graph, NodeId), EngineError> {
    let mut g = Graph::new_single();
    let paint = g.push_linear("paint.actions", 1, json!({ "actions": prior }), 0)?;
    if let Some((op, params)) = tail {
        g.push_linear(op, 1, params, 0)?;
    }
    Ok((g, paint))
}

/// 筆のあとに続ける最近傍の 4 倍の拡大。
pub fn nearest4() -> (&'static str, J) {
    (
        "image.resize",
        json!({ "width": { "scale": { "num": 4, "den": 1 } }, "height": "auto", "filter": "nearest" }),
    )
}

/// 保存の文書（NFR-13）：線形光で 50% に縮める（`filter` は lanczos3・bilinear・box）。
pub fn save_doc(filter: &str) -> Result<Graph, EngineError> {
    Ok(one_node(
        "image.resize",
        json!({ "width": { "scale": { "num": 1, "den": 2 } }, "height": "auto", "filter": filter }),
    )?
    .0)
}

/// 保存の JPEG の設定（q90）。
pub fn save_jpeg() -> J {
    json!({ "quality": 90 })
}

/// 一括の文書（NFR-14）：透明な縁のトリム（合併）→ 最近傍の 4 倍。
pub fn bulk_doc() -> Result<Graph, EngineError> {
    let mut g = Graph::new_single();
    g.push_linear(
        "image.trim_alpha",
        1,
        json!({ "threshold": 0, "mode": "union" }),
        0,
    )?;
    let (op, params) = nearest4();
    g.push_linear(op, 1, params, 0)?;
    Ok(g)
}

/// 一括の PNG の設定（既定の圧縮）。
pub fn bulk_png() -> J {
    json!({ "compression": "balanced" })
}

/// 5 分の曲の解析（NFR-15）：波形（`audio.peaks`）と、ラウドネス（トゥルーピークを含む `audio.loudness`）。
pub fn analysis_requests() -> Vec<AnalysisRequest> {
    ["audio.peaks", "audio.loudness"]
        .into_iter()
        .map(AnalysisRequest::new)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::path::{EditorPath, Step, bulk_render, plain_inputs, single_input};
    use super::*;
    use crate::digest::Digest;
    use crate::doc::InputId;
    use crate::monitor::NoMonitor;
    use crate::render::Engine;
    use crate::session::CommitMode;

    fn pool(n: usize) -> rayon::ThreadPool {
        #[allow(clippy::disallowed_methods)]
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .unwrap()
    }

    /// エディタの経路（3 スレッド）の今の文書のダイジェストが、別のエンジンの一括の経路（1 スレッド）と同じ。
    fn assert_matches(engine: &Engine, ed: &mut EditorPath, what: &str) {
        #[allow(clippy::disallowed_methods)]
        let got = pool(3).install(|| ed.digests(engine)).unwrap();
        let fresh = Engine::new().unwrap();
        let want: Vec<Digest> = bulk_render(
            &fresh,
            &ed.document().unwrap(),
            &plain_inputs(ed.inputs()),
            &NoMonitor,
        )
        .unwrap()
        .outputs
        .iter()
        .map(|o| o.digest)
        .collect();
        assert_eq!(got, want, "{what}");
    }

    /// どのスライダーも描け、手ごとに違う文書になり（何もしない手がない）、エディタの経路が一括の経路と同じ値になる。
    #[test]
    fn sweeps_render_distinct_steps_and_match_the_bulk_path() {
        let photo = InputGen::Photo {
            w: 96,
            h: 64,
            seed: 1,
        }
        .generate()
        .unwrap();
        let sweeps: Vec<Sweep> = pixel_sweeps(64)
            .unwrap()
            .into_iter()
            .chain(audio_sweeps(8_000, 3).unwrap())
            .chain(photo_sweeps(photo).unwrap())
            .collect();
        let names: Vec<&str> = sweeps.iter().map(|s| s.case).collect();
        assert_eq!(names.len(), 13, "{names:?}");
        for s in &sweeps {
            let engine = Engine::new().unwrap();
            let mut ed = EditorPath::open(
                &engine,
                &s.graph,
                single_input(s.input.clone(), s.source).unwrap(),
                1 << 30,
                Some(s.node),
            )
            .unwrap();
            ed.render(&engine, false, &NoMonitor).unwrap();
            let mut revs = Vec::new();
            for i in 0..4 {
                revs.push(ed.apply(&engine, Step::Preview((s.edit)(i))).unwrap());
                let out = ed.render_preview(&engine, s.level).unwrap();
                // 動かしたノードは描き直す（段の計画の入力の縮小は当たってよい）
                assert!(
                    out.rendered.drawn_nodes() >= 1,
                    "{}: {i} 回目を描いていない",
                    s.case
                );
                assert_eq!(
                    out.level, s.level,
                    "{}: 段 {} で描けていない",
                    s.case, s.level
                );
                if s.level == 0 {
                    assert_eq!(out.rendered.stats.hits, 0, "{}: {i} 回目が当たった", s.case);
                }
            }
            revs.dedup();
            assert_eq!(revs.len(), 4, "{}: 同じ文書の手がある", s.case);
            if s.case == "audio-normalize" {
                assert!(ed.cache().stats().analysis_hits >= 3, "{}", s.case);
            }
            assert_matches(&engine, &mut ed, s.case);
            assert_eq!(
                s.level,
                if s.case.starts_with("photo") {
                    PHOTO_LEVEL
                } else {
                    0
                }
            );
        }
    }

    /// 筆の文書・最近傍 ×4 が続く文書・塗りつぶし（取り消して足し直す）で、確定ごとに差分の適用が効き、値が一括の経路と同じ。
    #[test]
    fn paint_docs_append_and_match_the_bulk_path() {
        let side = 64;
        for tail in [None, Some(nearest4())] {
            let (g, paint) = paint_doc(prior_strokes(5, side).unwrap(), tail.clone()).unwrap();
            let engine = Engine::new().unwrap();
            let mut ed = EditorPath::open(
                &engine,
                &g,
                single_input(flat_canvas(side).unwrap(), Digest([2; 32])).unwrap(),
                1 << 30,
                Some(paint),
            )
            .unwrap();
            ed.render(&engine, false, &NoMonitor).unwrap();
            let append = |item: J| {
                Step::Commit(
                    Edit::AppendItems {
                        node: paint,
                        field: "actions".into(),
                        items: vec![item],
                    },
                    CommitMode::Step,
                )
            };
            for (i, size) in [1u8, 8, 32].into_iter().enumerate() {
                let out = ed
                    .step(&engine, append(stroke_item(i as u64, size, side).unwrap()))
                    .unwrap();
                assert_eq!(out.stats.appended, 1, "筆 {size}px");
            }
            ed.step(&engine, append(fill_item(3, 0, true).unwrap()))
                .unwrap();
            ed.step(&engine, Step::Undo).unwrap();
            ed.step(&engine, append(fill_item(4, 0, false).unwrap()))
                .unwrap();
            assert_matches(&engine, &mut ed, &format!("{tail:?}"));
            assert_eq!(ed.session().history_len(), 5);
        }
    }

    /// 保存・一括の文書と解析の要求が、小さな入力で描ける・解ける（形は 50%・トリム → 4 倍）。
    #[test]
    fn save_bulk_and_analysis_cases_run() {
        let engine = Engine::new().unwrap();
        let photo = InputGen::Photo {
            w: 60,
            h: 40,
            seed: 2,
        }
        .generate()
        .unwrap();
        for filter in ["lanczos3", "bilinear", "box"] {
            let out = crate::testing::render_with(
                &engine,
                &save_doc(filter).unwrap(),
                photo.clone(),
                Digest::default(),
                &NoMonitor,
            )
            .unwrap();
            let Value::Image(img) = &out else { panic!() };
            assert_eq!((img.px.width(), img.px.height()), (30, 20));
        }
        let sprite = super::super::sprite_batch(2, 5);
        for g in &sprite {
            let v = g.generate().unwrap();
            let out = crate::testing::render_with(
                &engine,
                &bulk_doc().unwrap(),
                v,
                Digest::default(),
                &NoMonitor,
            )
            .unwrap();
            let Value::Image(img) = &out else { panic!() };
            assert_eq!(img.px.width() % 4, 0);
        }
        let audio = program_audio(8_000, 2).unwrap();
        let outs = engine.analyze_many(&audio, &analysis_requests(), &NoMonitor);
        assert_eq!(outs.len(), 2);
        assert!(outs.iter().all(Result::is_ok));
        let inputs = BTreeMap::from([(
            InputId(0),
            single_input(photo, Digest::default()).unwrap()[&InputId(0)].clone(),
        )]);
        assert_eq!(plain_inputs(&inputs).len(), 1);
    }
}
