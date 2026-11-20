//! 一括加工（M14 段 5。docs/editing.md §14.2、docs/architecture.md「一括加工」、ADR-190・193・194・195）：事前チェック（と中止）・
//! 一括を積む・結果・1 件ずつの行・［元に戻す…］。一時停止・再開・中止はジョブの口（`job_*`）。ロジックは
//! `kura_core::edit::batch`。
//!
//! 事前チェックはジョブにしない（利用者がその場で待つ確かめ。ADR-195）：spawn_blocking で動かし、進みはイベント
//! `batch://precheck`（呼び手の決めた `token` で見分ける。100 ms ごとと最後）、シートを閉じたら `batch_precheck_cancel` で止める
//! （止めた事前チェックは kura-core の `Canceled`、IPC の code は `Conflict` を返す。エディタを開く途中の中止と同じ）。

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use kura_core::CoreError;
use kura_core::edit::EngineServices;
use kura_core::edit::batch::precheck::{
    BoundExample, DiskEstimate, Loss, LossGroup, OutputGroup, PrecheckGroup, ShapeChange,
    ShapeInfo, SheetInfo,
};
use kura_core::edit::batch::{
    self, BatchAction, BatchMode, BatchRequest, BatchResult, BatchRow, MergeRequest, Precheck,
    PrecheckCx, RecipeSource,
};
use kura_core::library::LibrarySession;
use kura_engine::recipe::RecipeNote;
use kura_engine::selection::SheetParams;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use super::blocking;
use crate::dto::JobStateDto;
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// 事前チェックの進みを送る間隔。
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// 覚えておく先に届いた中止の数（始まらなかった token が積もらないように古いものから捨てる）。
const EARLY_CANCELS: usize = 64;

/// 動いている事前チェックの中止の印（`token` ごと）と、始まる前に届いた中止（Tauri の async のコマンドはタスクごとに動くので、
/// `batch_precheck` が印を入れる前に `batch_precheck_cancel` が届きうる）。
#[derive(Default)]
struct Prechecks {
    running: BTreeMap<String, Arc<AtomicBool>>,
    early: VecDeque<String>,
}

impl Prechecks {
    /// 事前チェック `token` を始める（先に中止が届いていれば Canceled、同じ token が動いていれば Busy）。
    fn begin(&mut self, token: &str) -> Result<Arc<AtomicBool>, AppError> {
        if let Some(pos) = self.early.iter().position(|t| t == token) {
            self.early.remove(pos);
            return Err(CoreError::Canceled.into());
        }
        if self.running.contains_key(token) {
            return Err(AppError::new(
                AppErrorCode::Busy,
                "同じ事前チェックが動いています",
            ));
        }
        let flag = Arc::new(AtomicBool::new(false));
        self.running.insert(token.to_string(), flag.clone());
        Ok(flag)
    }

    /// 事前チェック `token` を止める（まだ始まっていなければ覚えておく）。
    fn cancel(&mut self, token: &str) {
        match self.running.get(token) {
            Some(flag) => flag.store(true, Ordering::Release),
            None => {
                if !self.early.iter().any(|t| t == token) {
                    self.early.push_back(token.to_string());
                }
                while self.early.len() > EARLY_CANCELS {
                    self.early.pop_front();
                }
            }
        }
    }

    /// 事前チェック `token` が終わった。
    fn end(&mut self, token: &str) {
        self.running.remove(token);
    }
}

static PRECHECKS: LazyLock<Mutex<Prechecks>> = LazyLock::new(|| Mutex::new(Prechecks::default()));

/// 当てるレシピの出どころ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Type)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum RecipeSourceDto {
    /// レシピの表のレシピ。
    Recipe { id: i64 },
    /// 一括のジョブを積んだときのレシピの写し（結果のシートの［やり直す］）。
    Job { job: i64 },
}

/// 一括の向き。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Type)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum BatchModeDto {
    Overwrite,
    /// 別名（`name` は名前の型。`{名前}` にアイテムの表示名が入る。既定は「{名前} のコピー」）。
    SaveAs {
        name: String,
    },
}

