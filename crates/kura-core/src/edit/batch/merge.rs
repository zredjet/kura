//! N → 1：選んだアイテムから 1 枚のスプライトシート（新しいアイテム）を作る一括（M14 段 6。docs/editing.md §14.2「N → 1」、
//! docs/data-model.md §4「一括加工（M14）」、ADR-197）。
//!
//! 一括のジョブ（`JobKind::Batch`。入力の向きは `merge`。レシピは持たない）で、行は選んだアイテム（並べる順）。1 本の単位として：
//! 1. 飛ばす（行は skipped）：ゴミ箱・画像でない（音声はシートにできない）・前の保存が終わっていない。元を読むだけなので、エディタで
//!    開いている・下書きのあるアイテムは飛ばさない（今の中身で作る）。前回 OS のデコーダで読む途中で落ちた行は failed（Crashed）。
//! 2. 今の中身の形を読み（段の順。OS の段なら行の印）、選択の形（`selection::shape`）とシートの文書（`selection::sheet_graph`）で
//!    計画し、メモリ（読んだ値の和 ＋ 描く・書く見積もり）を一括の枠で確保する。読めないアイテムは failed、シートにならない
//!    （大きすぎる・予算を超える）ならどの行も failed。
//! 3. 今の中身を読み、選択の値（`selection::frames`。アニメはすべてのフレーム、揃えは 9 点）にしてシートを描き、PNG に書いて
//!    staging に置く（fsync と区切りの同期）。乱数の元は選んだ順の中身のハッシュの列のダイジェスト（`selection::source`）。
//! 4. 予告 → 置く → トランザクション 2：新しいアイテム（元はできたファイル。版の行は持たない）・来歴 `item_sources`（選んだ順に
//!    元のアイテム・頭の版・読んだ中身・フレームの数）・素材の設定の格子（セル＝選択のキャンバス、間隔、コマの数）・行（saved と
//!    out_item_id）を書く。どの行もこのトランザクションで saved になるので、落ちても二重にも抜けにもならない（落ちたら pending の
//!    行から作り直す）。同じ中身のアイテムがあれば作らずに existing。
//!
//! 止め方：一時停止・譲るは読む途中でも止め（行は pending のまま。読み直す）、中止は残りの行を canceled にして作らない。

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use kura_engine::budget::Class;
use kura_engine::codec::Tier;
use kura_engine::doc::{DecodeOptions, InputId, OutputId};
use kura_engine::render::InputValue;
use kura_engine::render::estimate::{EstimateSpec, Sink, Source};
use kura_engine::selection::{self, SheetParams};
use kura_engine::{EngineError, Shape, Value};
use rusqlite::params;
use serde_json::json;

use super::{
    BatchHandler, BatchInput, BatchUnit, INPUT_V, InputRead, JobShape, ModeInput, Skip, Stop,
    dedup, finish_canceled, finish_row, insert_rows, observe, queue_follow_ups, save_as_names,
    staged_shape, summary,
};
use crate::asset::{self, Change, GridSetting, SettingsPatch};
use crate::db::Db;
use crate::edit::from_engine;
use crate::edit::save::{Flow, UnitCx, UnitMonitor, undo_placement};
use crate::import::place::{Staged, place};
use crate::import::store::{NewItem, find_owner, insert_item};
use crate::import::{MediaKind, format_id, mime_for};
use crate::jobs::{JobContext, JobEnd, JobId, JobKind, JobManager};
use crate::library::ledger;
use crate::media::MediaInfo;
use crate::text::nfc;
use crate::time::now_ms;
use crate::{CoreError, Result};

/// できるアイテムの名前の既定（`{名前}` は最初のアイテムの表示名）。
pub const DEFAULT_SHEET_NAME: &str = "{名前} のシート";

/// N → 1 の頼み（[`start_merge`]・`precheck::precheck_merge`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRequest {
    /// 並べるアイテム（並べる順。重なりは前のものだけ残す）。
    pub items: Vec<i64>,
    /// できるアイテムの名前（表示名とファイル名の語幹。`{名前}` は最初に並べたアイテムの表示名）。
    pub name: String,
    pub sheet: SheetParams,
}

