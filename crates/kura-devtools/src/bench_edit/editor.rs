//! エディタの NFR（11：引数の変更 → プレビュー、12：筆・塗りつぶしの確定）と、その NFR-16 の照らし。
//!
//! 1 手の流れは M12 のエディタと同じ（kura-core の `EngineRuntime` の約束）：手の適用と差分の計画・段の計画（プレビューの
//! プール）→ 予算の確保（`admit_preview`。表示する計画で。プールの外・待たない）→ 表示の経路（`Engine::render_view`：前回の
//! 表示から変わったタイルだけを描き直し、変わったタイルの一覧を作る。プレビューのプール）→ 確保を戻す（`settle`）。測るのは
//! この全体（エンジンの中）。NFR-16 の照合は等倍の全体（`render_cached`）のダイジェストで。

use kura_core::edit::EngineRuntime;
use kura_engine::Digest;
use kura_engine::budget::default_reserved;
use kura_engine::doc::{Graph, NodeId};
use kura_engine::render::Engine;
use kura_engine::render::estimate::{CacheLease, EstimateSpec, Source};
use kura_engine::session::{CommitMode, Edit};
use kura_engine::testing::bench::cases::{
    self, Sweep, fill_item, flat_canvas, nearest4, paint_doc, prior_strokes, stroke_item,
};
use kura_engine::testing::bench::path::{
    EditorPath, LevelRender, Step, bulk_render, plain_inputs, single_input,
};
use kura_engine::testing::bench::{
    OUT_OF_SCOPE_STROKE, PHOTO_FALLBACK, measure, measure_prepared, stroke_in_scope,
};
use kura_engine::{EngineError, NoMonitor, Value};
use serde_json::json;

use super::report::{Record, Verdict};
use super::{Ctx, clock, load1};

/// エディタ 1 つ：エディタの経路（`EditorPath`）と、実行の場・キャッシュの分の確保。
pub(super) struct Editor<'r> {
    pub rt: &'r EngineRuntime,
    pub ed: EditorPath,
    pub lease: CacheLease,
    /// 描く間の確保の最大（キャッシュの上限の分 ＋ 見積もりのピーク）。
    pub max_lease: u64,
    /// 描く間に要る量の見積もりの最大（描く前にキャッシュが持つ値と入力 ＋ 見積もりのピーク）。NFR-17 で実測と比べる。
    pub max_need: u64,
}

impl<'r> Editor<'r> {
    /// 文書を開く：取り置き（min(1 GiB, 予算/2)）を置き、キャッシュの上限をその量にして確保し、段 `level` で最初の表示を
    /// する。
    pub fn open(
        rt: &'r EngineRuntime,
        graph: &Graph,
        input: Value,
        source: Digest,
        focus: NodeId,
        level: u8,
    ) -> Result<Self, EngineError> {
        let want = default_reserved(rt.budget().total());
        rt.budget().set_reserved(want)?;
        let inputs = single_input(input, source)?;
        let mut ed = rt.preview(|e| EditorPath::open(e, graph, inputs, want, Some(focus)))?;
        let lease = rt.open_cache(ed.cache_mut(), want)?;
        let max_lease = lease.bytes();
        let mut editor = Self {
            rt,
            ed,
            lease,
            max_lease,
            max_need: 0,
        };
        editor.render(level)?;
        Ok(editor)
    }

    /// 今の文書を段 `level` で表示する：段の計画（プレビューのプール）→ 確保（表示する計画で。プールの外）→ 表示の経路
    /// （プレビューのプール）→ 確保を戻す。
    pub fn render(&mut self, level: u8) -> Result<LevelRender, EngineError> {
        let ed = &mut self.ed;
        self.rt.preview(|e| ed.plan_view(e, level))?;
        let (plan, inputs, cache) = self.ed.view_parts();
        let held = cache.bytes();
        let spec = EstimateSpec::new(0).all_inputs(plan, Source::Held);
        let est = self
            .rt
            .admit_preview(plan, spec, inputs, cache, &mut self.lease)?;
        self.max_lease = self.max_lease.max(self.lease.bytes());
        self.max_need = self.max_need.max(held.saturating_add(est.peak));
        let ed = &mut self.ed;
        let out = self.rt.preview(|e| ed.render_view(e, &NoMonitor));
        self.lease.settle(self.ed.cache_mut())?;
        out
    }

    /// 手を当てて（差分の計画まで。プレビューのプール）描く。
    pub fn step(&mut self, step: Step, level: u8) -> Result<LevelRender, EngineError> {
        let ed = &mut self.ed;
        self.rt.preview(|e| ed.apply(e, step))?;
        self.render(level)
    }
}