/// 一括の頼み（事前チェックと積むで同じ）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequestDto {
    pub source: RecipeSourceDto,
    /// 当てるアイテム（選んだ順）。
    pub items: Vec<i64>,
    pub mode: BatchModeDto,
    /// 上書きで、保存していない下書きのあるアイテムも含める（初めは外す）。
    pub include_drafts: bool,
}

impl From<BatchRequestDto> for BatchRequest {
    fn from(r: BatchRequestDto) -> Self {
        Self {
            source: match r.source {
                RecipeSourceDto::Recipe { id } => RecipeSource::Recipe(id),
                RecipeSourceDto::Job { job } => RecipeSource::Job(job),
            },
            items: r.items,
            mode: match r.mode {
                BatchModeDto::Overwrite => BatchMode::Overwrite,
                BatchModeDto::SaveAs { name } => BatchMode::SaveAs { name },
            },
            include_drafts: r.include_drafts,
        }
    }
}

/// N → 1（スプライトシート）の頼み。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MergeRequestDto {
    /// 並べるアイテム（並べる順。画面が選んだ順・名前順・一覧の並びを選ぶ）。
    pub items: Vec<i64>,
    /// できるアイテムの名前（`{名前}` は最初のアイテムの表示名。既定は「{名前} のシート」）。
    pub name: String,
    pub sheet: SheetParams,
}

impl From<MergeRequestDto> for MergeRequest {
    fn from(r: MergeRequestDto) -> Self {
        Self {
            items: r.items,
            name: r.name,
            sheet: r.sheet,
        }
    }
}

/// N → 1 のシートの見込み。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SheetInfoDto {
    pub width: u64,
    pub height: u64,
    pub frames: u64,
    pub columns: u32,
    pub cell_w: u64,
    pub cell_h: u64,
    /// 大きさ・フレームの数が上限しか分からない（作るときに決まる）。
    pub approximate: bool,
}

impl From<SheetInfo> for SheetInfoDto {
    fn from(s: SheetInfo) -> Self {
        Self {
            width: s.width,
            height: s.height,
            frames: s.frames,
            columns: s.columns,
            cell_w: s.cell_w,
            cell_h: s.cell_h,
            approximate: s.approximate,
        }
    }
}

/// 事前チェックの進み（イベント `batch://precheck`）。
#[derive(Debug, Clone, Serialize, Deserialize, Type, tauri_specta::Event)]
#[serde(rename_all = "camelCase")]
#[tauri_specta(event_name = "batch://precheck")]
pub struct BatchPrecheckProgress {
    /// `batch_precheck` に渡した印。
    pub token: String,
    pub done: u64,
    pub total: u64,
}

/// 理由ごとの群。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PrecheckGroupDto {
    /// `skipped`・`failed`（当てるときの行の状態）。確かめられなかったものは `pending`。
    pub state: String,
    pub code: String,
    pub message: String,
    pub items: Vec<i64>,
}

impl From<PrecheckGroup> for PrecheckGroupDto {
    fn from(g: PrecheckGroup) -> Self {
        Self {
            state: g.state,
            code: g.code,
            message: g.message,
            items: g.items,
        }
    }
}

/// 形の要約（大きさ・長さが上限しか分からなければ null）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum ShapeInfoDto {
    Image {
        w: Option<u64>,
        h: Option<u64>,
        /// フレームの列の数（1 枚の画像は null）。
        frames: Option<u64>,
    },
    Audio {
        rate: u32,
        channels: u16,
        frames: Option<u64>,
    },
}

impl From<ShapeInfo> for ShapeInfoDto {
    fn from(s: ShapeInfo) -> Self {
        match s {
            ShapeInfo::Image { w, h, frames } => Self::Image { w, h, frames },
            ShapeInfo::Audio {
                rate,
                channels,
                frames,
            } => Self::Audio {
                rate,
                channels,
                frames,
            },
        }
    }
}

