//! 一括の事前チェック（M14 段 5。docs/editing.md §14.2「事前チェック」、ADR-195）。
//!
//! 当てるシートが、積む前に「当てられるか・出力の形・失うもの・見積もり・飛ばす理由」を出すための確かめ。描かない：アイテム
//! ごとに、一括のジョブと同じ飛ばす決まりを見て、頭の文書にレシピを継ぎ足して結び付け（`Engine::instantiate`）、保存が読むのと
//! 同じ入力の形（アンカーの見出しか、元を段の順に）で compile し、出力の形式の書き手と保存できるもの（`MetaCaps`）を照らし、
//! メモリを見積もる（`Engine::estimate`）。ジョブにはしない（利用者がその場で待つ確かめ。spawn_blocking で動かし、進みは
//! 呼び手がイベントで送り、シートを閉じたら中止する。ADR-195）。見出しを読む分は対話の枠で確保し（待たない）、空いていなければ
//! そのアイテムは「確かめられなかった」に入れる（当てる側に数え、当てるときに確かめる）。終わりからの秒で長さが見出しで決まらない
//! 音声（MP3・AAC）も、流して数えるのは当てるときなので「確かめられなかった」に入れる。
//!
//! 結果は群にまとめ、アイテムは ID の列だけを返す（10 万件でも詳細を JS に載せない）：当てる・飛ばす／失敗する見込み（理由ごと）・
//! 確かめられなかった・出力の形式ごと（代わりの形式か・例 3 件・大きさや長さが変わる件数）・失うもの（種類と形式ごと）・編集済みに
//! 継ぎ足すもの・ディスクの目安・メモリの最大・結び付けで読む値の例 3 件・レシピの旧版の印。ディスクの新しいファイルは、元の
//! ファイルの大きさを値の大きさ（画素数・長さ × チャンネル）の比で伸ばした目安（形式による圧縮の違いは見ない）。上書きは、今の
//! ファイル（版 1 以上の頭）が置き換わって 7 日残る分（`replaced_bytes`。元はいつも残すので数えない）も出す。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kura_engine::budget::{Class, Lease, MemoryBudget};
use kura_engine::codec::formats::{AlphaCaps, MetaCaps, format_by_id};
use kura_engine::doc::{FormatChoice, Graph, InputId, OutputId};
use kura_engine::recipe::{BindError, Recipe, RecipeNote, bound_json};
use kura_engine::render::estimate::{EstimateSpec, Sink, Source};
use kura_engine::value::PixelFormat;
use kura_engine::{Dim, EngineError, Monitor, Shape};
use rusqlite::OptionalExtension;
use serde_json::json;