impl Drop for Editor<'_> {
    fn drop(&mut self) {
        // エディタを閉じたら取り置きを外す（外せなくても次に開くときに置き直す）
        let _ = self.rt.budget().set_reserved(0);
    }
}

fn at(case: &str) -> impl Fn(EngineError) -> String + '_ {
    move |e| format!("{case}: {e}")
}

/// NFR-16：エディタの経路の今の文書を、別のエンジン・キャッシュなしの一括の経路（1・一括の数・16 スレッド）で描き直し、
/// 値のダイジェストと書き出し（固定のコーデックの PNG・WAV）のバイト列が同じかを記録する。
fn check_paths(
    ctx: &mut Ctx,
    editor: &mut Editor<'_>,
    case: &str,
    item: &str,
    threads: usize,
) -> Result<(), String> {
    let load = load1();
    let rt = editor.rt;
    let ed = &mut editor.ed;
    let got = rt
        .preview(|e| ed.render(e, true, &NoMonitor))
        .map_err(at(case))?;
    let doc = editor.ed.document().map_err(at(case))?;
    let inputs = plain_inputs(editor.ed.inputs());
    let mut ok = true;
    let mut notes = Vec::new();
    let encode = |v: &Value| {
        let format = match v {
            Value::Audio(_) => "wav",
            _ => "png",
        };
        ctx.pinned
            .encode_bytes(v, format, &json!({}), &NoMonitor)
            .map(|(b, _)| Digest(*blake3::hash(&b).as_bytes()))
    };
    let mine: Vec<Option<Digest>> = got.outputs.iter().map(|o| o.digest).collect();
    let mine_bytes: Vec<Result<Digest, EngineError>> =
        got.outputs.iter().map(|o| encode(&o.value)).collect();
    for t in ctx.check_threads() {
        let fresh = Engine::new().map_err(at(case))?;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(t)
            .build()
            .map_err(|e| e.to_string())?;
        let want = pool
            .install(|| bulk_render(&fresh, &doc, &inputs, &NoMonitor))
            .map_err(at(case))?;
        let digests: Vec<Option<Digest>> = want.outputs.iter().map(|o| Some(o.digest)).collect();
        if digests != mine {
            ok = false;
            notes.push(format!("一括の経路（{t} スレッド）とダイジェストが違う"));
        }
        for (o, m) in want.outputs.iter().zip(&mine_bytes) {
            match (encode(&o.value), m) {
                (Ok(a), Ok(b)) if a == *b => {}
                (Ok(_), Ok(_)) => {
                    ok = false;
                    notes.push(format!(
                        "一括の経路（{t} スレッド）と書き出しのバイト列が違う"
                    ));
                }
                (Err(e), _) => {
                    let n = format!("書き出しは照らさない（{e}）");
                    if !notes.contains(&n) {
                        notes.push(n);
                    }
                }
                (_, Err(e)) => {
                    let n = format!("書き出しは照らさない（{e}）");
                    if !notes.contains(&n) {
                        notes.push(n);
                    }
                }
            }
        }
    }
    let list: Vec<String> = ctx.check_threads().iter().map(|t| t.to_string()).collect();
    let paths = format!(
        "エディタ（{threads} スレッド・キャッシュ・差分）＝一括（{} スレッド）",
        list.join("・")
    );
    let digest = mine
        .first()
        .copied()
        .flatten()
        .map(|d| d.to_hex())
        .unwrap_or_default();
    let r = Record::check(
        ctx.run,
        16,
        "determinism",
        case,
        item,
        threads,
        if ok { Verdict::Pass } else { Verdict::Fail },
        notes.join("・"),
        load,
        json!({ "paths": paths, "digest": digest }),
    );
    ctx.push(r);
    Ok(())
}