/// 元の形 → 出力の形。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ShapeChangeDto {
    pub item_id: i64,
    pub from: ShapeInfoDto,
    pub to: ShapeInfoDto,
}

impl From<ShapeChange> for ShapeChangeDto {
    fn from(c: ShapeChange) -> Self {
        Self {
            item_id: c.item_id,
            from: c.from.into(),
            to: c.to.into(),
        }
    }
}

/// 出力の形式ごとの群。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OutputGroupDto {
    pub format: String,
    /// 別名で、レシピの形式に書けないので代わりの形式で作る。
    pub fallback: bool,
    pub items: Vec<i64>,
    pub examples: Vec<ShapeChangeDto>,
    /// 今の大きさ・長さから変わる件数。
    pub resized: u64,
}

impl From<OutputGroup> for OutputGroupDto {
    fn from(g: OutputGroup) -> Self {
        Self {
            format: g.format,
            fallback: g.fallback,
            items: g.items,
            examples: g.examples.into_iter().map(Into::into).collect(),
            resized: g.resized,
        }
    }
}

/// 失うもの（描かないので「ありうる」もの。docs/editing.md §14.2「事前チェック」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum LossDto {
    /// 非可逆の形式に書く。
    Lossy,
    /// 透明を持てない形式（透明な画素・パレットの透明な色があれば書けずに失敗する）。
    Alpha,
    /// 半透明を持てない形式（GIF）。
    SemiAlpha,
    /// 16 ビットを 8 ビットに。
    HighBits,
    /// 色の数に上限のある形式（減色する）。
    Colors { max: u32 },
    /// ファイルのループ点（解析の事実）を保存できない形式。
    LoopPoints,
    /// 上書きで大きさ・長さが変わり、素材の設定が合わなくなりうる。
    Settings,
}

impl From<Loss> for LossDto {
    fn from(l: Loss) -> Self {
        match l {
            Loss::Lossy => Self::Lossy,
            Loss::Alpha => Self::Alpha,
            Loss::SemiAlpha => Self::SemiAlpha,
            Loss::HighBits => Self::HighBits,
            Loss::Colors { max } => Self::Colors { max },
            Loss::LoopPoints => Self::LoopPoints,
            Loss::Settings => Self::Settings,
        }
    }
}

/// 失うものの群。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LossGroupDto {
    pub loss: LossDto,
    /// 書く形式（素材の設定は空）。
    pub format: String,
    pub items: Vec<i64>,
}

impl From<LossGroup> for LossGroupDto {
    fn from(g: LossGroup) -> Self {
        Self {
            loss: g.loss.into(),
            format: g.format,
            items: g.items,
        }
    }
}

/// ディスクの目安（バイト）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct DiskEstimateDto {
    /// 新しく書くファイルの目安（元の大きさを値の大きさの比で伸ばしたもの）。
    pub new_bytes: u64,
    /// 上書きで置き換わる今のファイル（7 日残してから片付ける）。
    pub replaced_bytes: u64,
}

impl From<DiskEstimate> for DiskEstimateDto {
    fn from(d: DiskEstimate) -> Self {
        Self {
            new_bytes: d.new_bytes,
            replaced_bytes: d.replaced_bytes,
        }
    }
}

/// 結び付けで読む値の例。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BoundExampleDto {
    pub item_id: i64,
    /// 読んだ値と書いた値（`recipe::bound_json` の形の JSON の文字列）。
    pub bound_json: String,
}

impl From<BoundExample> for BoundExampleDto {
    fn from(b: BoundExample) -> Self {
        Self {
            item_id: b.item_id,
            bound_json: b.bound.to_string(),
        }
    }
}

