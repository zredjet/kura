//! 一括の事前チェック（M14 段 5。docs/editing.md §14.2「事前チェック」、ADR-195・196）。
//!
//! 当てるシートが、積む前に「当てられるか・出力の形・失うもの・見積もり・飛ばす理由」を出すための確かめ。描かない：アイテム
//! ごとに、一括の 1 件と同じ順に確かめる（飛ばす決まり → 結び付けがなければ継ぎ足して出力の形式と種類 → 保存が読むのと同じ
//! 入力の形 → 結び付けがあれば継ぎ足して結び付け → compile と書き手 → 保存と同じ式のメモリの見積もり）。ジョブにはしない
//! （利用者がその場で待つ確かめ。spawn_blocking で動かし、進みは呼び手がイベントで送り、シートを閉じたら中止する。ADR-195）。
//! 見出しを読む分は対話の枠で待たずに確保し、今は空いていなければそのアイテムは「確かめきれなかった」（Busy）に入れる（当てる
//! 側に数え、当てるときに確かめる）。終わりからの秒で長さが見出しで決まらない音声（MP3・AAC）は、見出しの上限の長さで形式・書き手・
//! 失うもの・見積もりを確かめ、「確かめきれなかった」（NoLength）にも入れる（流して数えるのは当てるとき）。
//!
//! 結果は群にまとめ、アイテムは ID の列だけを返す（10 万件でも詳細を JS に載せない）：当てる・飛ばす／失敗する見込み（理由ごと。
//! 言葉は一括の行と同じ）・確かめきれなかった・出力の形式ごと（代わりの形式か・例 3 件・大きさや長さが変わる件数）・失うもの（種類と
//! 形式ごと）・編集済みに継ぎ足すもの・ディスクの目安・メモリの最大・結び付けで読む値の例 3 件・レシピの旧版の印・ライブラリにない
//! アイテム。ディスクの新しいファイルは、元のファイルの大きさを値の大きさ（画素数・長さ × チャンネル）の比で伸ばした目安（形式による
//! 圧縮の違いは見ない）。上書きは、今のファイル（版 1 以上の頭）が置き換わって 7 日残る分（`replaced_bytes`。元はいつも残すので
//! 数えない）も出す。

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kura_engine::budget::{Class, Lease, MemoryBudget};
use kura_engine::codec::anchor;
use kura_engine::codec::formats::{AlphaCaps, MetaCaps, format_by_id};
use kura_engine::doc::{FormatChoice, Graph, InputId, OutputId};
use kura_engine::recipe::{BindError, Recipe, RecipeNote, bound_json};
use kura_engine::render::estimate::{EstimateSpec, Sink, Source};
use kura_engine::value::PixelFormat;
use kura_engine::{Dim, EngineError, Monitor, Shape};
use rusqlite::OptionalExtension;
use serde_json::json;

use super::merge::{MergeRequest, merge_skip, plan_sheet, read_piece};
use super::{
    BatchMode, BatchRequest, InputRead, ShapeHooks, Skip, bind_skip, dedup, facts_of, head_doc,
    read_input_shape, resolve, skip_rules,
};
use crate::edit::save::{Flow, lru_bytes, streams, verify_bytes};
use crate::edit::{EngineRuntime, EngineServices, versions};
use crate::import::{MediaKind, format_id};
use crate::library::Library;
use crate::{CoreError, Result};

/// 結び付けで読む値の例・出力の形の例の数。
const EXAMPLES: usize = 3;

/// 事前チェックの場（中止と進み）。
pub struct PrecheckCx<'a> {
    /// 真になったら止める（`precheck` は `Canceled` を返す）。
    pub canceled: &'a AtomicBool,
    /// 済んだ件数と全体（1 件ごとに呼ぶ。送る間隔は呼び手が間引く）。
    pub progress: &'a (dyn Fn(u64, u64) + Sync),
}

/// 事前チェックの結果（モジュールの説明）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Precheck {
    /// 確かめた件数（重なりを除いた選んだアイテムのうち、ライブラリにあるもの。一括の行の数と同じ）。
    pub total: u64,
    /// ライブラリにないアイテム（選んだ順。一括は行を入れない）。
    pub missing: Vec<i64>,
    /// 当てる見込みのアイテム（選んだ順。確かめられなかったものを含む）。
    pub apply: Vec<i64>,
    /// 飛ばす・失敗する見込み（理由ごと。最初に出た順）。
    pub skips: Vec<PrecheckGroup>,
    /// 当てる側に数えたが、確かめきれなかったもの（理由ごと）：予算が空いていなくて形を読めなかった（Busy。出力の形・失うもの・
    /// 見積もりもない）、終わりからの秒の長さが当てるときに決まる（NoLength。形式・書き手・失うもの・見積もりは見出しの上限の
    /// 長さで確かめ、`outputs`・`losses` にも入る）。
    pub unchecked: Vec<PrecheckGroup>,
    /// 当てる見込みのものの出力（形式ごと）。
    pub outputs: Vec<OutputGroup>,
    /// 失うもの（種類と形式ごと）。
    pub losses: Vec<LossGroup>,
    /// 編集済み（頭が版 1 以上）で、今の中身に続けて当てるもの。
    pub composed: Vec<i64>,
    pub disk: DiskEstimate,
    /// 1 件を当てる間のメモリの見積もりの最大（バイト。確かめたものだけ）。
    pub memory_peak: u64,
    /// メモリの予算の全体（バイト）。
    pub memory_budget: u64,
    /// 結び付けで読む値の例（結び付けのあるレシピだけ。最初の数件）。
    pub bound: Vec<BoundExample>,
    /// レシピの今の版でない操作の印（docs/editing.md §8.4）。
    pub notes: Vec<RecipeNote>,
    /// N → 1 のシートの見込み（[`precheck_merge`]。1 件ずつの一括は None）。
    pub sheet: Option<SheetInfo>,
}