/// スライダー 1 つ：仮の値を出して段 `level` で表示する（i 回目は i ごとに違う値）。`level` が `s.level` でなければ記録だけ
/// （写真の段 1）。
#[allow(clippy::too_many_arguments)]
fn run_sweep(
    ctx: &mut Ctx,
    rt: &EngineRuntime,
    threads: usize,
    s: &Sweep,
    level: u8,
    timed: bool,
    check: bool,
    photo_note: &str,
) -> Result<(), String> {
    let load = load1();
    let judged = threads == ctx.opts.preview_threads;
    let (case, item) = if level == s.level {
        (s.case.to_string(), s.item.to_string())
    } else {
        (format!("{}-level{level}", s.case), s.item.to_string())
    };
    let item = if level > 0 {
        format!("{item}（段 {level}）")
    } else {
        item
    };
    let mut editor =
        Editor::open(rt, &s.graph, s.input.clone(), s.source, s.node, level).map_err(at(&case))?;
    let (warmup, n) = if timed {
        (ctx.sizes.edit_warmup, ctx.sizes.edit_n)
    } else {
        (0, 1)
    };
    let c = clock();
    let mut levels = Vec::new();
    let mut analysis_solved = 0;
    let stats = measure(&c, warmup, n, |i| {
        let out = editor.step(Step::Preview((s.edit)(i)), level)?;
        levels.push(out.level);
        if i >= warmup {
            analysis_solved += out.rendered.stats.analysis_solved;
        }
        Ok(out)
    })
    .map_err(at(&case))?;
    if timed {
        let fallback = level > 0 && levels.iter().any(|l| *l != level);
        let mut r = Record::timed(
            ctx.run, 11, "preview", &case, &item, threads, stats, judged, load,
        )
        .with_note(s.note)
        .with_note(photo_note);
        r.stage = levels.last().copied();
        if fallback {
            r = r.with_note(PHOTO_FALLBACK).into_reference();
        }
        if !s.judged {
            // 決めた範囲の外（減色のプレビュー。注は Sweep の note）
            r = r.out_of_scope();
        } else if level != s.level {
            r = r.into_reference();
        }
        if s.case == "audio-normalize" {
            r.extra = json!({ "analysis_solved": analysis_solved });
            if analysis_solved > 0 {
                r = r.with_note(&format!("測った回で解析を {analysis_solved} 回解いた"));
            }
        }
        ctx.push(r);
    }
    if check && judged && level == s.level {
        check_paths(ctx, &mut editor, &case, &item, threads)?;
    }
    Ok(())
}

/// NFR-11：ドット絵の等倍・音声・写真（縮小の段 2 で判定し、段 1 も記録）の引数の変更。
pub(super) fn preview(ctx: &mut Ctx, timed: bool, check: bool) -> Result<(), String> {
    let z = ctx.sizes.clone();
    let photo = ctx.photo()?.clone();
    let photo_note = if photo.suffix.is_empty() {
        String::new()
    } else {
        photo.label.clone()
    };
    let groups: Vec<(Vec<Sweep>, String)> = vec![
        (
            cases::pixel_sweeps(z.pixel).map_err(|e| e.to_string())?,
            String::new(),
        ),
        (
            cases::audio_sweeps(z.audio_rate, z.audio_secs).map_err(|e| e.to_string())?,
            String::new(),
        ),
        (
            cases::photo_sweeps(photo.value).map_err(|e| e.to_string())?,
            photo_note,
        ),
    ];
    for threads in ctx.preview_set() {
        let rt = ctx.runtime(threads)?;
        for (sweeps, note) in &groups {
            for s in sweeps {
                run_sweep(ctx, &rt, threads, s, s.level, timed, check, note)?;
                // 写真は段 1 も記録する（段 2 の判定の参考）
                if s.level > 1 {
                    run_sweep(ctx, &rt, threads, s, 1, timed, false, note)?;
                }
            }
        }
    }
    Ok(())
}

/// 描画のノードに 1 つ足して確定する手。
fn append(paint: NodeId, item: serde_json::Value) -> Step {
    Step::Commit(
        Edit::AppendItems {
            node: paint,
            field: "actions".into(),
            items: vec![item],
        },
        CommitMode::Step,
    )
}

/// 筆を `n` 本足して測る（1 本ごとに確定）。
#[allow(clippy::too_many_arguments)]
fn strokes(
    ctx: &mut Ctx,
    editor: &mut Editor<'_>,
    paint: NodeId,
    case: &str,
    item: &str,
    threads: usize,
    side: u32,
    size: u8,
    seed: u64,
    judged: bool,
    timed: bool,
) -> Result<(), String> {
    let load = load1();
    let (warmup, n) = if timed {
        (ctx.sizes.edit_warmup, ctx.sizes.edit_n)
    } else {
        (0, 1)
    };
    let c = clock();
    let mut appended = 0;
    let stats = measure(&c, warmup, n, |i| {
        let item = stroke_item(seed + i as u64 * 31 + u64::from(size), size, side)?;
        let out = editor.step(append(paint, item), 0)?;
        appended += out.rendered.stats.appended;
        Ok(out)
    })
    .map_err(at(case))?;
    if timed {
        let mut r = Record::timed(
            ctx.run, 12, "stroke", case, item, threads, stats, judged, load,
        );
        r.extra = json!({ "appended": appended, "steps": warmup + n });
        if !stroke_in_scope(size) {
            r = r.with_note(OUT_OF_SCOPE_STROKE).out_of_scope();
        }
        ctx.push(r);
    }
    Ok(())
}