/// 事前チェックの結果（アイテムは ID の列だけ）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchPrecheckDto {
    /// 確かめた件数（ライブラリにあるもの）。
    pub total: u64,
    /// ライブラリにないアイテム（一括は行を入れない）。
    pub missing: Vec<i64>,
    /// 当てる見込み（確かめきれなかったものを含む）。
    pub apply: Vec<i64>,
    pub skips: Vec<PrecheckGroupDto>,
    /// 当てる側に数えたが確かめきれなかったもの（Busy：形を読めなかった。NoLength：終わりからの秒の長さは当てるときに決まる）。
    pub unchecked: Vec<PrecheckGroupDto>,
    pub outputs: Vec<OutputGroupDto>,
    pub losses: Vec<LossGroupDto>,
    /// 編集済みで、今の中身に続けて当てるもの。
    pub composed: Vec<i64>,
    pub disk: DiskEstimateDto,
    pub memory_peak: u64,
    pub memory_budget: u64,
    pub bound: Vec<BoundExampleDto>,
    pub notes: Vec<RecipeNote>,
    /// N → 1 のシートの見込み（1 件ずつの一括は null）。
    pub sheet: Option<SheetInfoDto>,
}

impl From<Precheck> for BatchPrecheckDto {
    fn from(p: Precheck) -> Self {
        Self {
            total: p.total,
            missing: p.missing,
            apply: p.apply,
            skips: p.skips.into_iter().map(Into::into).collect(),
            unchecked: p.unchecked.into_iter().map(Into::into).collect(),
            outputs: p.outputs.into_iter().map(Into::into).collect(),
            losses: p.losses.into_iter().map(Into::into).collect(),
            composed: p.composed,
            disk: p.disk.into(),
            memory_peak: p.memory_peak,
            memory_budget: p.memory_budget,
            bound: p.bound.into_iter().map(Into::into).collect(),
            notes: p.notes,
            sheet: p.sheet.map(Into::into),
        }
    }
}

/// 一括の向き（結果）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum BatchActionDto {
    Overwrite,
    SaveAs {
        name: String,
    },
    /// 一括 `of` の［元に戻す…］（`saveAs` は元が別名か N → 1、`merge` は N → 1 か）。
    Undo {
        of: i64,
        save_as: bool,
        merge: bool,
    },
    /// N → 1（スプライトシート）。
    Merge {
        name: String,
        sheet: SheetParams,
    },
}

impl From<BatchAction> for BatchActionDto {
    fn from(a: BatchAction) -> Self {
        match a {
            BatchAction::Overwrite => Self::Overwrite,
            BatchAction::SaveAs { name } => Self::SaveAs { name },
            BatchAction::Undo { of, save_as, merge } => Self::Undo { of, save_as, merge },
            BatchAction::Merge { name, sheet } => Self::Merge { name, sheet },
        }
    }
}

/// 状態と理由ごとの群。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchGroupDto {
    pub state: String,
    pub code: Option<String>,
    pub items: Vec<i64>,
}

/// 一括の結果（結果のシート）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchResultDto {
    pub job: i64,
    pub state: JobStateDto,
    pub action: BatchActionDto,
    /// 当てたレシピの名前（元に戻す一括は null）。
    pub recipe: Option<String>,
    pub include_drafts: bool,
    pub total: i64,
    pub groups: Vec<BatchGroupDto>,
    /// 別名で作ったアイテム。
    pub created: Vec<i64>,
    /// この一括を元に戻す一括（古い順）。
    pub undone_by: Vec<i64>,
    /// 元に戻せる件数（0 なら［元に戻す…］を押せない）。
    pub undo_available: i64,
}

impl From<BatchResult> for BatchResultDto {
    fn from(r: BatchResult) -> Self {
        Self {
            job: r.job,
            state: r.state.into(),
            action: r.action.into(),
            recipe: r.recipe,
            include_drafts: r.include_drafts,
            total: r.total,
            groups: r
                .groups
                .into_iter()
                .map(|g| BatchGroupDto {
                    state: g.state,
                    code: g.code,
                    items: g.items,
                })
                .collect(),
            created: r.created,
            undone_by: r.undo.by,
            undo_available: r.undo.available,
        }
    }
}

/// 1 件の行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BatchRowDto {
    pub ord: i64,
    pub item_id: i64,
    pub state: String,
    pub code: Option<String>,
    pub message: Option<String>,
    pub version_id: Option<i64>,
    pub out_item_id: Option<i64>,
    pub finished_at: Option<i64>,
}