/// N → 1 の一括を積む（モジュールの説明）。名前が空・対象がない（どれもライブラリにない）なら Invalid。
pub fn start_merge(jobs: &JobManager, req: &MergeRequest) -> Result<JobId> {
    let name = nfc(req.name.trim());
    if name.is_empty() {
        return Err(CoreError::Invalid("シートの名前が空です".into()));
    }
    let items = dedup(&req.items);
    if items.is_empty() {
        return Err(CoreError::Invalid("並べるアイテムがありません".into()));
    }
    let input = BatchInput {
        v: INPUT_V,
        recipe: None,
        mode: ModeInput::Merge {
            name,
            sheet: req.sheet,
        },
        include_drafts: false,
    };
    let input_json = serde_json::to_string(&input)?;
    let now = now_ms();
    jobs.enqueue_then(JobKind::Batch, input_json, move |tx, job| {
        // 元を読むだけなので、エディタで開いているアイテムも並べる
        if insert_rows(tx, job, &items, None, now)? == 0 {
            return Err(CoreError::Invalid(
                "並べるアイテムがありません（どれもライブラリにありません）".into(),
            ));
        }
        Ok(())
    })
}

/// N → 1 で飛ばす決まり（ゴミ箱・画像でない・前の保存。開いている・下書きは飛ばさない）。並べられるなら頭。
pub(super) fn merge_skip(
    engine: &crate::edit::EngineServices,
    db: &Db,
    item: i64,
) -> std::result::Result<crate::edit::versions::Head, Skip> {
    let head = super::skip_rules_with(engine, db, item, false, false)?;
    if head.kind != MediaKind::Image {
        return Err(Skip::skipped(
            "Kind",
            format!(
                "{} はスプライトシートにできません（画像とアニメだけ）",
                head.kind.as_str()
            ),
        ));
    }
    Ok(head)
}

/// 並べる 1 件（今の中身の形を読んだもの）。
pub(super) struct Piece {
    pub(super) ord: i64,
    pub(super) item: i64,
    /// 読んだ今の中身と頭の版。
    pub(super) hash: [u8; 32],
    pub(super) version: i64,
    pub(super) size: u64,
    pub(super) title: String,
    pub(super) read: InputRead,
}

/// 頭 `head` のアイテムの今の中身の形を読む（段の順。`hooks` で予算と印）。読めなければ Err（行の失敗）。
pub(super) fn read_piece(
    engine: &crate::edit::EngineServices,
    library: &crate::library::Library,
    ord: i64,
    item: i64,
    head: &crate::edit::versions::Head,
    hooks: &dyn super::ShapeHooks,
) -> Result<Flow<Piece>> {
    let db = library.db();
    let format = format_id(&head.ext)
        .ok_or_else(|| CoreError::Unsupported(format!("形式（{}）を読めません", head.ext)))?;
    let (title, size): (String, i64) = db.read(|c| {
        Ok(
            c.prepare_cached("SELECT title, size_bytes FROM items WHERE id = ?1")?
                .query_row([item], |r| Ok((r.get(0)?, r.get(1)?)))?,
        )
    })?;
    let path = library.layout().content_path(&head.hash, &head.ext);
    let read = match staged_shape(engine, path, format, DecodeOptions::default(), hooks)? {
        Flow::Go(read) => read,
        Flow::Stop(end) => return Ok(Flow::Stop(end)),
    };
    if matches!(read.shape, Shape::Audio { .. }) {
        return Err(CoreError::Unfit(
            "音声はスプライトシートにできません（画像とアニメだけ）".into(),
        ));
    }
    Ok(Flow::Go(Piece {
        ord,
        item,
        hash: head.hash,
        version: head.version,
        size: u64::try_from(size).unwrap_or(0),
        title,
        read,
    }))
}

/// シートの計画（選択の形 → 文書 → compile）と、描く・書く間のメモリの見積もり（読んだ値の和 ＋ 読む作業の最大 ＋ 計画の見積もり）。
pub(super) struct SheetPlan {
    /// シートの形。
    pub(super) out: Shape,
    /// 選択の形（フレームの列）。
    pub(super) selection: Shape,
    pub(super) columns: u32,
    pub(super) need: u64,
}