/// NFR-12：4096² に 0・999・5 万本を描いたあとの筆、筆のあとに最近傍 ×4 が続く文書、全面の塗りつぶし。
pub(super) fn paint(ctx: &mut Ctx, timed: bool, check: bool) -> Result<(), String> {
    let z = ctx.sizes.clone();
    let canvas = flat_canvas(z.canvas).map_err(|e| e.to_string())?;
    let source = Digest([2; 32]);
    for threads in ctx.preview_set() {
        let rt = ctx.runtime(threads)?;
        let judged = threads == ctx.opts.preview_threads;
        for &prior in &z.priors {
            let (g, paint) = paint_doc(
                prior_strokes(prior, z.canvas).map_err(|e| e.to_string())?,
                None,
            )
            .map_err(|e| e.to_string())?;
            let case = format!("stroke-{prior}");
            let mut editor =
                Editor::open(&rt, &g, canvas.clone(), source, paint, 0).map_err(at(&case))?;
            for &size in &z.stroke_sizes {
                strokes(
                    ctx,
                    &mut editor,
                    paint,
                    &format!("stroke-{prior}-{size}px"),
                    &format!(
                        "筆 1 本（500 点）の確定（{0}²・前の筆 {prior} 本・{size}px）",
                        z.canvas
                    ),
                    threads,
                    z.canvas,
                    size,
                    7_000,
                    judged,
                    timed,
                )?;
            }
            if check && judged {
                check_paths(
                    ctx,
                    &mut editor,
                    &case,
                    &format!("筆の文書（{0}²・前の筆 {prior} 本 ＋ 足した筆）", z.canvas),
                    threads,
                )?;
            }
        }
        for &side in &z.nearest_sides {
            let small = flat_canvas(side).map_err(|e| e.to_string())?;
            let (g, paint) = paint_doc(
                prior_strokes(z.priors.get(1).copied().unwrap_or(0), side)
                    .map_err(|e| e.to_string())?,
                Some(nearest4()),
            )
            .map_err(|e| e.to_string())?;
            let case = format!("stroke-nearest4-{side}");
            let mut editor = Editor::open(&rt, &g, small, source, paint, 0).map_err(at(&case))?;
            for &size in &z.nearest_strokes {
                strokes(
                    ctx,
                    &mut editor,
                    paint,
                    &format!("stroke-nearest4-{side}-{size}px"),
                    &format!("筆のあとに最近傍 ×4（{side}² → {}²・{size}px）", side * 4),
                    threads,
                    side,
                    size,
                    9_000,
                    judged && side * 4 <= z.canvas,
                    timed,
                )?;
            }
            if check && judged {
                check_paths(
                    ctx,
                    &mut editor,
                    &case,
                    &format!("筆のあとに最近傍 ×4 の文書（{side}²）"),
                    threads,
                )?;
            }
        }
        let (g, paint) = paint_doc(Vec::new(), None).map_err(|e| e.to_string())?;
        let mut editor =
            Editor::open(&rt, &g, canvas.clone(), source, paint, 0).map_err(at("fill"))?;
        for contiguous in [true, false] {
            let case = if contiguous {
                "fill-contiguous"
            } else {
                "fill-global"
            };
            let load = load1();
            let (warmup, n) = if timed {
                (z.edit_warmup, z.edit_n)
            } else {
                (0, 1)
            };
            let c = clock();
            // 1 手ごとに測らずに取り消して、同じ単色の画像に塗る
            let stats = measure_prepared(
                &c,
                warmup,
                n,
                &mut editor,
                |ed, _| {
                    if ed.ed.session().can_undo() {
                        ed.step(Step::Undo, 0)?;
                    }
                    Ok(())
                },
                |ed, i| ed.step(append(paint, fill_item(i, 0, contiguous)?), 0),
            )
            .map_err(at(case))?;
            if timed {
                ctx.push(Record::timed(
                    ctx.run,
                    12,
                    "fill",
                    case,
                    &format!(
                        "全面の塗りつぶし（{0}²・{1}）",
                        z.canvas,
                        if contiguous {
                            "4 連結のつながった全面"
                        } else {
                            "画像全体の同じ色"
                        }
                    ),
                    threads,
                    stats,
                    judged,
                    load,
                ));
            }
        }
        if check && judged {
            check_paths(ctx, &mut editor, "fill", "塗りつぶしの文書", threads)?;
        }
    }
    Ok(())
}