/// N → 1 のシートの見込み（大きさ・フレームの数は、形を読めたものから。上限しか分からなければ上限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetInfo {
    pub width: u64,
    pub height: u64,
    /// 並べるフレームの数（アニメは全フレーム）。
    pub frames: u64,
    pub columns: u32,
    /// セル（選択のキャンバス）の大きさ。
    pub cell_w: u64,
    pub cell_h: u64,
    /// 大きさ・フレームの数が上限しか分からない（読むまで決まらない）。
    pub approximate: bool,
}

/// 理由ごとの群。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecheckGroup {
    /// `skipped`・`failed`（当てるときの行の状態）。確かめられなかったものは `pending`。
    pub state: String,
    /// 理由の code（一括の行の code と同じ。docs/editing.md §14.2）。
    pub code: String,
    /// 理由の言葉（同じ code でも言葉が違えば別の群）。
    pub message: String,
    pub items: Vec<i64>,
}

/// 出力の形式ごとの群。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputGroup {
    /// 書く形式の ID。
    pub format: String,
    /// 別名で、レシピの形式に書けないので代わりの形式で作る。
    pub fallback: bool,
    pub items: Vec<i64>,
    /// 元の形 → 出力の形の例（最初の数件）。
    pub examples: Vec<ShapeChange>,
    /// 今の大きさ・長さから変わる件数。
    pub resized: u64,
}

/// 元の形 → 出力の形。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeChange {
    pub item_id: i64,
    pub from: ShapeInfo,
    pub to: ShapeInfo,
}

/// 形の要約（大きさ・長さが上限しか分からなければ None）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeInfo {
    Image {
        w: Option<u64>,
        h: Option<u64>,
        /// フレームの列の数（1 枚の画像は None）。
        frames: Option<u64>,
    },
    Audio {
        rate: u32,
        channels: u16,
        frames: Option<u64>,
    },
}

impl ShapeInfo {
    fn of(shape: &Shape) -> Self {
        match *shape {
            Shape::Image { w, h, .. } => Self::Image {
                w: exact(w),
                h: exact(h),
                frames: None,
            },
            Shape::Frames { count, w, h, .. } => Self::Image {
                w: exact(w),
                h: exact(h),
                frames: Some(count.max()),
            },
            Shape::Audio {
                rate,
                channels,
                frames,
            } => Self::Audio {
                rate,
                channels,
                frames: exact(frames),
            },
        }
    }
}

fn exact(d: Dim) -> Option<u64> {
    match d {
        Dim::Exact(n) => Some(n),
        _ => None,
    }
}

/// 失うもの（書く形式の書き手の保存できるもので決める。描かないので「ありうる」もの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Loss {
    /// 非可逆の形式に書く（書くたびに少し劣化する）。
    Lossy,
    /// 透明を持てる出力を、透明を持てない形式に書く（描かないので中身は見ない。透明な画素があれば書き手が断り、そのアイテムは
    /// 失敗する：JPEG は「先に背景を合成してください」）。
    Alpha,
    /// 半透明を持てる出力を、透明か不透明かだけの形式（GIF）に書く。
    SemiAlpha,
    /// 16 ビットの出力を、8 ビットまでの形式に書く。
    HighBits,
    /// 色の数に上限のある形式（GIF は 256 色）に、パレットでない出力を書く（減色する）。
    Colors { max: u32 },
    /// ループ点（試聴のループの設定）のある音声を、ループ点を保存できない形式に書く。
    LoopPoints,
    /// 上書きで大きさ・長さが変わり、素材の設定（格子・ピボット・試聴のループ）が合わなくなりうる（設定は変えない）。
    Settings,
}

/// 失うものの群。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossGroup {
    pub loss: Loss,
    /// 書く形式（素材の設定は形式によらないので空）。
    pub format: String,
    pub items: Vec<i64>,
}

/// ディスクの目安（バイト）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskEstimate {
    /// 新しく書くファイルの目安の合計（モジュールの説明）。
    pub new_bytes: u64,
    /// 上書きで置き換わる今のファイルの合計（7 日残してから片付ける。版 1 以上の頭だけ）。
    pub replaced_bytes: u64,
}

/// 結び付けで読む値の例（`recipe::bound_json` の JSON。読んだ値と書いた値）。
#[derive(Debug, Clone, PartialEq)]
pub struct BoundExample {
    pub item_id: i64,
    pub bound: serde_json::Value,
}

/// 1 件の見込み。
enum Verdict {
    Skip(Skip),
    Unchecked {
        code: &'static str,
        message: String,
        composed: bool,
        estimate: Bytes,
    },
    Apply(Box<Applies>),
}

/// ディスクの目安の 1 件分。
#[derive(Debug, Clone, Copy, Default)]
struct Bytes {
    new: u64,
    replaced: u64,
}