impl From<BatchRow> for BatchRowDto {
    fn from(r: BatchRow) -> Self {
        Self {
            ord: r.ord,
            item_id: r.item_id,
            state: r.state,
            code: r.code,
            message: r.message,
            version_id: r.version_id,
            out_item_id: r.out_item_id,
            finished_at: r.finished_at,
        }
    }
}

/// このライブラリのエンジンの部品（エンジンを使わないライブラリなら断る）。
fn engine_of(session: &LibrarySession) -> Result<Arc<EngineServices>, AppError> {
    session.engine().cloned().ok_or_else(|| {
        AppError::new(
            AppErrorCode::Invalid,
            "このライブラリではエンジンを使えません",
        )
    })
}

/// 事前チェックを動かす（`token` の中止の印を登録し、spawn_blocking の中で `run` を呼び、進みを `batch://precheck` で送る。
/// 送るかを決めて送るまで錠を持つ：働き手が並べて呼ぶので、送る順を done の順にそろえる。最後の 1 件は必ず送る）。
async fn run_precheck<F>(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    token: String,
    run: F,
) -> Result<BatchPrecheckDto, AppError>
where
    F: FnOnce(&LibrarySession, &EngineServices, &PrecheckCx<'_>) -> kura_core::Result<Precheck>
        + Send
        + 'static,
{
    let session = state.session()?;
    let canceled = PRECHECKS
        .lock()
        .map_err(|_| AppError::internal("事前チェックの錠が壊れました"))?
        .begin(&token)?;
    let key = token.clone();
    let result = blocking(move || {
        let engine = engine_of(&session)?;
        let last = Mutex::new((None::<Instant>, 0u64));
        let progress = |done: u64, total: u64| {
            let Ok(mut sent) = last.lock() else { return };
            let (at, max) = *sent;
            if done <= max {
                return;
            }
            if done != total && at.is_some_and(|t: Instant| t.elapsed() < PROGRESS_EVERY) {
                return;
            }
            *sent = (Some(Instant::now()), done);
            let event = BatchPrecheckProgress {
                token: token.clone(),
                done,
                total,
            };
            if let Err(err) = tauri_specta::Event::emit(&event, &app) {
                tracing::warn!(%err, "事前チェックの進みを送れませんでした");
            }
        };
        let p = run(
            &session,
            &engine,
            &PrecheckCx {
                canceled: &canceled,
                progress: &progress,
            },
        )?;
        Ok(p.into())
    })
    .await;
    if let Ok(mut running) = PRECHECKS.lock() {
        running.end(&key);
    }
    result
}

/// 事前チェック（描かずに、当てられるか・出力の形・失うもの・見積もり・飛ばす理由）。進みは `batch://precheck`（`token`）。
/// `batch_precheck_cancel` で止めると code `Conflict`（kura-core の Canceled）。同じ `token` の事前チェックが動いていれば Busy。
#[tauri::command]
#[specta::specta]
pub async fn batch_precheck(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    token: String,
    request: BatchRequestDto,
) -> Result<BatchPrecheckDto, AppError> {
    let request: BatchRequest = request.into();
    run_precheck(app, state, token, move |session, engine, cx| {
        batch::precheck(session.library(), engine, &request, cx)
    })
    .await
}

/// N → 1（スプライトシート）の事前チェック（元ごとの飛ばす理由と、シートの大きさ・セル・メモリ・ディスクの見込み `sheet`）。
/// 進み・中止は `batch_precheck` と同じ（`batch://precheck`・`batch_precheck_cancel`）。
#[tauri::command]
#[specta::specta]
pub async fn merge_precheck(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    token: String,
    request: MergeRequestDto,
) -> Result<BatchPrecheckDto, AppError> {
    let request: MergeRequest = request.into();
    run_precheck(app, state, token, move |session, engine, cx| {
        batch::precheck_merge(session.library(), engine, &request, cx)
    })
    .await
}

/// N → 1（スプライトシート）の一括を積む（ジョブの ID を返す）。名前が空・どれもライブラリにないなら Invalid。結果・
/// ［元に戻す…］は `batch_result`・`batch_undo`（作ったアイテムは結果の `created`）。
#[tauri::command]
#[specta::specta]
pub async fn merge_start(
    state: State<'_, AppState>,
    request: MergeRequestDto,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || Ok(batch::start_merge(session.jobs(), &request.into())?)).await
}