/// 並べる形の列 `shapes` からシートを計画する（描かない）。シートにならないなら Unfit・Invalid、見積もりが予算の全体を超えるなら
/// TooLarge。
pub(super) fn plan_sheet(
    engine: &crate::edit::EngineServices,
    pieces: &[(Shape, &'static str)],
    sheet: &SheetParams,
) -> Result<SheetPlan> {
    let runtime = engine.runtime()?.clone();
    let codecs = engine.codecs();
    let shapes: Vec<Shape> = pieces.iter().map(|(s, _)| *s).collect();
    let selection = selection::shape(&shapes)
        .map_err(|e| CoreError::Unfit(format!("シートにできません：{e}")))?;
    let count = match selection {
        Shape::Frames { count, .. } => count.max(),
        _ => 1,
    };
    let columns = selection::columns_for(count, sheet.columns);
    let graph = selection::sheet_graph(sheet, count)
        .map_err(|e| from_engine("シートの文書を作れません", e))?;
    let plan = runtime
        .bulk(|e| e.compile(&graph, &BTreeMap::from([(InputId(0), selection)])))
        .map_err(|issues| CoreError::Unfit(format!("シートを描けません：{issues}")))?;
    let out = plan
        .output_shapes()
        .first()
        .map(|(_, s)| *s)
        .ok_or_else(|| CoreError::Invalid("シートの文書に出力がありません".into()))?;
    let opts = json!({});
    let spec = EstimateSpec::new(runtime.bulk_threads() as u64)
        .digests(true)
        .input(InputId(0), Source::Memory)
        .output(
            OutputId(0),
            Sink::Encode {
                codecs: &codecs,
                format: "png",
                opts: &opts,
            },
        );
    let est = runtime
        .bulk(|e| e.estimate(&plan, &spec, None))
        .map_err(|e| from_engine("シートのメモリを見積もれません", e))?;
    // 読んだ値は選択の値を作るまで持つ（形式を広げるなら写しも）。読む間の作業は 1 件ずつ
    let values: u64 = shapes
        .iter()
        .fold(0u64, |a, s| a.saturating_add(s.value_bytes()));
    let decode = pieces
        .iter()
        .map(|(s, f)| codecs.decode_work_bytes(f, s))
        .max()
        .unwrap_or(0);
    let need = est.peak.saturating_add(values).saturating_add(decode);
    let total = runtime.budget().total();
    if need > total {
        return Err(CoreError::TooLarge(
            "メモリの予算を超える見込みなので、シートを作れません".into(),
        ));
    }
    Ok(SheetPlan {
        out,
        selection,
        columns,
        need,
    })
}

/// できたシート（staging に書いたもの）。
struct Made {
    part: std::path::PathBuf,
    hash: [u8; 32],
    size: u64,
    width: u32,
    height: u32,
    grid: GridSetting,
}

impl BatchHandler {
    /// N → 1 の一括を動かす（モジュールの説明）。
    pub(super) fn run_merge(
        &self,
        ctx: &JobContext,
        name: &str,
        sheet: &SheetParams,
    ) -> Result<JobEnd> {
        let db = ctx.library().db();
        let job = ctx.id();
        let durability = self.engine.durability();
        let yielding = AtomicBool::new(false);
        let rows: Vec<(i64, i64)> = db.read(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT ord, item_id FROM batch_items WHERE job_id = ?1 AND state = 'pending' ORDER BY ord",
            )?;
            let rows = stmt.query_map([job], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<std::result::Result<_, _>>()?)
        })?;
        let total = rows.len();
        let stopped = |stop: Stop| -> Result<JobEnd> {
            match stop {
                Stop::Canceled => finish_canceled(db, job),
                Stop::Paused => Ok(JobEnd::Paused),
                Stop::Yield => Ok(JobEnd::Yield),
                Stop::Finished => Ok(JobEnd::Paused),
            }
        };
        // 1・2. 飛ばす・今の中身の形を読む（順に）
        let mut pieces: Vec<Piece> = Vec::new();
        for (k, (ord, item)) in rows.into_iter().enumerate() {
            if let Some(stop) = observe(ctx, None) {
                return stopped(stop);
            }
            ctx.progress(
                0.5 * k as f64 / total.max(1) as f64,
                &format!("元を確かめています（{k} / {total}）"),
            );
            let head = match merge_skip(&self.engine, db, item) {
                Ok(head) => head,
                Err(skip) => {
                    finish_row(db, job, ord, skip.state, &skip.code, &skip.message)?;
                    continue;
                }
            };
            let unit = BatchUnit {
                ctx,
                ord,
                durability: durability.as_ref(),
                yielding: &yielding,
            };
            let runtime = self.engine.runtime()?.clone();
            let hooks = JobShape {
                unit: &unit,
                mon: UnitMonitor(&unit),
                runtime: &runtime,
            };
            match read_piece(&self.engine, ctx.library(), ord, item, &head, &hooks) {
                Ok(Flow::Go(piece)) => pieces.push(piece),
                Ok(Flow::Stop(_)) => {
                    return stopped(observe(ctx, None).unwrap_or(Stop::Paused));
                }
                Err(e) => {
                    let skip = match &e {
                        CoreError::Unfit(_) => Skip::skipped("Kind", e.to_string()),
                        _ => Skip::failed(&e),
                    };
                    finish_row(db, job, ord, skip.state, &skip.code, &skip.message)?;
                }
            }
        }
        if pieces.is_empty() {
            return self.merge_done(ctx, None);
        }
        let fail_all = |pieces: &[Piece], e: &CoreError| -> Result<JobEnd> {
            for p in pieces {
                finish_row(db, job, p.ord, "failed", e.code(), &e.to_string())?;
            }
            self.merge_done(ctx, None)
        };
        // 2. 計画と予算
        let shapes: Vec<(Shape, &'static str)> = pieces
            .iter()
            .map(|p| (p.read.shape, p.read.format))
            .collect();
        let planned = match plan_sheet(&self.engine, &shapes, sheet) {
            Ok(p) => p,
            Err(e) => return fail_all(&pieces, &e),
        };
        let runtime = self.engine.runtime()?.clone();
        let first = BatchUnit {
            ctx,
            ord: pieces[0].ord,
            durability: durability.as_ref(),
            yielding: &yielding,
        };
        let mon = UnitMonitor(&first);
        let lease = match runtime.budget().acquire(planned.need, Class::Bulk, &mon) {
            Ok(lease) => lease,
            Err(e) if matches!(e.root(), EngineError::Canceled) => {
                return stopped(observe(ctx, None).unwrap_or(Stop::Paused));
            }
            Err(e) => return fail_all(&pieces, &from_engine("シートのメモリを確保できません", e)),
        };
        // 3. 今の中身を読む
        let codecs = self.engine.codecs();
        let limits = runtime.bulk(|e| e.limits().clone());
        let mut used: Vec<(Piece, Value)> = Vec::new();
        let n = pieces.len();
        for (k, piece) in pieces.into_iter().enumerate() {
            if let Some(stop) = observe(ctx, None) {
                return stopped(stop);
            }
            ctx.progress(
                0.5 + 0.3 * k as f64 / n.max(1) as f64,
                &format!("元を読んでいます（{k} / {n}）"),
            );
            let unit = BatchUnit {
                ctx,
                ord: piece.ord,
                durability: durability.as_ref(),
                yielding: &yielding,
            };
            let platform = codecs
                .decoders_for(piece.read.format)
                .iter()
                .any(|d| d.tier() == Tier::Platform);
            if platform {
                unit.mark(super::DECODING, 0.0, "元を読んでいます")?;
            }
            let read = runtime.bulk(|_| {
                codecs.decode_path(
                    &piece.read.path,
                    piece.read.format,
                    &DecodeOptions::default(),
                    &limits,
                    &UnitMonitor(&unit),
                )
            });
            if platform {
                unit.mark(super::DECODED, 0.0, "元を読みました")?;
            }
            match read {
                Ok((value, _)) if !matches!(value, Value::Audio(_)) => used.push((piece, value)),
                Ok(_) => finish_row(
                    db,
                    job,
                    piece.ord,
                    "skipped",
                    "Kind",
                    "音声はスプライトシートにできません（画像とアニメだけ）",
                )?,
                Err(e) if matches!(e.root(), EngineError::Canceled) => {
                    return stopped(observe(ctx, None).unwrap_or(Stop::Paused));
                }
                Err(e) => {
                    let e = from_engine("元を読めません", e);
                    finish_row(db, job, piece.ord, "failed", e.code(), &e.to_string())?;
                }
            }
        }
        if used.is_empty() {
            return self.merge_done(ctx, None);
        }
        // 4. 描いて書く
        ctx.progress(0.85, "シートを描いています");
        let made = match self.draw_sheet(ctx, &first, &used, sheet) {
            Ok(Flow::Go(made)) => made,
            Ok(Flow::Stop(_)) => return stopped(observe(ctx, None).unwrap_or(Stop::Paused)),
            Err(e) => {
                let pieces: Vec<Piece> = used.into_iter().map(|(p, _)| p).collect();
                return fail_all(&pieces, &e);
            }
        };
        drop(lease);
        if let Some(stop) = observe(ctx, None) {
            let _ = std::fs::remove_file(&made.part);
            return stopped(stop);
        }
        // 5. 予告 → 置く → コミット
        ctx.progress(0.95, "シートを置いています");
        let names = save_as_names(name, &used[0].0.title);
        let made_id = self.commit_merge(ctx, &used, &made, &names)?;
        self.merge_done(ctx, made_id)
    }

    /// 選択の値にしてシートを描き、PNG に書いて staging に置く（fsync と区切りの同期）。
    fn draw_sheet(
        &self,
        ctx: &JobContext,
        unit: &BatchUnit<'_>,
        used: &[(Piece, Value)],
        sheet: &SheetParams,
    ) -> Result<Flow<Made>> {
        let runtime = self.engine.runtime()?.clone();
        let codecs = self.engine.codecs();
        let mon = UnitMonitor(unit);
        let values: Vec<Value> = used.iter().map(|(_, v)| v.clone()).collect();
        let frames = match runtime.bulk(|_| selection::frames(&values, sheet.anchor, &mon)) {
            Ok(f) => f,
            Err(e) if matches!(e.root(), EngineError::Canceled) => {
                return Ok(Flow::Stop(JobEnd::Paused));
            }
            Err(e) => return Err(from_engine("シートにできません", e)),
        };
        drop(values);
        let (canvas, count) = (frames.canvas(), frames.frames().len() as u64);
        let value = Value::Frames(Arc::new(frames));
        let graph = selection::sheet_graph(sheet, count)
            .map_err(|e| from_engine("シートの文書を作れません", e))?;
        let plan = runtime
            .bulk(|e| e.compile(&graph, &BTreeMap::from([(InputId(0), value.shape())])))
            .map_err(|issues| CoreError::Unfit(format!("シートを描けません：{issues}")))?;
        let hashes: Vec<[u8; 32]> = used.iter().map(|(p, _)| p.hash).collect();
        let inputs = BTreeMap::from([(
            InputId(0),
            InputValue {
                value,
                source: selection::source(&hashes),
            },
        )]);
        let rendered = match runtime.bulk(|e| e.render(&plan, &inputs, &mon)) {
            Ok(r) => r,
            Err(e) if matches!(e.root(), EngineError::Canceled) => {
                return Ok(Flow::Stop(JobEnd::Paused));
            }
            Err(e) => return Err(from_engine("シートを描けません", e)),
        };
        drop(inputs);
        let out = rendered
            .outputs
            .into_iter()
            .next()
            .map(|o| o.value)
            .ok_or_else(|| CoreError::Invalid("シートの出力がありません".into()))?;
        let Value::Image(img) = &out else {
            return Err(CoreError::Invalid("シートが画像になりません".into()));
        };
        let (width, height) = (img.px.width(), img.px.height());
        let (bytes, _) = match runtime.bulk(|_| codecs.encode_bytes(&out, "png", &json!({}), &mon))
        {
            Ok(b) => b,
            Err(e) if matches!(e.root(), EngineError::Canceled) => {
                return Ok(Flow::Stop(JobEnd::Paused));
            }
            Err(e) => return Err(from_engine("シートを書けません", e)),
        };
        drop(out);
        let hash = *blake3::hash(&bytes).as_bytes();
        let layout = ctx.library().layout();
        std::fs::create_dir_all(layout.staging_dir())?;
        let part = layout
            .staging_dir()
            .join(format!("edit-{}-merge.png.part", ctx.id()));
        {
            let mut f = std::fs::File::create(&part)?;
            f.write_all(&bytes)?;
            unit.sync_file(&f)?;
            self.engine.durability().barrier(&f).map_err(|e| {
                CoreError::Io(std::io::Error::other(format!(
                    "区切りの同期に失敗しました：{e}"
                )))
            })?;
        }
        Ok(Flow::Go(Made {
            part,
            hash,
            size: bytes.len() as u64,
            width,
            height,
            grid: GridSetting {
                cell_w: canvas.w,
                cell_h: canvas.h,
                offset_x: 0,
                offset_y: 0,
                gap_x: sheet.gap_x,
                gap_y: sheet.gap_y,
                count: u32::try_from(count).ok(),
            },
        }))
    }

    /// 予告 → 置く → トランザクション 2（モジュールの説明の 4）。作ったアイテム（同じ中身があればそのアイテム）を返す。中止を
    /// 書いたあと（行が pending でない）なら作らずに None。
    fn commit_merge(
        &self,
        ctx: &JobContext,
        used: &[(Piece, Value)],
        made: &Made,
        names: &(String, String),
    ) -> Result<Option<i64>> {
        let library = ctx.library();
        let db = library.db();
        let layout = library.layout();
        let job = ctx.id();
        let gate = library.ingest_exclusive();
        let hash = made.hash;
        let now = now_ms();
        enum Tx1 {
            Gone,
            Existing(i64),
            Go { ext: String, announced: Vec<i64> },
        }
        let tx1 = db.write(move |tx| {
            let pending: i64 = tx
                .prepare_cached(
                    "SELECT count(*) FROM batch_items WHERE job_id = ?1 AND state = 'pending'",
                )?
                .query_row([job], |r| r.get(0))?;
            if pending == 0 {
                return Ok(Tx1::Gone);
            }
            if let Some(owner) = find_owner(tx, &hash)? {
                return Ok(Tx1::Existing(owner.id));
            }
            let listed = ledger::ext_of(tx, &hash)?;
            let ext = listed.clone().unwrap_or_else(|| "png".to_string());
            let announced = if listed.is_none() {
                vec![ledger::announce(
                    tx,
                    &hash,
                    &ext,
                    ledger::PendingKind::Place,
                    Some(job),
                    now,
                )?]
            } else {
                Vec::new()
            };
            Ok(Tx1::Go { ext, announced })
        })?;
        let (ext, announced) = match tx1 {
            Tx1::Gone => {
                drop(gate);
                let _ = std::fs::remove_file(&made.part);
                return Ok(None);
            }
            Tx1::Existing(owner) => {
                drop(gate);
                let _ = std::fs::remove_file(&made.part);
                let message = format!("同じ中身のアイテム（{owner}）があるので、新しく作りません");
                ctx.log(&message);
                let ords: Vec<i64> = used.iter().map(|(p, _)| p.ord).collect();
                db.write(move |tx| {
                    let mut stmt = tx.prepare_cached(
                        "UPDATE batch_items SET state = 'existing', code = 'Existing', message = ?3, out_item_id = ?4,
                                decoding = 0, finished_at = ?5
                          WHERE job_id = ?1 AND ord = ?2 AND state = 'pending'",
                    )?;
                    for ord in &ords {
                        stmt.execute(params![job, ord, message, owner, now])?;
                    }
                    Ok(())
                })?;
                return Ok(Some(owner));
            }
            Tx1::Go { ext, announced } => (ext, announced),
        };
        #[cfg(test)]
        if self.crash_now(0) == Some(crate::edit::save::Fault::AfterAnnounce) {
            return Err(CoreError::Invalid("テストの強制終了（予告のあと）".into()));
        }
        // 置く
        if let Err(err) = place(
            &Staged {
                path: made.part.clone(),
                hash,
                size: made.size,
            },
            &layout.content_path(&hash, &ext),
        ) {
            undo_placement(db, layout, job);
            return Err(err);
        }
        #[cfg(test)]
        if self.crash_now(0) == Some(crate::edit::save::Fault::AfterPlace) {
            return Err(CoreError::Invalid("テストの強制終了（置いたあと）".into()));
        }
        // コミット：新しいアイテム・来歴・格子・行を 1 つのトランザクションで
        let rows: Vec<(i64, i64, i64, [u8; 32], u64)> = used
            .iter()
            .map(|(p, v)| {
                let frames = match v {
                    Value::Frames(f) => f.frames().len() as u64,
                    _ => 1,
                };
                (p.ord, p.item, p.version, p.hash, frames)
            })
            .collect();
        let (title, stem) = names.clone();
        let (size, width, height, grid) = (made.size, made.width, made.height, made.grid);
        let committed = db.write(move |tx| {
            let original_name = format!("{stem}.{ext}");
            let info = MediaInfo {
                width: Some(i64::from(width)),
                height: Some(i64::from(height)),
                ..MediaInfo::default()
            };
            let id = insert_item(
                tx,
                &NewItem {
                    hash: &hash,
                    ext: &ext,
                    kind: MediaKind::Image,
                    mime: mime_for(&ext),
                    size,
                    title: &title,
                    original_name: &original_name,
                    original_path: "",
                    info: Some(&info),
                    source_site: "",
                    source_url: None,
                    captured_at: None,
                    now,
                },
            )?;
            {
                let mut src = tx.prepare_cached(
                    "INSERT INTO item_sources (item_id, ord, source_item_id, source_version, source_hash, frames, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                let mut row = tx.prepare_cached(
                    "UPDATE batch_items SET state = 'saved', base_hash = ?3, base_version = ?4, out_item_id = ?5,
                            decoding = 0, code = NULL, message = NULL, finished_at = ?6
                      WHERE job_id = ?1 AND ord = ?2 AND state = 'pending'",
                )?;
                for (k, (ord, item, version, source_hash, frames)) in rows.iter().enumerate() {
                    src.execute(params![
                        id,
                        k as i64,
                        item,
                        version,
                        &source_hash[..],
                        i64::try_from(*frames).unwrap_or(i64::MAX),
                        now
                    ])?;
                    let n = row.execute(params![job, ord, &source_hash[..], version, id, now])?;
                    if n != 1 {
                        return Err(CoreError::Conflict(
                            "N → 1 の行がありません（中止されたか、アイテムが消えました）".into(),
                        ));
                    }
                }
            }
            asset::set_settings(
                tx,
                &[id],
                &SettingsPatch {
                    grid: Some(Change::Set(grid)),
                    ..SettingsPatch::default()
                },
                now,
            )?;
            ledger::clear_pending(tx, &announced)?;
            Ok(id)
        });
        let id = match committed {
            Ok(id) => id,
            Err(err) => {
                undo_placement(db, layout, job);
                return Err(err);
            }
        };
        #[cfg(test)]
        if self.crash_now(0) == Some(crate::edit::save::Fault::AfterCommit) {
            return Err(CoreError::Invalid(
                "テストの強制終了（コミットのあと）".into(),
            ));
        }
        drop(gate);
        ctx.log(&format!("スプライトシート（アイテム {id}）を作りました"));
        queue_follow_ups(ctx, &[MediaKind::Image]);
        Ok(Some(id))
    }

    /// 終わり（結果は状態ごとの件数と、作ったアイテム）。
    fn merge_done(&self, ctx: &JobContext, made: Option<i64>) -> Result<JobEnd> {
        let db = ctx.library().db();
        let job = ctx.id();
        // 中止を書いたあとなら中止で終える
        let canceled: bool = db.read(|c| {
            Ok(c.prepare_cached(
                "SELECT EXISTS (SELECT 1 FROM batch_items WHERE job_id = ?1 AND state = 'canceled')",
            )?
            .query_row([job], |r| r.get(0))?)
        })?;
        if canceled {
            return finish_canceled(db, job);
        }
        let mut result = summary(db, job)?;
        result["made"] = json!(made);
        let message = match made {
            Some(id) => format!("スプライトシート（アイテム {id}）を作りました"),
            None => "シートにできるアイテムがありませんでした".to_string(),
        };
        Ok(JobEnd::Done {
            result_json: result.to_string(),
            message: Some(message),
        })
    }
}