/// 当てる見込みの 1 件。
struct Applies {
    composed: bool,
    format: String,
    fallback: bool,
    change: (ShapeInfo, ShapeInfo),
    resized: bool,
    losses: Vec<(Loss, String)>,
    bytes: Bytes,
    peak: u64,
    bound: Option<serde_json::Value>,
    /// 当てる側に数えるが、確かめきれなかったこと（終わりからの秒の長さ）。
    note: Option<(&'static str, String)>,
}

/// 頼み `req` の事前チェック（モジュールの説明）。レシピに当てられない理由がある・N → 1 のレシピ・元に戻した一括の写しは
/// Invalid、別名の名前の型が空なら Invalid、`cx.canceled` で止めたら Canceled。
pub fn precheck(
    library: &Library,
    engine: &EngineServices,
    req: &BatchRequest,
    cx: &PrecheckCx<'_>,
) -> Result<Precheck> {
    let db = library.db();
    let runtime = engine.runtime()?.clone();
    let resolved = resolve(db, &runtime, req.source)?;
    let save_as = match &req.mode {
        BatchMode::Overwrite => false,
        BatchMode::SaveAs { name } => {
            if name.trim().is_empty() {
                return Err(CoreError::Invalid("別名の名前の型が空です".into()));
            }
            true
        }
    };
    let drafts = !save_as && !req.include_drafts;
    let (items, missing) = split_missing(db, &req.items, "当てる")?;
    let total = items.len() as u64;
    let one = OneCx {
        library,
        engine,
        runtime: &runtime,
        recipe: &resolved.recipe,
        save_as,
        drafts,
        canceled: cx.canceled,
    };
    let verdicts = fan_out(
        &items,
        runtime.bulk_threads(),
        cx,
        &|item| one.check(item),
        &|| Verdict::Skip(internal()),
    )?;
    let mut out = Precheck {
        total,
        missing,
        memory_budget: runtime.budget().total(),
        notes: resolved.notes,
        ..Precheck::default()
    };
    for (item, verdict) in items.iter().zip(verdicts) {
        out.add(*item, verdict);
    }
    Ok(out)
}

/// N → 1 の 1 件の見込み。
enum PieceVerdict {
    Skip(Skip),
    /// 予算が空いていなくて形を読めなかった（並べる側に数える）。
    Busy,
    Go {
        shape: Shape,
        format: &'static str,
        size: u64,
    },
}

/// N → 1（スプライトシート）の事前チェック（docs/editing.md §14.2「N → 1」）：元ごとに一括と同じ飛ばす決まりを見て今の中身の形を
/// 読み（対話の枠。空いていなければ Busy で並べる側）、形を読めたものでシートを計画する（大きさ・セル・メモリ。シートにならない・
/// 予算を超えるなら、どの元も一括と同じ理由で失敗の見込み）。名前が空・どれもライブラリにないなら Invalid、中止なら Canceled。
pub fn precheck_merge(
    library: &Library,
    engine: &EngineServices,
    req: &MergeRequest,
    cx: &PrecheckCx<'_>,
) -> Result<Precheck> {
    let db = library.db();
    let runtime = engine.runtime()?.clone();
    if req.name.trim().is_empty() {
        return Err(CoreError::Invalid("シートの名前が空です".into()));
    }
    req.sheet
        .validate()
        .map_err(|e| crate::edit::from_engine("シートの引数が正しくありません", e))?;
    let (items, missing) = split_missing(db, &req.items, "並べる")?;
    let check = |item: i64| -> Result<PieceVerdict> {
        let head = match merge_skip(engine, db, item) {
            Ok(head) => head,
            Err(skip) => return Ok(PieceVerdict::Skip(skip)),
        };
        let hooks = PrecheckShape {
            mon: CancelMon(cx.canceled),
            budget: runtime.budget(),
            denied: std::cell::Cell::new(false),
        };
        match read_piece(engine, library, 0, item, &head, &hooks) {
            Ok(Flow::Go(p)) => Ok(PieceVerdict::Go {
                shape: p.read.shape,
                format: p.read.format,
                size: p.size,
            }),
            Ok(Flow::Stop(_)) => Err(CoreError::Canceled),
            Err(_) if hooks.denied.get() => Ok(PieceVerdict::Busy),
            Err(e @ CoreError::Unfit(_)) => {
                Ok(PieceVerdict::Skip(Skip::skipped("Kind", e.to_string())))
            }
            Err(e) => Ok(PieceVerdict::Skip(Skip::failed(&e))),
        }
    };
    let verdicts = fan_out(&items, runtime.bulk_threads(), cx, &check, &|| {
        PieceVerdict::Skip(internal())
    })?;
    let mut out = Precheck {
        total: items.len() as u64,
        missing,
        memory_budget: runtime.budget().total(),
        ..Precheck::default()
    };
    let mut go: Vec<(i64, Shape, &'static str, u64)> = Vec::new();
    for (item, v) in items.iter().zip(verdicts) {
        match v {
            PieceVerdict::Skip(skip) => {
                push_group(&mut out.skips, skip.state, &skip.code, skip.message, *item)
            }
            PieceVerdict::Busy => {
                out.apply.push(*item);
                push_group(
                    &mut out.unchecked,
                    "pending",
                    "Busy",
                    "メモリが空いていないので、作るときに確かめます".into(),
                    *item,
                );
            }
            PieceVerdict::Go {
                shape,
                format,
                size,
            } => {
                out.apply.push(*item);
                go.push((*item, shape, format, size));
            }
        }
    }
    if go.is_empty() {
        return Ok(out);
    }
    let pieces: Vec<(Shape, &'static str, u64)> =
        go.iter().map(|(_, s, f, z)| (*s, *f, *z)).collect();
    match plan_sheet(engine, &pieces, &req.sheet) {
        Ok(sp) => {
            let (first, first_shape) = (go[0].0, go[0].1);
            out.outputs.push(OutputGroup {
                format: "png".into(),
                fallback: false,
                items: out.apply.clone(),
                examples: vec![ShapeChange {
                    item_id: first,
                    from: ShapeInfo::of(&first_shape),
                    to: ShapeInfo::of(&sp.out),
                }],
                resized: 0,
            });
            out.memory_peak = sp.need;
            let sizes = go.iter().fold(0u64, |a, (_, _, _, z)| a.saturating_add(*z));
            let values = go
                .iter()
                .fold(0u64, |a, (_, s, _, _)| a.saturating_add(s.value_bytes()));
            out.disk.new_bytes = scaled(sizes, values, sp.out.value_bytes());
            let (cell_w, cell_h, frames, exact_sel) = match sp.selection {
                Shape::Frames { count, w, h, .. } => (
                    w.max(),
                    h.max(),
                    count.max(),
                    matches!((count, w, h), (Dim::Exact(_), Dim::Exact(_), Dim::Exact(_))),
                ),
                _ => (0, 0, 0, false),
            };
            let (width, height, exact_out) = match sp.out {
                Shape::Image { w, h, .. } => (
                    w.max(),
                    h.max(),
                    matches!((w, h), (Dim::Exact(_), Dim::Exact(_))),
                ),
                _ => (0, 0, false),
            };
            out.sheet = Some(SheetInfo {
                width,
                height,
                frames,
                columns: sp.columns,
                cell_w,
                cell_h,
                approximate: !(exact_sel && exact_out) || !out.unchecked.is_empty(),
            });
        }
        // シートにならない・予算を超える：一括と同じく、読めたどの元も同じ理由で失敗
        Err(e) => {
            for (item, ..) in &go {
                out.apply.retain(|a| a != item);
                push_group(&mut out.skips, "failed", e.code(), e.to_string(), *item);
            }
        }
    }
    Ok(out)
}

/// 選んだアイテムを、ライブラリにあるもの（重なりを除いて選んだ順）とないものに分ける（一括も行を入れない）。どれもなければ
/// 一括と同じく Invalid（`verb` は「当てる」「並べる」）。
fn split_missing(db: &crate::db::Db, req: &[i64], verb: &str) -> Result<(Vec<i64>, Vec<i64>)> {
    let mut items = Vec::new();
    let mut missing = Vec::new();
    db.read(|c| {
        let mut exists = c.prepare_cached("SELECT 1 FROM items WHERE id = ?1")?;
        for id in dedup(req) {
            if exists.query_row([id], |_| Ok(())).optional()?.is_some() {
                items.push(id);
            } else {
                missing.push(id);
            }
        }
        Ok(())
    })?;
    if items.is_empty() {
        return Err(CoreError::Invalid(if missing.is_empty() {
            format!("{verb}アイテムがありません")
        } else {
            format!("{verb}アイテムがありません（どれもライブラリにありません）")
        }));
    }
    Ok((items, missing))
}

/// 1 件の panic の見込み。
fn internal() -> Skip {
    Skip {
        state: "failed",
        code: "Internal".into(),
        message: "確かめる途中で内部の誤りが起きました".into(),
    }
}

/// `items` を `k` 本の働き手で 1 件ずつ `check` し、選んだ順の結果を返す。進みは 1 件ごと、`cx.canceled` で止め（Canceled）、
/// `check` の誤りは全体の誤り。1 件の panic は `on_panic` の結果にする（一括の働き手と同じ。全体を落とさない）。
fn fan_out<T: Send>(
    items: &[i64],
    k: usize,
    cx: &PrecheckCx<'_>,
    check: &(dyn Fn(i64) -> Result<T> + Sync),
    on_panic: &(dyn Fn() -> T + Sync),
) -> Result<Vec<T>> {
    let total = items.len() as u64;
    let k = k.max(1).min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<T>>> = Mutex::new((0..items.len()).map(|_| None).collect());
    let failure: Mutex<Option<CoreError>> = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..k {
            s.spawn(|| {
                loop {
                    if cx.canceled.load(Ordering::Acquire) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::AcqRel);
                    let Some(&item) = items.get(i) else { return };
                    let checked =
                        catch_unwind(AssertUnwindSafe(|| check(item))).unwrap_or_else(|_| {
                            tracing::error!(item, "事前チェックの 1 件が panic しました");
                            Ok(on_panic())
                        });
                    match checked {
                        Ok(v) => {
                            if let Ok(mut all) = results.lock()
                                && let Some(slot) = all.get_mut(i)
                            {
                                *slot = Some(v);
                            }
                        }
                        Err(e) => {
                            if let Ok(mut f) = failure.lock() {
                                f.get_or_insert(e);
                            }
                            cx.canceled.store(true, Ordering::Release);
                            return;
                        }
                    }
                    let n = done.fetch_add(1, Ordering::AcqRel) + 1;
                    (cx.progress)(n as u64, total);
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().ok().flatten() {
        return Err(e);
    }
    if cx.canceled.load(Ordering::Acquire) {
        return Err(CoreError::Canceled);
    }
    results
        .into_inner()
        .map_err(|_| CoreError::Invalid("事前チェックの結果の錠が壊れました".into()))?
        .into_iter()
        .map(|v| v.ok_or(CoreError::Canceled))
        .collect()
}

impl Precheck {
    fn add(&mut self, item: i64, verdict: Verdict) {
        match verdict {
            Verdict::Skip(skip) => {
                push_group(&mut self.skips, skip.state, &skip.code, skip.message, item)
            }
            Verdict::Unchecked {
                code,
                message,
                composed,
                estimate,
            } => {
                self.apply.push(item);
                if composed {
                    self.composed.push(item);
                }
                self.disk.new_bytes = self.disk.new_bytes.saturating_add(estimate.new);
                self.disk.replaced_bytes =
                    self.disk.replaced_bytes.saturating_add(estimate.replaced);
                push_group(&mut self.unchecked, "pending", code, message, item);
            }
            Verdict::Apply(a) => {
                self.apply.push(item);
                if a.composed {
                    self.composed.push(item);
                }
                if let Some((code, message)) = &a.note {
                    push_group(&mut self.unchecked, "pending", code, message.clone(), item);
                }
                self.disk.new_bytes = self.disk.new_bytes.saturating_add(a.bytes.new);
                self.disk.replaced_bytes =
                    self.disk.replaced_bytes.saturating_add(a.bytes.replaced);
                self.memory_peak = self.memory_peak.max(a.peak);
                if let Some(bound) = a.bound
                    && self.bound.len() < EXAMPLES
                {
                    self.bound.push(BoundExample {
                        item_id: item,
                        bound,
                    });
                }
                let group = match self
                    .outputs
                    .iter_mut()
                    .find(|g| g.format == a.format && g.fallback == a.fallback)
                {
                    Some(g) => g,
                    None => {
                        self.outputs.push(OutputGroup {
                            format: a.format.clone(),
                            fallback: a.fallback,
                            items: Vec::new(),
                            examples: Vec::new(),
                            resized: 0,
                        });
                        let last = self.outputs.len() - 1;
                        &mut self.outputs[last]
                    }
                };
                group.items.push(item);
                if group.examples.len() < EXAMPLES {
                    group.examples.push(ShapeChange {
                        item_id: item,
                        from: a.change.0,
                        to: a.change.1,
                    });
                }
                if a.resized {
                    group.resized += 1;
                }
                for (loss, format) in a.losses {
                    match self
                        .losses
                        .iter_mut()
                        .find(|g| g.loss == loss && g.format == format)
                    {
                        Some(g) => g.items.push(item),
                        None => self.losses.push(LossGroup {
                            loss,
                            format,
                            items: vec![item],
                        }),
                    }
                }
            }
        }
    }
}

fn push_group(
    groups: &mut Vec<PrecheckGroup>,
    state: &str,
    code: &str,
    message: String,
    item: i64,
) {
    match groups
        .iter_mut()
        .find(|g| g.state == state && g.code == code && g.message == message)
    {
        Some(g) => g.items.push(item),
        None => groups.push(PrecheckGroup {
            state: state.to_string(),
            code: code.to_string(),
            message,
            items: vec![item],
        }),
    }
}

/// 1 件を確かめる場（働き手が共有する）。
struct OneCx<'a> {
    library: &'a Library,
    engine: &'a EngineServices,
    runtime: &'a EngineRuntime,
    recipe: &'a Recipe,
    save_as: bool,
    drafts: bool,
    canceled: &'a AtomicBool,
}

/// 事前チェックの見出しの読み方：止める指示はシートの中止、予算は対話の枠（待たない）、印は置かない。
struct PrecheckShape<'a> {
    mon: CancelMon<'a>,
    budget: &'a Arc<MemoryBudget>,
    /// 予算が空いていなくて読めなかった。
    denied: std::cell::Cell<bool>,
}

struct CancelMon<'a>(&'a AtomicBool);

impl Monitor for CancelMon<'_> {
    fn canceled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl ShapeHooks for PrecheckShape<'_> {
    fn monitor(&self) -> &dyn Monitor {
        &self.mon
    }

    fn acquire(&self, bytes: u64) -> std::result::Result<Lease, EngineError> {
        if self.mon.canceled() {
            return Err(EngineError::Canceled);
        }
        // 今は空いていないだけなら「確かめられなかった」、予算の全体を超えるなら当てるときも失敗する（TooLarge）
        match self.budget.try_acquire(bytes, Class::Interactive)? {
            Some(lease) => Ok(lease),
            None => {
                self.denied.set(true);
                Err(EngineError::Limit("メモリが空いていません".into()))
            }
        }
    }

    fn mark(&self, _decoding: bool) -> Result<()> {
        Ok(())
    }

    fn log(&self, line: &str) {
        tracing::debug!("{line}");
    }
}

/// 入力の形の読みの結果。
enum Read {
    Go(Box<InputRead>),
    /// 予算が今は空いていない（確かめられなかった）。
    Denied,
    /// 読めない（当てるときも同じ誤りで失敗する）。
    Failed(CoreError),
}

impl OneCx<'_> {
    /// 1 件の見込み（誤りは事前チェックそのものの失敗。アイテムの誤りは Verdict に入れる）。一括の 1 件と同じ順に確かめる：
    /// 結び付けがなければ、継ぎ足してから形を読む前に出力の形式と種類を照らし（保存の段 0）、あれば先に形を読んで結び付ける
    /// （段 2）。そのあと compile と書き手（保存の段 1・2）。
    fn check(&self, item: i64) -> Result<Verdict> {
        let db = self.library.db();
        let head = match skip_rules(self.engine, db, item, self.drafts) {
            Ok(head) => head,
            Err(skip) => return Ok(Verdict::Skip(skip)),
        };
        let composed = head.version > 0;
        let (cur_size, cur_w, cur_h, cur_ms): (i64, Option<i64>, Option<i64>, Option<i64>) = db
            .read(|c| {
                Ok(c.prepare_cached(
                    "SELECT size_bytes, width, height, duration_ms FROM items WHERE id = ?1",
                )?
                .query_row([item], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .optional()?
                .unwrap_or((0, None, None, None)))
            })?;
        let Some(original) = db.read(|c| versions::original(c, item))? else {
            return Ok(Verdict::Skip(Skip::skipped(
                "Gone",
                "アイテムがありません".into(),
            )));
        };
        let replaced = if !self.save_as && head.version > 0 {
            u64::try_from(cur_size).unwrap_or(0)
        } else {
            0
        };
        let rough = Bytes {
            new: u64::try_from(original.size_bytes).unwrap_or(0),
            replaced,
        };
        let unchecked = |code: &'static str, message: &str| {
            Ok(Verdict::Unchecked {
                code,
                message: message.to_string(),
                composed,
                estimate: rough,
            })
        };
        let Some(source_format) = format_id(&original.ext) else {
            return Ok(Verdict::Skip(Skip::failed(&CoreError::Unsupported(
                format!("元の形式（{}）を読めません", original.ext),
            ))));
        };
        let head_doc = match head_doc(db, self.runtime, item, head.version) {
            Ok(g) => g,
            Err(e) => return Ok(Verdict::Skip(Skip::failed(&e))),
        };
        let facts = db.read(|c| facts_of(c, item))?;
        // ファイルのループ点（解析の事実。解析していなければ分からないので言わない）
        let file_loop = db
            .read(|c| crate::asset::get(c, item))
            .map(|a| a.facts.is_some_and(|f| f.audio.has_loop == Some(true)))
            .unwrap_or(false);
        let busy = "メモリが空いていないので、当てるときに確かめます";
        let mut note: Option<(&'static str, String)> = None;
        let (graph, bound, read, compile_shape) = if self.recipe.bindings.is_empty() {
            let instance = match self
                .runtime
                .bulk(|e| e.instantiate(&head_doc, self.recipe, &BTreeMap::new(), &facts))
            {
                Ok(instance) => instance,
                Err(e) => return Ok(Verdict::Skip(bind_skip(e))),
            };
            // 形を読む前に、出力の形式と種類（書き手のない形式は、上書きなら読まずに飛ばす。別名は代わりの形式で読む）
            match self.format_kind(&instance.graph, source_format, head.kind) {
                Ok(()) => {}
                Err(Attempt::Skip(skip)) => return Ok(Verdict::Skip(skip)),
                Err(Attempt::Unwritable(why)) if !self.save_as => {
                    return Ok(Verdict::Skip(Skip::skipped("FormatUnavailable", why)));
                }
                Err(Attempt::Unwritable(_)) => {}
            }
            let read = match self.read(item, &head_doc)? {
                Read::Go(read) => read,
                Read::Denied => return unchecked("Busy", busy),
                Read::Failed(e) => return Ok(Verdict::Skip(Skip::failed(&e))),
            };
            let shape = read.shape;
            (instance.graph, None, read, shape)
        } else {
            let read = match self.read(item, &head_doc)? {
                Read::Go(read) => read,
                Read::Denied => return unchecked("Busy", busy),
                Read::Failed(e) => return Ok(Verdict::Skip(Skip::failed(&e))),
            };
            let shapes = BTreeMap::from([(InputId(0), read.shape)]);
            let (instance, shape) = match self
                .runtime
                .bulk(|e| e.instantiate(&head_doc, self.recipe, &shapes, &facts))
            {
                Ok(instance) => (instance, read.shape),
                Err(BindError::NoLength { .. }) if read.stage.is_some() => {
                    // 当てるときは流して数えた長さで結び付ける。ここでは見出しの上限の長さで、形式・書き手・失うもの・
                    // 見積もりを確かめる（結び付けた値と長さは当てるときに決まる）
                    note = Some((
                        "NoLength",
                        "終わりからの秒は、当てるときに元の長さを数えて決めます".into(),
                    ));
                    let upper = upper_bound(read.shape);
                    let shapes = BTreeMap::from([(InputId(0), upper)]);
                    match self
                        .runtime
                        .bulk(|e| e.instantiate(&head_doc, self.recipe, &shapes, &facts))
                    {
                        Ok(instance) => (instance, upper),
                        Err(e) => return Ok(Verdict::Skip(bind_skip(e))),
                    }
                }
                Err(e) => return Ok(Verdict::Skip(bind_skip(e))),
            };
            let bound = bound_json(&instance.bound)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            (instance.graph, bound, read, shape)
        };
        let mut graph = graph;
        let shapes = BTreeMap::from([(InputId(0), compile_shape)]);
        // 出力の形と形式（保存の段 0〜2 と同じ確かめ。別名は代わりの形式も。一括と同じ順）
        let mut fallback = false;
        let (plan, out_shape, out_format, enc_opts) =
            match self.output(&graph, &shapes, source_format, head.kind) {
                Ok(out) => out,
                Err(Attempt::Skip(skip)) => return Ok(Verdict::Skip(skip)),
                Err(Attempt::Unwritable(why)) if !self.save_as => {
                    return Ok(Verdict::Skip(Skip::skipped("FormatUnavailable", why)));
                }
                Err(Attempt::Unwritable(why)) => {
                    let fallbacks: &[&str] = match head.kind {
                        MediaKind::Audio => &["ogg", "wav"],
                        _ => &["png"],
                    };
                    let mut last = Err(Attempt::Unwritable(why));
                    for format in fallbacks {
                        if let Some(out) = graph.outputs.first_mut() {
                            out.format = FormatChoice::To {
                                format: (*format).to_string(),
                                options: json!({}),
                            };
                            out.quantize = None;
                        }
                        last = self.output(&graph, &shapes, source_format, head.kind);
                        if !matches!(last, Err(Attempt::Unwritable(_))) {
                            break;
                        }
                    }
                    fallback = true;
                    match last {
                        Ok(out) => out,
                        Err(Attempt::Skip(skip)) => return Ok(Verdict::Skip(skip)),
                        Err(Attempt::Unwritable(why)) => {
                            return Ok(Verdict::Skip(Skip::skipped("FormatUnavailable", why)));
                        }
                    }
                }
            };
        let codecs = self.engine.codecs();
        // 失うもの
        let caps = codecs
            .encoder(&out_format, &out_shape)
            .map(|e| e.caps(&out_format, &enc_opts));
        let resized = match (out_shape, cur_w, cur_h, cur_ms) {
            (Shape::Image { w, h, .. } | Shape::Frames { w, h, .. }, Some(cw), Some(ch), _) => {
                exact(w) != u64::try_from(cw).ok() || exact(h) != u64::try_from(ch).ok()
            }
            (
                Shape::Audio {
                    rate,
                    frames: Dim::Exact(n),
                    ..
                },
                _,
                _,
                Some(ms),
            ) if rate > 0 && note.is_none() => {
                let out_ms = u128::from(n) * 1000 / u128::from(rate);
                out_ms.abs_diff(u128::try_from(ms).unwrap_or(0)) > 1
            }
            // 長さが上限しか分からない音声（MP3・AAC）は、元の見出しの長さと比べる（同じ上限なら変わらない）
            (Shape::Audio { frames, .. }, ..) => match read.shape {
                Shape::Audio { frames: from, .. } => frames != from,
                _ => true,
            },
            _ => false,
        };
        let has_settings = facts.grid.is_some()
            || facts.pivot.is_some()
            || (matches!(out_shape, Shape::Audio { .. }) && facts.loop_range.is_some());
        let mut losses: Vec<(Loss, String)> = losses_of(caps.as_ref(), &out_shape, file_loop)
            .into_iter()
            .map(|l| (l, out_format.clone()))
            .collect();
        if !self.save_as && resized && has_settings {
            losses.push((Loss::Settings, String::new()));
        }
        // 見積もり（メモリ。保存と同じ式で、予算の全体を超えるなら当てるときに失敗する）
        let tasks = self.runtime.bulk_threads();
        let anchored = read.anchor.is_some();
        let in_shape = compile_shape;
        let peak = if streams(&codecs, head.kind, &out_format) {
            // 流して書く：源は LRU だけ持つ。再現しない読みでアンカーがなければ、昇格の照らしの分
            let reproducible = read.stage.as_ref().is_none_or(|stage| {
                codecs
                    .decoders_for(source_format)
                    .iter()
                    .find(|d| d.id() == stage.id)
                    .is_none_or(|d| d.reproducible())
            });
            let promote = if !anchored && !reproducible {
                verify_bytes(&in_shape, tasks as u64, 32 << 20)
            } else {
                0
            };
            let spec = EstimateSpec::new(tasks as u64)
                .stream(OutputId(0))
                .digests(true)
                .input(
                    InputId(0),
                    Source::Lazy {
                        resident: lru_bytes(&in_shape, tasks),
                    },
                )
                .output(
                    OutputId(0),
                    Sink::Encode {
                        codecs: &codecs,
                        format: &out_format,
                        opts: &enc_opts,
                    },
                );
            self.runtime
                .bulk(|e| e.estimate(&plan, &spec, None))
                .map_or(0, |est| est.peak.max(promote))
        } else {
            let (source, path) = match &read.anchor {
                Some(path) => (
                    Source::Decoder {
                        decoder: &anchor::AnchorDecoder,
                        format: anchor::FORMAT,
                    },
                    path.as_path(),
                ),
                None => (
                    Source::Decode {
                        codecs: &codecs,
                        format: source_format,
                    },
                    read.path.as_path(),
                ),
            };
            let input_len = std::fs::metadata(path).map_or(0, |m| m.len());
            let spec = EstimateSpec::new(tasks as u64)
                .digests(true)
                .input(InputId(0), source)
                .input_len(InputId(0), input_len)
                .output(
                    OutputId(0),
                    Sink::Encode {
                        codecs: &codecs,
                        format: &out_format,
                        opts: &enc_opts,
                    },
                );
            // アンカーを作る読み（再現しないデコーダの形式で、アンカーがない）：読む持ち越し ＋ 値 ＋ 書いて読み直す分
            let may_anchor = !anchored
                && codecs
                    .decoders_for(source_format)
                    .iter()
                    .any(|d| !d.reproducible());
            self.runtime
                .bulk(|e| e.estimate(&plan, &spec, None))
                .map_or(0, |est| {
                    let anchor_need = if may_anchor {
                        est.carried_decode
                            .saturating_add(in_shape.value_bytes())
                            .saturating_add(anchor::work_bytes(&in_shape))
                    } else {
                        0
                    };
                    est.peak.max(anchor_need)
                })
        };
        if peak > self.runtime.budget().total() {
            return Ok(Verdict::Skip(Skip::failed(&CoreError::TooLarge(
                "メモリの予算を超える見込みなので、当てると失敗します".into(),
            ))));
        }
        // ディスクの目安：元のファイルの大きさを値の大きさの比で伸ばす
        let new = scaled(
            u64::try_from(original.size_bytes).unwrap_or(0),
            read.shape.value_bytes(),
            out_shape.value_bytes(),
        );
        Ok(Verdict::Apply(Box::new(Applies {
            composed,
            format: out_format,
            fallback,
            change: (ShapeInfo::of(&read.shape), ShapeInfo::of(&out_shape)),
            resized,
            losses,
            bytes: Bytes { new, replaced },
            peak,
            bound,
            note,
        })))
    }

    /// 入力 0 の形を読む（一括と同じ読み方。予算は対話の枠で待たず、空いていなければ Denied。全体を超えるなら Failed）。
    fn read(&self, item: i64, head_doc: &Graph) -> Result<Read> {
        let hooks = PrecheckShape {
            mon: CancelMon(self.canceled),
            budget: self.runtime.budget(),
            denied: std::cell::Cell::new(false),
        };
        match read_input_shape(self.engine, self.library, item, head_doc, &hooks) {
            Ok(Flow::Go(read)) => Ok(Read::Go(Box::new(read))),
            Ok(Flow::Stop(_)) => Err(CoreError::Canceled),
            Err(_) if hooks.denied.get() => Ok(Read::Denied),
            Err(e) => Ok(Read::Failed(e)),
        }
    }

    /// 出力の形式と種類（保存の段 0：知らない形式は Unwritable、種類を変える形式は NotApplicable）。
    fn format_kind(
        &self,
        graph: &Graph,
        source_format: &str,
        kind: MediaKind,
    ) -> std::result::Result<(), Attempt> {
        let out_format = match graph.outputs.first().map(|o| &o.format) {
            Some(FormatChoice::To { format, .. }) => format.as_str(),
            Some(FormatChoice::Keep) | None => source_format,
        };
        let desc = format_by_id(out_format)
            .ok_or_else(|| Attempt::Unwritable(format!("出力の形式 {out_format} を知りません")))?;
        if MediaKind::from(desc.item_kind) != kind {
            return Err(Attempt::Skip(Skip::skipped(
                "NotApplicable",
                CoreError::Unfit(format!(
                    "保存でアイテムの種類は変えられません（{} → {out_format}）",
                    kind.as_str()
                ))
                .to_string(),
            )));
        }
        Ok(())
    }

    /// 文書 `graph` の出力の形式と種類を照らし（[`Self::format_kind`]）、入力の形で compile して、書き手を確かめる（保存の
    /// 段 0〜2 と同じ順：描けない手は NotApplicable、書き手がなければ Unwritable）。
    fn output(
        &self,
        graph: &Graph,
        shapes: &BTreeMap<InputId, Shape>,
        source_format: &str,
        kind: MediaKind,
    ) -> std::result::Result<Output, Attempt> {
        self.format_kind(graph, source_format, kind)?;
        let plan = self
            .runtime
            .bulk(|e| e.compile(graph, shapes))
            .map_err(|issues| {
                Attempt::Skip(Skip::skipped(
                    "NotApplicable",
                    CoreError::Unfit(format!("文書を描けません：{issues}")).to_string(),
                ))
            })?;
        let out_shape = plan
            .output_shapes()
            .first()
            .map(|(_, s)| *s)
            .ok_or_else(|| {
                Attempt::Skip(Skip::failed(&CoreError::Invalid(
                    "文書に出力がありません".into(),
                )))
            })?;
        let (out_format, enc_opts) = match graph.outputs.first().map(|o| &o.format) {
            Some(FormatChoice::To { format, options }) => (
                format.clone(),
                if options.is_null() {
                    json!({})
                } else {
                    options.clone()
                },
            ),
            Some(FormatChoice::Keep) | None => (source_format.to_string(), json!({})),
        };
        if self
            .engine
            .codecs()
            .encoder(&out_format, &out_shape)
            .is_none()
        {
            return Err(Attempt::Unwritable(format!(
                "この結果は {out_format} に書けません（保存する形式を選び直してください。音声の既定は Ogg Vorbis）"
            )));
        }
        Ok((plan, out_shape, out_format, enc_opts))
    }
}

/// 長さが上限しか分からない音声の形を、上限の長さで決まった形にする（事前チェックで形式・書き手を確かめるため）。
fn upper_bound(shape: Shape) -> Shape {
    match shape {
        Shape::Audio {
            rate,
            channels,
            frames,
        } => Shape::Audio {
            rate,
            channels,
            frames: Dim::Exact(frames.max()),
        },
        other => other,
    }
}

/// 出力を確かめられなかった理由。
enum Attempt {
    /// 書き手がない（別名なら代わりの形式を試す）。
    Unwritable(String),
    /// このアイテムに当てられない・失敗する。
    Skip(Skip),
}

/// 確かめた出力（計画・形・形式・設定）。
type Output = (kura_engine::render::Plan, Shape, String, serde_json::Value);

/// `size` を `to / from` の比で伸ばす（`from` が 0 なら `size`）。
fn scaled(size: u64, from: u64, to: u64) -> u64 {
    if from == 0 {
        return size;
    }
    let v = u128::from(size) * u128::from(to) / u128::from(from);
    u64::try_from(v).unwrap_or(u64::MAX)
}

/// 書き手の保存できるもの `caps` と出力の形で、失うもの（並べる順）。書き手がなければ何も言わない。
fn losses_of(caps: Option<&MetaCaps>, out: &Shape, has_loop: bool) -> Vec<Loss> {
    let Some(caps) = caps else {
        return Vec::new();
    };
    let mut out_losses = Vec::new();
    if !caps.lossless {
        out_losses.push(Loss::Lossy);
    }
    match out {
        Shape::Image { fmt, .. } | Shape::Frames { fmt, .. } => {
            if fmt.has_alpha() {
                match caps.alpha {
                    AlphaCaps::None => out_losses.push(Loss::Alpha),
                    AlphaCaps::Binary => out_losses.push(Loss::SemiAlpha),
                    AlphaCaps::Full => {}
                }
            } else if *fmt == PixelFormat::Indexed8 && caps.alpha == AlphaCaps::None {
                // パレットは色が透明を持てる（Indexed8 の `has_alpha` は偽）。JPEG の書き手は透明な色があれば断る。GIF（2 値）は
                // パレットのまま書くことが多く、半透明の色があるかは描かないと分からないので言わない
                out_losses.push(Loss::Alpha);
            }
            if matches!(
                fmt,
                PixelFormat::Gray16
                    | PixelFormat::GrayA16
                    | PixelFormat::Rgb16
                    | PixelFormat::Rgba16
            ) && !caps.high_bit_depth
            {
                out_losses.push(Loss::HighBits);
            }
            if let Some(max) = caps.max_colors
                && *fmt != PixelFormat::Indexed8
            {
                out_losses.push(Loss::Colors { max });
            }
        }
        Shape::Audio { .. } => {
            if has_loop && !caps.loop_points {
                out_losses.push(Loss::LoopPoints);
            }
        }
    }
    out_losses
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_engine::codec::formats::NONE;

    #[test]
    fn scaled_follows_the_value_ratio() {
        assert_eq!(scaled(1_000, 100, 400), 4_000);
        assert_eq!(scaled(1_000, 0, 400), 1_000);
        assert_eq!(scaled(u64::MAX, 1, 2), u64::MAX);
    }

    #[test]
    fn losses_follow_the_writer_caps() {
        let rgba = Shape::Image {
            w: Dim::Exact(4),
            h: Dim::Exact(4),
            fmt: PixelFormat::Rgba8,
        };
        let jpeg = MetaCaps {
            alpha: AlphaCaps::None,
            ..NONE
        };
        assert_eq!(
            losses_of(Some(&jpeg), &rgba, false),
            vec![Loss::Lossy, Loss::Alpha]
        );
        let gif = MetaCaps {
            lossless: true,
            alpha: AlphaCaps::Binary,
            max_colors: Some(256),
            ..NONE
        };
        assert_eq!(
            losses_of(Some(&gif), &rgba, false),
            vec![Loss::SemiAlpha, Loss::Colors { max: 256 }]
        );
        let indexed = Shape::Image {
            w: Dim::Exact(4),
            h: Dim::Exact(4),
            fmt: PixelFormat::Indexed8,
        };
        assert!(losses_of(Some(&gif), &indexed, false).is_empty());
        assert_eq!(
            losses_of(Some(&jpeg), &indexed, false),
            vec![Loss::Lossy, Loss::Alpha]
        );
        let png16 = MetaCaps {
            lossless: true,
            alpha: AlphaCaps::Full,
            ..NONE
        };
        let rgb16 = Shape::Image {
            w: Dim::Exact(4),
            h: Dim::Exact(4),
            fmt: PixelFormat::Rgb16,
        };
        assert_eq!(losses_of(Some(&png16), &rgb16, false), vec![Loss::HighBits]);
        let audio = Shape::Audio {
            rate: 44_100,
            channels: 2,
            frames: Dim::Exact(10),
        };
        let mp3 = NONE;
        assert_eq!(
            losses_of(Some(&mp3), &audio, true),
            vec![Loss::Lossy, Loss::LoopPoints]
        );
        assert_eq!(losses_of(Some(&mp3), &audio, false), vec![Loss::Lossy]);
        assert!(losses_of(None, &audio, true).is_empty());
    }
}