use super::{
    BatchMode, BatchRequest, InputRead, ShapeHooks, Skip, bind_skip, dedup, facts_of, head_doc,
    read_input_shape, resolve, skip_rules,
};
use crate::edit::save::Flow;
use crate::edit::{EngineRuntime, EngineServices, versions};
use crate::import::MediaKind;
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
    /// 確かめた件数（重なりを除いた選んだアイテム）。
    pub total: u64,
    /// 当てる見込みのアイテム（選んだ順。確かめられなかったものを含む）。
    pub apply: Vec<i64>,
    /// 飛ばす・失敗する見込み（理由ごと。最初に出た順）。
    pub skips: Vec<PrecheckGroup>,
    /// 当てる側に数えたが、出力の形・失うもの・見積もりを確かめられなかったもの（理由ごと）。
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
    let items = dedup(&req.items);
    let total = items.len() as u64;
    let k = runtime.bulk_threads().max(1).min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let verdicts: Mutex<Vec<Option<Verdict>>> =
        Mutex::new((0..items.len()).map(|_| None).collect());
    let failure: Mutex<Option<CoreError>> = Mutex::new(None);
    let one = OneCx {
        library,
        engine,
        runtime: &runtime,
        recipe: &resolved.recipe,
        save_as,
        drafts,
        canceled: cx.canceled,
    };
    std::thread::scope(|s| {
        for _ in 0..k {
            s.spawn(|| {
                loop {
                    if cx.canceled.load(Ordering::Acquire) {
                        return;
                    }
                    let i = next.fetch_add(1, Ordering::AcqRel);
                    let Some(&item) = items.get(i) else { return };
                    match one.check(item) {
                        Ok(v) => {
                            if let Ok(mut all) = verdicts.lock()
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
    let verdicts = verdicts
        .into_inner()
        .map_err(|_| CoreError::Invalid("事前チェックの結果の錠が壊れました".into()))?;
    let mut out = Precheck {
        total,
        memory_budget: runtime.budget().total(),
        notes: resolved.notes,
        ..Precheck::default()
    };
    for (item, verdict) in items.iter().zip(verdicts) {
        let Some(verdict) = verdict else {
            return Err(CoreError::Canceled);
        };
        out.add(*item, verdict);
    }
    Ok(out)
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
        let got = self.budget.acquire(bytes, Class::Interactive, &self.mon);
        if matches!(&got, Err(e) if matches!(e.root(), EngineError::Limit(_))) {
            self.denied.set(true);
        }
        got
    }

    fn mark(&self, _decoding: bool) -> Result<()> {
        Ok(())
    }

    fn log(&self, line: &str) {
        tracing::debug!("{line}");
    }
}

impl OneCx<'_> {
    /// 1 件の見込み（誤りは事前チェックそのものの失敗。アイテムの誤りは Verdict に入れる）。
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
        let original = match db.read(|c| versions::original(c, item))? {
            Some(o) => o,
            None => {
                return Ok(Verdict::Skip(Skip::skipped(
                    "Gone",
                    "アイテムがありません".into(),
                )));
            }
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
        let head_doc = match head_doc(db, self.runtime, item, head.version) {
            Ok(g) => g,
            Err(e) => return Ok(Verdict::Skip(Skip::failed(&e))),
        };
        let facts = db.read(|c| facts_of(c, item))?;
        // 入力の形（保存が読むのと同じもの）
        let hooks = PrecheckShape {
            mon: CancelMon(self.canceled),
            budget: self.runtime.budget(),
            denied: std::cell::Cell::new(false),
        };
        let read: InputRead =
            match read_input_shape(self.engine, self.library, item, &head_doc, &hooks) {
                Ok(Flow::Go(read)) => read,
                Ok(Flow::Stop(_)) => return Err(CoreError::Canceled),
                Err(_) if hooks.denied.get() => {
                    return Ok(Verdict::Unchecked {
                        code: "Busy",
                        message: "メモリが空いていないので、当てるときに確かめます".into(),
                        composed,
                        estimate: rough,
                    });
                }
                Err(e) => return Ok(Verdict::Skip(Skip::failed(&e))),
            };
        let shapes = BTreeMap::from([(InputId(0), read.shape)]);
        // 継ぎ足して結び付ける
        let instance = match self
            .runtime
            .bulk(|e| e.instantiate(&head_doc, self.recipe, &shapes, &facts))
        {
            Ok(instance) => instance,
            Err(BindError::NoLength { .. }) => {
                return Ok(Verdict::Unchecked {
                    code: "NoLength",
                    message: "終わりからの秒は、当てるときに元の長さを数えて決めます".into(),
                    composed,
                    estimate: rough,
                });
            }
            Err(e) => return Ok(Verdict::Skip(bind_skip(e))),
        };
        let bound = if self.recipe.bindings.is_empty() {
            None
        } else {
            bound_json(&instance.bound)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
        };
        let mut graph = instance.graph;
        let source_format = read.format;
        let codecs = self.engine.codecs();
        // 出力の形と形式（保存の段 1・2 と同じ確かめ。別名は代わりの形式も。一括と同じ順）
        let mut fallback = false;
        let first = self.output(&graph, &shapes, source_format, head.kind);
        let (plan, out_shape, out_format, enc_opts) = match first {
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
            ) if rate > 0 => {
                let out_ms = u128::from(n) * 1000 / u128::from(rate);
                out_ms.abs_diff(u128::try_from(ms).unwrap_or(0)) > 1
            }
            _ => false,
        };
        let has_settings = facts.grid.is_some()
            || facts.pivot.is_some()
            || (matches!(out_shape, Shape::Audio { .. }) && facts.loop_range.is_some());
        let mut losses: Vec<(Loss, String)> =
            losses_of(caps.as_ref(), &out_shape, facts.loop_range.is_some())
                .into_iter()
                .map(|l| (l, out_format.clone()))
                .collect();
        if !self.save_as && resized && has_settings {
            losses.push((Loss::Settings, String::new()));
        }
        // 見積もり（メモリ。予算の全体を超えるなら当てるときに失敗する）
        let streams = matches!(out_shape, Shape::Audio { .. })
            && codecs.encode_streams(
                &out_format,
                &Shape::Audio {
                    rate: 48_000,
                    channels: 2,
                    frames: Dim::Exact(48_000),
                },
            );
        let input_len = std::fs::metadata(&read.path).map_or(0, |m| m.len());
        let mut spec = EstimateSpec::new(self.runtime.bulk_threads() as u64)
            .digests(true)
            .input(
                InputId(0),
                Source::Decode {
                    codecs: &codecs,
                    format: source_format,
                },
            )
            .input_len(InputId(0), input_len)
            .output(
                OutputId(0),
                Sink::Encode {
                    codecs: &codecs,
                    format: &out_format,
                    opts: &enc_opts,
                },
            );
        if streams {
            spec = spec.stream(OutputId(0));
        }
        let peak = self
            .runtime
            .bulk(|e| e.estimate(&plan, &spec, None))
            .map_or(0, |est| est.peak);
        let budget = self.runtime.budget().total();
        if peak > budget {
            return Ok(Verdict::Skip(Skip::failed(&CoreError::TooLarge(format!(
                "メモリの予算を超える見込みです（見積もり {peak} バイト。予算は {budget} バイト）"
            )))));
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
        })))
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

impl OneCx<'_> {
    /// 文書 `graph` を入力の形で compile し、出力の形と形式を確かめる（保存の段 1・2 と同じ：描けない・種類を変える形式は
    /// NotApplicable、知らない形式・書き手がないは Unwritable）。
    fn output(
        &self,
        graph: &Graph,
        shapes: &BTreeMap<InputId, Shape>,
        source_format: &str,
        kind: MediaKind,
    ) -> std::result::Result<Output, Attempt> {
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
        let desc = format_by_id(&out_format)
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