/// 事前チェック `token` を止める（まだ始まっていなければ覚えておき、始まったらすぐ code `Conflict` で返す）。
#[tauri::command]
#[specta::specta]
pub async fn batch_precheck_cancel(token: String) -> Result<(), AppError> {
    if let Ok(mut running) = PRECHECKS.lock() {
        running.cancel(&token);
    }
    Ok(())
}

/// 一括のジョブを積む（ジョブの ID を返す）。レシピが読んだあとに変わったら Conflict、当てられないレシピ・対象がない・
/// 別名の名前の型が空なら Invalid、写しの文書が片付けられていれば NotFound。
#[tauri::command]
#[specta::specta]
pub async fn batch_start(
    state: State<'_, AppState>,
    request: BatchRequestDto,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let engine = engine_of(&session)?;
        Ok(batch::start_batch(
            session.library().db(),
            session.jobs(),
            &engine,
            &request.into(),
        )?)
    })
    .await
}

/// 一括の結果（向き・レシピ・状態と理由ごとの群・作ったアイテム・元に戻せるか）。
#[tauri::command]
#[specta::specta]
pub async fn batch_result(
    state: State<'_, AppState>,
    job: i64,
) -> Result<BatchResultDto, AppError> {
    let session = state.session()?;
    blocking(move || Ok(batch::result(session.library().db(), job)?.into())).await
}

/// 一括の行（行の順。`state`・`code` があればそれだけ。`offset` から `limit` 件、limit は 1〜1,000）。
#[tauri::command]
#[specta::specta]
pub async fn batch_rows(
    state: State<'_, AppState>,
    job: i64,
    row_state: Option<String>,
    code: Option<String>,
    offset: i64,
    limit: i64,
) -> Result<Vec<BatchRowDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(batch::rows(
            session.library().db(),
            job,
            row_state.as_deref(),
            code.as_deref(),
            offset,
            limit,
        )?
        .into_iter()
        .map(Into::into)
        .collect())
    })
    .await
}

/// 一括 `job` の［元に戻す…］を積む（ジョブの ID を返す）。元の一括が終わっていない・戻す一括が動いていれば Busy、戻す一括を
/// 戻す・戻すものがなければ Invalid。
#[tauri::command]
#[specta::specta]
pub async fn batch_undo(state: State<'_, AppState>, job: i64) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let engine = engine_of(&session)?;
        Ok(batch::start_undo(
            session.library().db(),
            session.jobs(),
            &engine,
            job,
        )?)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 始まる前に届いた中止は覚えておき、始まったらすぐ Canceled（code Conflict）。動いている間の中止は印を立てる。同じ
    /// token は Busy、終われば同じ token で始め直せる。覚える中止は古いものから捨てる。
    #[test]
    fn prechecks_remember_early_cancels() {
        let mut p = Prechecks::default();
        p.cancel("a");
        let early = p.begin("a").unwrap_err();
        assert_eq!(early.code, AppErrorCode::Conflict);
        let flag = p.begin("a").unwrap();
        assert!(!flag.load(Ordering::Acquire));
        assert_eq!(p.begin("a").unwrap_err().code, AppErrorCode::Busy);
        p.cancel("a");
        assert!(flag.load(Ordering::Acquire));
        p.end("a");
        assert!(p.begin("a").is_ok());
        for k in 0..(EARLY_CANCELS + 5) {
            p.cancel(&format!("t{k}"));
        }
        assert_eq!(p.early.len(), EARLY_CANCELS);
        assert!(p.begin("t0").is_ok(), "古い中止は捨てた");
    }
}
