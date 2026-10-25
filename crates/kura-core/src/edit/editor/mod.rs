//! エディタのセッション（M12 の段 4。docs/editing.md §12.8・§12.10〜§12.13、ADR-141・154）。
//!
//! 開いているライブラリに 1 つだけ（`EngineServices` が持つ。M12 の決定）。エンジンの `EditSession`（履歴）・ノードのキャッシュ・
//! 予算の確保・入力・版ごとの計画・前回の表示を持ち、手を当てて表示の経路（`render_view`）で描き、変わったタイルを返す。
//! タイルの中身は kura:// の `/edit/…`（[`tile`]）で配り、値を IPC で送らない。
//!
//! - **開く**（[`open`]）：アイテムの頭の文書（下書きから開くなら下書きの文書と土台）と、元を読む（アンカーがあれば
//!   アンカー。[`source`]。読みも対話の予算で確保し、足りなければ Busy・TooLarge）。入力を `register_external` で数え、
//!   取り置き（`default_reserved`）を立て、キャッシュの確保（`CacheLease`）を開く。下書きの有無・このまま保存できるか
//!   （`fresh`）を返す。前のエディタは閉じる。
//! - **手**（[`step`]）：手をまとめて当て（確定する手は仮の値で計画を作れることを確かめてから確定する。作れなければ断り、
//!   前の仮の値を含めて状態を変えない）、描くのは最後に 1 回。段は Rust が決め（[`view::view_level`]。ドット絵は DB の実効値）、確保は
//!   プールの外、描くのはプレビューのプール。新しい要求が来たら前の描画を止め（`Latest::supersede`。当てた手は残す）、止めた
//!   描画は `Superseded` で返す。版ごとの計画を小さな LRU で持つ（取り消しで描画のノードを読み直さない。§12.4）。
//! - **表示だけ**（[`view`]）：手のない [`step`]。保存の終わり・予算の駐車もここで拾う。
//! - **スポイト**（[`sample`]）：今の rev の等倍の計画で `spoit`（段の値からは拾わない）。描いている途中の表示は止めない。
//! - **保存**（[`save`]）：確定した頭を保存のジョブへ（`start_save`）。保存の間も編集を続けられ、同じアイテムの保存が待って
//!   いる間は次の保存を断る（Busy。このエディタが積んだものに加えて DB の保存のジョブで見るので、閉じて開き直しても効く）。ジョブが終わったら（次の要求で `jobs` を見る）、結果の版・中身で土台を更新して
//!   `mark_saved_at(積んだときの rev)`。版に戻したあとは開き直す（`needs_reopen`）。保存が予算を待って駐車しているときは、
//!   エディタのキャッシュを空にする（取り置きは外さない。M12 の決定）。
//! - **下書き**：頭が変わるたびに、間引いて（[`autosave::INTERVAL`]）頭の文書と土台を置く（`drafts::save`。保存のジョブが
//!   動いている間は `after_job`）。保存した頭に戻ったら捨てる。閉じるときは待っている分を置いてから止める。
//! - **閉じる**（[`close`]・`EngineServices::close_editor`）：エディタを閉じる・ライブラリを閉じる・切り替える・終了のどれでも
//!   同じ：中止 → キャッシュを空に → 確保を落とす → 取り置きを 0（NFR-17・ADR-154。取り置きは持ち主のエディタだけが外す：
//!   `Reservation`）。

mod autosave;
mod source;
#[cfg(test)]
mod tests;
pub mod tile;
pub mod view;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kura_engine::budget::default_reserved;
use kura_engine::codec::CodecSet;
use kura_engine::doc::{FormatChoice, InputId, NodeId, OutputId};
use kura_engine::monitor::{CancelFlag, Latest, Ticket};
use kura_engine::render::area::SpoitAt;
use kura_engine::render::cache::{FingerprintedInput, NodeCache};
use kura_engine::render::estimate::{CacheLease, CacheProbe, EstimateSpec, Source};
use kura_engine::render::region::PartialValue;
use kura_engine::render::view::{View, ViewRequest};
use kura_engine::session::{CommitMode, Edit, EditSession, HistoryLimits, Rev, Snapshot};
use kura_engine::{Digest, Engine, EngineError, Graph, Issue, Issues, Monitor, Plan, Shape};
use rusqlite::Connection;
use serde_json::Value as Json;

pub use tile::{TileError, TileFormat, TileRequest};
pub use view::{
    ChangedTiles, DocDelta, FrameInfo, LogSummary, NodeSummary, TILE, ViewInfo, Viewport,
};

use self::autosave::{Autosave, DraftJob};
use self::view::{
    changed_tiles, doc_delta, format_name, frames_of, shape_size, view_area, view_level,
};
use super::save::{Base, SaveAction, SaveRequest, check_doc, pending_save, start_save};
use super::versions::{self, FileState, VersionKind};
use super::{EngineRuntime, EngineServices, anchors, drafts, store};
use crate::CoreError;
use crate::import::{MediaKind, format_id};
use crate::jobs::{JobId, JobManager, JobState};
use crate::library::{Library, LibrarySession, hex, parse_hex32};

/// 版ごとの計画を覚える数（取り消し・やり直しで描画のノードを読み直さない。docs/editing.md §12.4）。
const PLAN_LRU: usize = 8;

/// エディタを使うのに要るもの（開いているライブラリの部品）。
#[derive(Clone, Copy)]
pub struct EditorEnv<'a> {
    pub library: &'a Arc<Library>,
    pub jobs: &'a JobManager,
    pub engine: &'a Arc<EngineServices>,
}

impl<'a> EditorEnv<'a> {
    /// 開いているライブラリから（エンジンを使わないライブラリなら Unavailable）。
    pub fn of(session: &'a LibrarySession) -> Result<Self, EditorError> {
        let engine = session.engine().ok_or_else(|| {
            EditorError::Unavailable("このライブラリではエンジンを使えません".into())
        })?;
        Ok(Self {
            library: session.library(),
            jobs: session.jobs(),
            engine,
        })
    }
}

/// エディタのエラー（src-tauri が `AppError` の code に写す）。
#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    /// エディタがない（閉じた・別のアイテムを開いた・開いている途中）。
    #[error("エディタがありません（閉じたか、別のアイテムを開きました）")]
    NoSession,
    /// エンジンを使えない。
    #[error("エンジンを使えません: {0}")]
    Unavailable(String),
    #[error("見つかりません: {0}")]
    NotFound(String),
    /// 手・文書・入力が正しくない（指摘はノード・引数つき）。
    #[error("{message}")]
    Invalid { message: String, issues: Vec<Issue> },
    /// 大きすぎる（メモリの予算の全体を超える。やり直しても同じ）。
    #[error("大きすぎます: {0}")]
    TooLarge(String),
    /// 混んでいる（予算が今は空いていない・前の保存が終わっていない。待ってやり直せば通りうる）。
    #[error("混んでいます: {0}")]
    Busy(String),
    /// 外部ツールがない（名前）。
    #[error("{0} が見つかりません。外部ツールの欄の案内に従って入れてください")]
    ToolMissing(String),
    #[error("競合しています: {0}")]
    Conflict(String),
    /// 記録と食い違う（アンカーが使えず、元を記録のデコーダで読み直した値が違う）。
    #[error("記録と食い違います: {0}")]
    Mismatch(String),
    /// 開いている途中で閉じた・ほかのアイテムを開いた。
    #[error("中止しました（エディタを閉じたか、ほかのアイテムを開きました）")]
    Canceled,
    /// 読めない・描けない。
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Core(#[from] CoreError),
}

impl EditorError {
    /// エンジンのエラーから（`what` は何をしようとしたか）。
    pub(crate) fn from_engine(what: &str, e: EngineError) -> Self {
        let message = format!("{what}：{e}");
        match e.root() {
            EngineError::Invalid(issues) => Self::Invalid {
                message,
                issues: issues.0.clone(),
            },
            EngineError::Limit(_) => Self::TooLarge(message),
            EngineError::CodecMissing { tool } => Self::ToolMissing(tool.clone()),
            EngineError::Canceled => Self::Canceled,
            EngineError::Mismatch(_) => Self::Mismatch(message),
            EngineError::Unsupported(_)
            | EngineError::Decode { .. }
            | EngineError::Encode { .. }
            | EngineError::Io(_)
            | EngineError::Internal(_)
            | EngineError::AtNode { .. } => Self::Failed(message),
        }
    }

    /// 結果に入れる形（手の断り・表示の失敗）。
    pub fn problem(&self) -> Problem {
        let code = match self {
            Self::Invalid { .. } | Self::NotFound(_) | Self::Failed(_) => ProblemCode::Invalid,
            Self::TooLarge(_) => ProblemCode::TooLarge,
            Self::Busy(_) => ProblemCode::Busy,
            Self::ToolMissing(_) => ProblemCode::ToolMissing,
            Self::Conflict(_) | Self::Mismatch(_) | Self::Canceled | Self::NoSession => {
                ProblemCode::Conflict
            }
            Self::Unavailable(_) | Self::Core(_) => ProblemCode::Failed,
        };
        let issues = match self {
            Self::Invalid { issues, .. } => issues.clone(),
            _ => Vec::new(),
        };
        Problem {
            code,
            message: self.to_string(),
            issues,
        }
    }
}

/// 断り・失敗の種類（src-tauri の `AppErrorCode` に写す）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProblemCode {
    Invalid,
    TooLarge,
    Busy,
    ToolMissing,
    Conflict,
    Failed,
}

/// 結果に入れる断り・失敗（指摘はノード・引数つき）。
#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    pub code: ProblemCode,
    pub message: String,
    pub issues: Vec<Issue>,
}

/// エディタの手 1 つ（`Edit` に、仮の値・取り消し・描画の便宜を足したもの）。
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// 確定する（`Merge` は同じ引数を続けて変える手を 1 手にまとめる）。
    Commit {
        edit: Edit,
        mode: CommitMode,
    },
    /// 仮の値（スライダー・ドラッグの途中。履歴に入らない）。
    Preview {
        edit: Edit,
    },
    /// 仮の値を確定する。
    CommitPreview {
        mode: CommitMode,
    },
    /// 仮の値を捨てる。
    CancelPreview,
    Undo,
    Redo,
    /// まとめている途中の手を閉じる（blur・Enter）。
    Seal,
    /// 描画の動作（`paint.actions` の要素の JSON）を 1 手で足す：末尾が有効な `paint.actions` ならそこへ追記、ほかなら
    /// 末尾に `paint.actions` を足す（最初の 1 本も 1 手。M12 の決定）。
    Paint {
        actions: Vec<Json>,
    },
}

/// エディタの状態（どの結果にも付ける）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 今の文書の番号（仮の値があればその番号）。タイルはこの番号で取る。
    pub rev: u64,
    /// 確定した頭の番号。
    pub head: u64,
    /// 仮の値を出している。
    pub provisional: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    /// 確定した頭が保存したものと違う。
    pub dirty: bool,
    /// 動いている保存のジョブ。
    pub saving: Option<JobId>,
    /// 版に戻したので開き直す（手・保存は Conflict）。
    pub needs_reopen: bool,
}

/// 表示の結果。
#[derive(Debug, Clone, PartialEq)]
pub enum ViewOutcome {
    Shown(ViewInfo),
    /// 新しい表示の要求（手・表示だけ）に取って代わられた（当てた手は残る。取って代わった要求が表示を返すので、その結果を
    /// 待つ。スポイトは取って代わらない）。
    Superseded,
    /// 描けない（予算・文書の誤り）。手は当たっている（取り消せる）。
    Failed(Problem),
}

/// 断った手。
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    /// 何番目の手か（それより前の手は当たり、それより後の手は当てていない）。
    pub index: usize,
    pub problem: Problem,
}

/// 手（と表示）の結果。
#[derive(Debug, Clone, PartialEq)]
pub struct StepOutcome {
    pub status: Status,
    /// 足したノード（`AddNode`・最初の描画）。
    pub created: Vec<NodeId>,
    pub rejected: Option<Rejected>,
    /// 前に返した文書からの差分。
    pub doc: DocDelta,
    pub view: ViewOutcome,
    /// 知らせ（保存が終わった・失敗した・予算を待っている保存のためにキャッシュを空にした、など）。
    pub notices: Vec<String>,
}

/// 下書きの有無。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DraftInfo {
    pub exists: bool,
    /// このまま保存できる（土台が今と同じで、待っている保存がない）。
    pub fresh: bool,
    pub updated_at: Option<i64>,
    /// 下書きから開いた。
    pub opened: bool,
}

/// 元の形の要約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    /// `image`・`frames`・`audio`。
    pub kind: &'static str,
    pub width: u64,
    pub height: u64,
    /// フレームの数（フレームの列だけ）。
    pub frames: Option<u64>,
    /// 画素の形式（`rgba8`・`indexed8` など）。ファイルの形式（PNG など）ではない（ファイルの形式はアイテムの拡張子から）。
    pub format: Option<String>,
}

/// 開く要求。
#[derive(Debug, Clone, PartialEq)]
pub struct OpenRequest {
    pub item_id: i64,
    pub viewport: Viewport,
    /// 下書きがあれば下書きの文書と土台で開く（なければ頭の文書）。
    pub from_draft: bool,
}

/// 開いた結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    /// セッション（kura:// の `/edit/{session}/…`。再起動をまたいで重ならない）。
    pub session: String,
    pub item_id: i64,
    /// 土台の版（保存はこの版を土台にする）。
    pub base_version: i64,
    pub source: SourceInfo,
    /// ドット絵として表示する（DB の実効値。段は 0、拡大は最近傍）。
    pub pixel_art: bool,
    pub draft: DraftInfo,
    /// 開いたときの状態・文書（全体）・表示。
    pub outcome: StepOutcome,
}

/// 保存の向き。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveMode {
    Overwrite,
    SaveAs {
        title: String,
        stem: String,
    },
    /// 版 `to` に戻す（0 が元）。終わったら開き直す。
    Revert {
        to: i64,
    },
}

/// 保存を積んだ結果。
#[derive(Debug, Clone, PartialEq)]
pub struct SaveStarted {
    pub job: JobId,
    pub status: Status,
    /// 出力の形式を変えたときの文書の差分（変えなければ空）。
    pub doc: DocDelta,
}

/// スポイトで拾った 1 画素。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sampled {
    /// ストレートの RGBA8（16 ビットは四捨五入）。
    pub rgba: [u8; 4],
    /// パレットの番号（Indexed8）。
    pub index: Option<u8>,
}

/// 版の一覧の行（文字だけ。途中の版は実ファイルがないので絵は出さない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSummary {
    pub version: i64,
    /// `original`・`edit`・`revert`。
    pub kind: &'static str,
    pub revert_to: Option<i64>,
    pub ext: String,
    pub format: Option<String>,
    pub size_bytes: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    /// `hold`・`waiting`・`kept`・`released`（実ファイルを持っているか）。
    pub file_state: &'static str,
    pub created_at: i64,
}

/// 動いている保存。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Saving {
    job: JobId,
    /// 積んだときの確定した頭。
    rev: Rev,
    kind: SaveKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveKind {
    Overwrite,
    SaveAs,
    Revert,
}

/// 表示した今か直前の表示（配信はこれだけを読み、描いている途中の錠を待たない）。
struct Shown {
    rev: u64,
    level: u8,
    view: View,
}

#[derive(Default)]
struct Published {
    now: Option<Shown>,
    before: Option<Shown>,
}

/// 開いているエディタの状態（`Editor::state` の錠の中）。
struct State {
    session: EditSession,
    inputs: BTreeMap<InputId, FingerprintedInput>,
    shapes: BTreeMap<InputId, Shape>,
    cache: NodeCache,
    lease: CacheLease,
    output: OutputId,
    /// 今の文書（仮の値を含む）の等倍の計画と、その番号。
    plan: Plan,
    plan_rev: Rev,
    /// 版ごとの計画（古い順。最後がいちばん新しい）。
    plans: Vec<(Rev, Plan)>,
    /// 前回の表示（`render_view` の前の値）。
    view: Option<View>,
    /// 前にフロントへ返した文書とその番号。
    reported: (Snapshot, Rev),
    base: Base,
    pixel_art: bool,
    saving: Option<Saving>,
    needs_reopen: bool,
}

/// エディタの取り置き（`default_reserved`）の持ち主（エディタのセッション。ライブラリに 1 つ。`EngineServices` が持つ）。
/// 立てる・外すはこの錠の中で行い、外すのは持ち主のときだけ（閉じ終わる前に別のアイテムを開いたとき、古いエディタの閉じ
/// 終わりが新しいエディタの取り置きを消さないように）。
#[derive(Debug, Default)]
pub(crate) struct Reservation(Mutex<Option<String>>);

impl Reservation {
    fn lock(&self) -> MutexGuard<'_, Option<String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// エディタ `owner` の取り置き `bytes` を立てる（持ち主を `owner` にする）。
    fn hold(
        &self,
        budget: &kura_engine::budget::MemoryBudget,
        owner: &str,
        bytes: u64,
    ) -> Result<(), EngineError> {
        let mut slot = self.lock();
        budget.set_reserved(bytes)?;
        *slot = Some(owner.to_string());
        Ok(())
    }

    /// エディタ `owner` が持ち主なら、取り置きを 0 にして持ち主をなくす（ほかのエディタが持っていれば何もしない）。
    fn release(&self, budget: &kura_engine::budget::MemoryBudget, owner: &str) {
        let mut slot = self.lock();
        if slot.as_deref() != Some(owner) {
            return;
        }
        if let Err(err) = budget.set_reserved(0) {
            tracing::warn!(%err, "エディタの取り置きを外せませんでした");
        }
        *slot = None;
    }
}

/// 開いているエディタ（ライブラリに 1 つ。`EngineServices` が持つ）。
pub struct Editor {
    id: String,
    item_id: i64,
    runtime: Arc<EngineRuntime>,
    codecs: Arc<CodecSet>,
    /// 取り置きの持ち主（ライブラリの `EngineServices` と共有）。
    reservation: Arc<Reservation>,
    latest: Arc<Latest>,
    cancel: Arc<CancelFlag>,
    /// None：開いている途中か閉じた。
    state: Mutex<Option<State>>,
    published: Mutex<Published>,
    autosave: Autosave,
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor")
            .field("id", &self.id)
            .field("item_id", &self.item_id)
            .finish()
    }
}

/// 再起動をまたいで重ならないセッションの値：時刻・プロセス番号・通し番号のダイジェストの 16 桁 ＋ 通し番号（16 進）。
fn session_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = blake3::Hasher::new();
    h.update(b"kura-editor-session-v1");
    h.update(&nanos.to_le_bytes());
    h.update(&std::process::id().to_le_bytes());
    h.update(&seq.to_le_bytes());
    format!("{}{seq:x}", hex(&h.finalize().as_bytes()[..8]))
}

/// セッションの値として受け付ける綴り（kura:// のパス。小文字の 16 進、1〜64 文字）。
pub fn is_session_id(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Editor {
    /// このエディタのセッションの値。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 編集しているアイテム。
    pub fn item_id(&self) -> i64 {
        self.item_id
    }

    /// 開いたときのコーデックの組（タイルの PNG を書く）。
    pub(crate) fn codecs(&self) -> Arc<CodecSet> {
        self.codecs.clone()
    }

    fn lock_state(&self) -> MutexGuard<'_, Option<State>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_published(&self) -> MutexGuard<'_, Published> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// 表示した今か直前の表示のうち、rev と段が同じものの値（配信用。なければ None）。
    pub fn published_value(&self, rev: u64, level: u8) -> Option<PartialValue> {
        let p = self.lock_published();
        [p.now.as_ref(), p.before.as_ref()]
            .into_iter()
            .flatten()
            .find(|s| s.rev == rev && s.level == level)
            .map(|s| s.view.value().clone())
    }

    fn publish(&self, rev: u64, level: u8, view: View) {
        let mut p = self.lock_published();
        p.before = p.now.take();
        p.now = Some(Shown { rev, level, view });
    }

    /// 閉じる：中止 → キャッシュを空に → 確保を落とす → 取り置きを 0（このエディタが取り置きの持ち主のときだけ。閉じ終わる前に
    /// 開いた次のエディタの取り置きは消さない）。待っている下書きを置いてから下書きのスレッドを止める。2 回目からは何もしない。
    pub(crate) fn shutdown(&self) {
        self.cancel.cancel();
        let state = self.lock_state().take();
        if let Some(st) = state {
            let State {
                mut cache, lease, ..
            } = st;
            cache.clear();
            drop(lease);
        }
        self.reservation.release(self.runtime.budget(), &self.id);
        *self.lock_published() = Published::default();
        self.autosave.close();
        tracing::debug!(
            session = self.id,
            item_id = self.item_id,
            "エディタを閉じました"
        );
    }

    /// 状態から結果の状態を作る。
    fn status(st: &State) -> Status {
        Status {
            rev: st.session.rev().0,
            head: st.session.head_rev().0,
            provisional: st.session.is_provisional(),
            can_undo: st.session.can_undo(),
            can_redo: st.session.can_redo(),
            dirty: st.session.is_dirty(),
            saving: st.saving.map(|s| s.job),
            needs_reopen: st.needs_reopen,
        }
    }

    /// 前に返した文書からの差分を作り、返したことにする。
    fn report_doc(st: &mut State) -> DocDelta {
        let now = st.session.current().clone();
        let rev = st.session.rev();
        let delta = doc_delta(Some((&st.reported.0, st.reported.1.0)), &now);
        st.reported = (now, rev);
        delta
    }

    /// 頭が変わった：下書きを頼む（保存していない頭か、保存のジョブが動いていれば置く。保存した頭に戻ったら捨てる）。
    fn schedule_draft(&self, st: &State) {
        let saving = st
            .saving
            .filter(|s| s.kind != SaveKind::Revert)
            .map(|s| s.job);
        if st.session.is_dirty() || saving.is_some() {
            self.autosave.request(DraftJob::Put {
                snapshot: st.session.head().clone(),
                base: st.base,
                after_job: saving,
            });
        } else {
            self.autosave.request(DraftJob::Discard);
        }
    }

    /// 動いている保存のジョブを見る：終わっていれば土台・保存の印を更新し、予算を待って駐車していればキャッシュを空にする。
    fn sync_save(&self, env: &EditorEnv<'_>, st: &mut State, notices: &mut Vec<String>) {
        let Some(saving) = st.saving else {
            return;
        };
        let record = match env.jobs.get(saving.job) {
            Ok(r) => r,
            Err(err) => {
                tracing::warn!(%err, job = saving.job, "保存のジョブを読めません");
                return;
            }
        };
        match record.state {
            JobState::Queued | JobState::Running | JobState::Paused => {
                self.relieve(st, notices);
                return;
            }
            JobState::Done => {
                let result: Json = record
                    .result_json
                    .as_deref()
                    .and_then(|r| serde_json::from_str(r).ok())
                    .unwrap_or(Json::Null);
                self.saved(env, st, saving, &result);
                notices.push("保存しました".into());
            }
            JobState::Failed | JobState::Canceled => {
                notices.push(format!(
                    "保存できませんでした（{}）",
                    record.message.as_deref().unwrap_or("理由は不明です")
                ));
            }
        }
        st.saving = None;
    }

    /// 保存のジョブが終わった：上書きは結果の版・中身で土台を更新し、積んだときの手に保存の印。別名も保存の印（作業は新しい
    /// アイテムに残った）。版に戻したなら開き直す。
    fn saved(&self, env: &EditorEnv<'_>, st: &mut State, saving: Saving, result: &Json) {
        let outcome = result.get("outcome").and_then(Json::as_str).unwrap_or("");
        match saving.kind {
            SaveKind::Overwrite => {
                let from_result = (result.get("itemId").and_then(Json::as_i64)
                    == Some(self.item_id))
                .then(|| {
                    let hash = result
                        .get("hash")
                        .and_then(Json::as_str)
                        .and_then(parse_hex32)?;
                    let version = result.get("version").and_then(Json::as_i64)?;
                    Some(Base { hash, version })
                })
                .flatten();
                let base = from_result.or_else(|| {
                    env.library
                        .db()
                        .read(|c| versions::head(c, self.item_id))
                        .ok()
                        .flatten()
                        .map(|h| Base {
                            hash: h.hash,
                            version: h.version,
                        })
                });
                if let Some(base) = base {
                    st.base = base;
                }
                st.session.mark_saved_at(saving.rev);
            }
            SaveKind::SaveAs => {
                if matches!(outcome, "saved" | "unchanged" | "existing") {
                    st.session.mark_saved_at(saving.rev);
                }
            }
            SaveKind::Revert => st.needs_reopen = true,
        }
    }

    /// 保存が予算を待って駐車している：キャッシュを空にして確保を戻す（入力は数え直す。取り置きは外さない。M12 の決定）。
    /// 駐車の条件は「予算 − max(取り置き, 持ち続ける対話)」なので、効くのは固定した値と入力でキャッシュの確保が取り置きを
    /// 超えているとき（取り置きのほうで超えるなら、エディタを閉じるまで通らない。docs/editing.md §12.10）。入力だけに
    /// なっていれば何もしない（駐車している間、手ごとに空にし直さない）。
    fn relieve(&self, st: &mut State, notices: &mut Vec<String>) {
        let parked = self
            .runtime
            .budget()
            .usage()
            .map(|u| u.parked > 0)
            .unwrap_or(false);
        if !parked {
            return;
        }
        let mut inputs_only = NodeCache::new(u64::MAX);
        for input in st.inputs.values() {
            inputs_only.register_external(&input.value);
        }
        if st.cache.bytes() <= inputs_only.bytes() {
            return;
        }
        let before = st.cache.bytes();
        st.cache.clear();
        for input in st.inputs.values() {
            st.cache.register_external(&input.value);
        }
        if let Err(err) = st.lease.settle(&mut st.cache) {
            tracing::warn!(%err, "エディタの確保を戻せませんでした");
        }
        tracing::info!(
            before,
            after = st.cache.bytes(),
            "保存が予算を待っているので、エディタのキャッシュを空にしました"
        );
        notices.push("保存のためにエディタのキャッシュを空にしました".into());
    }

    /// 手を当てて表示する（[`step`] の本体）。
    fn run(
        &self,
        env: &EditorEnv<'_>,
        steps: Vec<Step>,
        vp: &Viewport,
    ) -> Result<StepOutcome, EditorError> {
        let ticket = self.latest.issue(self.cancel.clone());
        self.latest.supersede(ticket.generation());
        let mut guard = self.lock_state();
        let st = guard.as_mut().ok_or(EditorError::NoSession)?;
        let mut notices = Vec::new();
        self.sync_save(env, st, &mut notices);
        if st.needs_reopen && !steps.is_empty() {
            return Err(EditorError::Conflict(
                "版に戻したので、エディタを開き直してください".into(),
            ));
        }
        let head = st.session.head_rev();
        let (created, rejected) = self.runtime.preview(|e| apply_steps(e, st, steps));
        if st.session.head_rev() != head {
            self.schedule_draft(st);
        }
        let view = self.render(st, vp, &ticket)?;
        Ok(StepOutcome {
            status: Self::status(st),
            created,
            rejected,
            doc: Self::report_doc(st),
            view,
            notices,
        })
    }

    /// 今の文書を表示の経路で描く（段は Rust が決める）。描けなければ `Failed`、新しい要求に取って代わられたら `Superseded`、
    /// 閉じたら Canceled。
    fn render(
        &self,
        st: &mut State,
        vp: &Viewport,
        ticket: &Ticket,
    ) -> Result<ViewOutcome, EditorError> {
        if st.plan_rev != st.session.rev() {
            return Ok(ViewOutcome::Failed(Problem {
                code: ProblemCode::Invalid,
                message: "今の文書の計画を作れません（取り消すか、引数を直してください）".into(),
                issues: Vec::new(),
            }));
        }
        let started = Instant::now();
        let Some(full) = st
            .plan
            .output_shapes()
            .into_iter()
            .find(|(id, _)| *id == st.output)
            .map(|(_, s)| s)
        else {
            return Ok(ViewOutcome::Failed(Problem {
                code: ProblemCode::Invalid,
                message: "文書に出力がありません".into(),
                issues: Vec::new(),
            }));
        };
        let want = view_level(&full, vp.scale, vp.interacting, st.pixel_art);
        let lp = match self.runtime.preview(|e| e.plan_level(&st.plan, want)) {
            Ok(lp) => lp,
            Err(e) => return failed(EditorError::from_engine("段の計画を作れません", e)),
        };
        let level = lp.plan.level();
        let area = view_area(&full, vp, level);
        let spec = EstimateSpec::new(0).all_inputs(&lp.plan, Source::Held);
        if let Err(e) = self.runtime.admit_preview(
            &lp.plan,
            spec.clone(),
            &st.inputs,
            &mut st.cache,
            &mut st.lease,
        ) {
            let problem = self.admit_problem(&lp.plan, spec, st, e);
            settle(st);
            return Ok(ViewOutcome::Failed(problem));
        }
        let req = ViewRequest {
            output: st.output,
            area,
            focus: vp.focus,
        };
        let (inputs, cache, prev) = (&st.inputs, &mut st.cache, st.view.as_ref());
        let r = self
            .runtime
            .preview(|e| e.render_view(&lp.plan, inputs, cache, &req, prev, ticket));
        settle(st);
        match r {
            Ok(vr) => {
                let rev = st.session.rev().0;
                let value = vr.view.value();
                let info = ViewInfo {
                    rev,
                    level: vr.level(),
                    fallback: lp.fallback.as_ref().map(view::fallback_name),
                    full: shape_size(&full),
                    canvas: shape_size(&value.shape()),
                    frames: frames_of(value),
                    format: format_name(value),
                    tile: TILE,
                    changed: changed_tiles(&vr.changed),
                    meta_changed: vr.meta_changed,
                    warnings: vr.warnings.clone(),
                    drawn_nodes: vr.drawn_nodes(),
                    ms: started.elapsed().as_secs_f64() * 1000.0,
                };
                self.publish(rev, info.level, vr.view.clone());
                st.view = Some(vr.view);
                Ok(ViewOutcome::Shown(info))
            }
            Err(e) if matches!(e.root(), EngineError::Canceled) => {
                if self.cancel.canceled() {
                    Err(EditorError::Canceled)
                } else {
                    Ok(ViewOutcome::Superseded)
                }
            }
            Err(e) => failed(EditorError::from_engine("表示を描けません", e)),
        }
    }

    /// 確保できなかった理由：キャッシュの当たりを引いた見積もりが予算の全体を超えれば TooLarge（やり直しても同じ）、ほかは
    /// Busy（一括が使っている・キャッシュの固定した値で埋まっている）。
    fn admit_problem(
        &self,
        plan: &Plan,
        spec: EstimateSpec<'_>,
        st: &State,
        e: EngineError,
    ) -> Problem {
        if !matches!(e.root(), EngineError::Limit(_)) {
            return EditorError::from_engine("メモリを確保できません", e).problem();
        }
        let total = self.runtime.budget().total();
        let spec = spec.tasks(self.runtime.preview_threads() as u64);
        let probe = CacheProbe {
            cache: &st.cache,
            inputs: &st.inputs,
        };
        let peak = self
            .runtime
            .preview(|eng| eng.estimate(plan, &spec, Some(probe)))
            .map(|est| est.peak)
            .unwrap_or(u64::MAX);
        let err = if peak > total {
            EditorError::TooLarge(e.to_string())
        } else {
            EditorError::Busy(e.to_string())
        };
        err.problem()
    }
}

/// 描けなかった結果。
fn failed(err: EditorError) -> Result<ViewOutcome, EditorError> {
    match err {
        EditorError::Canceled => Err(err),
        other => Ok(ViewOutcome::Failed(other.problem())),
    }
}

/// 描いたあと：確保をキャッシュの分に戻す。
fn settle(st: &mut State) {
    if let Err(err) = st.lease.settle(&mut st.cache) {
        tracing::warn!(%err, "エディタの確保を戻せませんでした");
    }
}

/// 版ごとの計画から `rev` のものを取り出す。
fn take_plan(plans: &mut Vec<(Rev, Plan)>, rev: Rev) -> Option<Plan> {
    let at = plans.iter().position(|(r, _)| *r == rev)?;
    Some(plans.remove(at).1)
}

/// 版ごとの計画に覚える（いちばん新しいものとして。古いものから捨てる）。
fn remember(plans: &mut Vec<(Rev, Plan)>, rev: Rev, plan: Plan) {
    plans.retain(|(r, _)| *r != rev);
    plans.push((rev, plan));
    while plans.len() > PLAN_LRU {
        plans.remove(0);
    }
}

/// 今の文書（仮の値を含む）の計画を用意する（覚えていればそれ、なければ今の計画から差分で作る）。
fn ensure_plan(e: &Engine, st: &mut State) -> Result<(), Issues> {
    let rev = st.session.rev();
    if rev == st.plan_rev {
        return Ok(());
    }
    let plan = match take_plan(&mut st.plans, rev) {
        Some(plan) => plan,
        None => e.compile_snapshot(st.session.current(), &st.shapes, Some(&st.plan))?,
    };
    remember(&mut st.plans, rev, plan.clone());
    st.plan = plan;
    st.plan_rev = rev;
    Ok(())
}

/// 頭に手を当てた文書で計画を作れるか確かめてから、仮の値にする（`EditSession::preview_if`）。作れなければ断り、状態を
/// 変えない（前の仮の値・その計画・表示もそのまま）。作った計画は版ごとの計画に覚える。
fn preview_checked(e: &Engine, st: &mut State, edit: Edit) -> Result<(), EngineError> {
    let (shapes, prev) = (&st.shapes, &st.plan);
    let (rev, plan) = st.session.preview_if(e, edit, |snap| {
        e.compile_snapshot(snap, shapes, Some(prev))
            .map_err(EngineError::Invalid)
    })?;
    remember(&mut st.plans, rev, plan.clone());
    st.plan = plan;
    st.plan_rev = rev;
    Ok(())
}

/// 確かめてから確定する（仮の値で計画を作れたら、その仮の値を確定する。`commit` と同じ手・同じまとめ方）。
fn commit_checked(
    e: &Engine,
    st: &mut State,
    edit: Edit,
    mode: CommitMode,
) -> Result<Option<NodeId>, EngineError> {
    preview_checked(e, st, edit)?;
    let c = st.session.commit_preview(mode)?;
    // まとめて前の手に戻ったときは、その手の計画（覚えていなければ作る）
    ensure_plan(e, st).map_err(EngineError::Invalid)?;
    Ok(c.created)
}

/// 描画の動作を 1 手にする手（[`Step::Paint`]）：末尾が有効な `paint.actions` ならそこへ追記、ほかなら末尾に足す。
pub fn paint_edit(e: &Engine, head: &Snapshot, actions: Vec<Json>) -> Result<Edit, EngineError> {
    const OP: &str = "paint.actions";
    if let Some(last) = head.nodes().last()
        && last.op == OP
        && last.enabled
    {
        return Ok(Edit::AppendItems {
            node: last.id,
            field: "actions".into(),
            items: actions,
        });
    }
    let v = e
        .registry()
        .current_version(OP)
        .ok_or_else(|| EngineError::Internal(format!("{OP} がありません")))?;
    Ok(Edit::AddNode {
        at: head.node_count(),
        op: OP.into(),
        v,
        params: serde_json::json!({ "actions": actions }),
        seed: None,
        extra_inputs: Vec::new(),
    })
}

/// 手を順に当てる（断った手で止める。それより前の手は残る）。足したノードと、断った手を返す。
fn apply_steps(e: &Engine, st: &mut State, steps: Vec<Step>) -> (Vec<NodeId>, Option<Rejected>) {
    let mut created = Vec::new();
    for (index, step) in steps.into_iter().enumerate() {
        let r: Result<Option<NodeId>, EngineError> = match step {
            Step::Commit { edit, mode } => commit_checked(e, st, edit, mode),
            Step::Preview { edit } => preview_checked(e, st, edit).map(|()| None),
            Step::CommitPreview { mode } => st.session.commit_preview(mode).and_then(|c| {
                ensure_plan(e, st)
                    .map_err(EngineError::Invalid)
                    .map(|()| c.created)
            }),
            Step::CancelPreview => {
                st.session.cancel_preview();
                ensure_plan(e, st)
                    .map_err(EngineError::Invalid)
                    .map(|()| None)
            }
            Step::Undo => {
                st.session.undo();
                ensure_plan(e, st)
                    .map_err(EngineError::Invalid)
                    .map(|()| None)
            }
            Step::Redo => {
                st.session.redo();
                ensure_plan(e, st)
                    .map_err(EngineError::Invalid)
                    .map(|()| None)
            }
            Step::Seal => {
                st.session.seal();
                Ok(None)
            }
            Step::Paint { actions } => paint_edit(e, st.session.head(), actions)
                .and_then(|edit| commit_checked(e, st, edit, CommitMode::Step)),
        };
        match r {
            Ok(Some(id)) => created.push(id),
            Ok(None) => {}
            Err(err) => {
                // 断った手は状態を変えないが、念のため計画を今の番号に合わせる（同じなら何もしない。覚えていればそれ）
                if let Err(issues) = ensure_plan(e, st) {
                    tracing::warn!(?issues, "断った手のあと、今の文書の計画を作れません");
                }
                return (
                    created,
                    Some(Rejected {
                        index,
                        problem: EditorError::from_engine("手を当てられません", err).problem(),
                    }),
                );
            }
        }
    }
    (created, None)
}

/// 開いているエディタ（セッションが違えば NoSession）。
fn editor_of(env: &EditorEnv<'_>, session: &str) -> Result<Arc<Editor>, EditorError> {
    env.engine.editor_for(session).ok_or(EditorError::NoSession)
}

/// 手を当てて表示する（モジュールの説明）。`steps` が空なら表示だけ（[`view`]）。
pub fn step(
    env: &EditorEnv<'_>,
    session: &str,
    steps: Vec<Step>,
    viewport: &Viewport,
) -> Result<StepOutcome, EditorError> {
    editor_of(env, session)?.run(env, steps, viewport)
}

/// 表示だけ（パン・ズーム・段の変更。保存の終わりもここで拾う）。
pub fn view(
    env: &EditorEnv<'_>,
    session: &str,
    viewport: &Viewport,
) -> Result<StepOutcome, EditorError> {
    step(env, session, Vec::new(), viewport)
}

/// スポイト：今の rev の等倍の計画の出力の 1 画素（キャンバスの座標）。画像の外は None。表示の要求ではないので、描いている
/// 途中の表示（手・表示の要求）を止めず（`Latest` の世代を払い出さない。止めると、止めた要求の代わりの表示がどこからも
/// 返らない）、錠を待ってから拾う。止まるのはエディタを閉じたときだけ（等倍の 1 画素なので軽い）。
pub fn sample(
    env: &EditorEnv<'_>,
    session: &str,
    frame: u32,
    x: i64,
    y: i64,
) -> Result<Option<Sampled>, EditorError> {
    let editor = editor_of(env, session)?;
    let mut guard = editor.lock_state();
    let st = guard.as_mut().ok_or(EditorError::NoSession)?;
    if st.plan_rev != st.session.rev() {
        return Err(EditorError::Invalid {
            message: "今の文書の計画を作れません".into(),
            issues: Vec::new(),
        });
    }
    let spec = EstimateSpec::new(0).all_inputs(&st.plan, Source::Held);
    let plan = st.plan.clone();
    if let Err(e) = editor.runtime.admit_preview(
        &plan,
        spec.clone(),
        &st.inputs,
        &mut st.cache,
        &mut st.lease,
    ) {
        let problem = editor.admit_problem(&plan, spec, st, e);
        settle(st);
        return Err(match problem.code {
            ProblemCode::TooLarge => EditorError::TooLarge(problem.message),
            _ => EditorError::Busy(problem.message),
        });
    }
    let at = SpoitAt {
        output: st.output,
        frame,
        x,
        y,
    };
    let (inputs, cache, mon) = (&st.inputs, &mut st.cache, editor.cancel.as_ref());
    let r = editor
        .runtime
        .preview(|e| e.spoit(&plan, inputs, cache, &at, mon));
    settle(st);
    match r {
        Ok(s) => Ok(s.map(|s| Sampled {
            rgba: s.rgba,
            index: s.index,
        })),
        Err(e) => Err(EditorError::from_engine("色を拾えません", e)),
    }
}

/// エディタを閉じる（セッションが今のものでなければ何もしない）。
pub fn close(engine: &EngineServices, session: &str) {
    if let Some(editor) = engine.take_editor(Some(session)) {
        editor.shutdown();
    }
}

/// 保存を積む（モジュールの説明）。`format` を渡すと、先に出力の形式を変える手を確定する（取り消せる）。
pub fn save(
    env: &EditorEnv<'_>,
    session: &str,
    mode: SaveMode,
    format: Option<FormatChoice>,
) -> Result<SaveStarted, EditorError> {
    let editor = editor_of(env, session)?;
    let mut guard = editor.lock_state();
    let st = guard.as_mut().ok_or(EditorError::NoSession)?;
    let mut notices = Vec::new();
    editor.sync_save(env, st, &mut notices);
    if st.needs_reopen {
        return Err(EditorError::Conflict(
            "版に戻したので、エディタを開き直してください".into(),
        ));
    }
    if let Some(s) = st.saving {
        return Err(EditorError::Busy(format!(
            "前の保存（ジョブ {}）が終わるまで待ってください",
            s.job
        )));
    }
    // このエディタを開く前に積んだ同じアイテムの保存（閉じて開き直したとき）も、形式を変える手を確定する前に断る（積むときにも
    // 同じトランザクションで確かめ直す）
    if let Some(job) = env.library.db().read(|c| pending_save(c, editor.item_id))? {
        return Err(EditorError::Busy(format!(
            "前の保存（ジョブ {job}）が終わるまで待ってください"
        )));
    }
    let revert = matches!(mode, SaveMode::Revert { .. });
    if let Some(format) = format.filter(|_| !revert) {
        let out = st
            .session
            .head()
            .outputs()
            .first()
            .cloned()
            .ok_or_else(|| EditorError::Failed("文書に出力がありません".into()))?;
        if out.format != format {
            let edit = Edit::SetOutput {
                output: out.id,
                format,
                quantize: out.quantize,
                expand: out.expand,
            };
            editor
                .runtime
                .preview(|e| commit_checked(e, st, edit, CommitMode::Step))
                .map_err(|e| EditorError::from_engine("出力の形式を変えられません", e))?;
        }
    }
    let rev = st.session.head_rev();
    let (action, kind) = match mode {
        SaveMode::Overwrite => (
            SaveAction::Overwrite {
                graph: st.session.head().to_graph(),
            },
            SaveKind::Overwrite,
        ),
        SaveMode::SaveAs { title, stem } => (
            SaveAction::SaveAs {
                graph: st.session.head().to_graph(),
                title,
                stem,
            },
            SaveKind::SaveAs,
        ),
        SaveMode::Revert { to } => (SaveAction::Revert { to }, SaveKind::Revert),
    };
    // 同じアイテムの保存が DB で待っていれば（このエディタを開く前に積んだものを含む）Busy
    let job = start_save(
        env.library.db(),
        env.jobs,
        &SaveRequest {
            item_id: editor.item_id,
            base: st.base,
            action,
        },
    )
    .map_err(|e| match e {
        CoreError::Busy(message) => EditorError::Busy(message),
        other => other.into(),
    })?;
    st.saving = Some(Saving { job, rev, kind });
    if !revert {
        // 保存を積んだあとも編集を続けるので、下書きは `after_job` 付きで置く（保存した文書と同じなら保存のジョブが消す）
        editor.schedule_draft(st);
    }
    Ok(SaveStarted {
        job,
        status: Editor::status(st),
        doc: Editor::report_doc(st),
    })
}

/// アイテムの下書きを捨てる（あったら真）。そのアイテムのエディタが開いていれば、待っている下書きも取り消す。
pub fn discard_draft(env: &EditorEnv<'_>, item_id: i64) -> Result<bool, EditorError> {
    if let Some(editor) = env.engine.current_editor()
        && editor.item_id == item_id
    {
        editor.autosave.cancel_pending();
    }
    Ok(drafts::discard(env.library.db(), item_id)?)
}

/// アイテムの版の一覧（版の順。まだ保存していなければ空）。
pub fn version_list(conn: &Connection, item_id: i64) -> crate::Result<Vec<VersionSummary>> {
    let kind = |k: VersionKind| match k {
        VersionKind::Original => "original",
        VersionKind::Edit => "edit",
        VersionKind::Revert => "revert",
    };
    let state = |s: FileState| match s {
        FileState::Hold => "hold",
        FileState::Waiting => "waiting",
        FileState::Kept => "kept",
        FileState::Released => "released",
    };
    Ok(versions::list(conn, item_id)?
        .into_iter()
        .map(|v| VersionSummary {
            version: v.version,
            kind: kind(v.kind),
            revert_to: v.revert_to,
            ext: v.ext,
            format: v.format,
            size_bytes: v.size_bytes,
            width: v.width,
            height: v.height,
            duration_ms: v.duration_ms,
            file_state: state(v.file_state),
            created_at: v.created_at,
        })
        .collect())
}

/// 下書きを置く間隔（テストは間引かない）。
fn draft_interval() -> Duration {
    if cfg!(test) {
        Duration::ZERO
    } else {
        autosave::INTERVAL
    }
}

/// エディタを開く（モジュールの説明）。前のエディタは閉じる。
pub fn open(env: &EditorEnv<'_>, req: &OpenRequest) -> Result<Opened, EditorError> {
    let db = env.library.db();
    let item_id = req.item_id;
    let runtime = env
        .engine
        .runtime()
        .map_err(|e| EditorError::Unavailable(e.to_string()))?
        .clone();
    // 前のエディタを閉じる（取り置き・キャッシュを外してから開く）
    if let Some(old) = env.engine.take_editor(None) {
        old.shutdown();
    }
    let head = db
        .read(|c| versions::head(c, item_id))?
        .ok_or_else(|| EditorError::NotFound(format!("アイテム {item_id}")))?;
    if head.trashed {
        return Err(EditorError::Conflict(
            "ゴミ箱のアイテムは編集できません".into(),
        ));
    }
    if head.kind != MediaKind::Image {
        return Err(EditorError::Invalid {
            message: format!("{} は編集できません（画像だけ）", head.kind.as_str()),
            issues: Vec::new(),
        });
    }
    let draft = db.read(|c| {
        let Some(d) = drafts::get(c, item_id)? else {
            return Ok(None);
        };
        let fresh = drafts::is_fresh(c, &d)?;
        Ok(Some((d, fresh)))
    })?;
    let mut info = DraftInfo {
        exists: draft.is_some(),
        fresh: draft.as_ref().is_some_and(|(_, f)| *f),
        updated_at: draft.as_ref().map(|(d, _)| d.updated_at),
        opened: false,
    };
    let (graph, base) = match (req.from_draft, &draft) {
        (true, Some(_)) => {
            let opened = drafts::open(db, &runtime, item_id)?
                .ok_or_else(|| EditorError::NotFound(format!("アイテム {item_id} の下書き")))?;
            info.opened = true;
            info.fresh = opened.fresh;
            (opened.graph, opened.base)
        }
        _ => {
            let v = db.read(|c| versions::get(c, item_id, head.version))?;
            let graph = match v.and_then(|v| v.doc_id) {
                Some(doc_id) => {
                    let bytes = db.read(|c| store::load_doc_bytes(c, doc_id))?;
                    runtime
                        .preview(|e| e.load(&bytes))
                        .map_err(|e| EditorError::from_engine("頭の文書を読めません", e))?
                }
                None => Graph::new_single(),
            };
            (
                graph,
                Base {
                    hash: head.hash,
                    version: head.version,
                },
            )
        }
    };
    check_doc(&graph)?;
    let output = graph
        .outputs
        .first()
        .map(|o| o.id)
        .ok_or_else(|| EditorError::Failed("文書に出力がありません".into()))?;
    let original = db
        .read(|c| versions::original(c, item_id))?
        .ok_or_else(|| EditorError::NotFound(format!("アイテム {item_id}")))?;
    let format = format_id(&original.ext).ok_or_else(|| EditorError::Invalid {
        message: format!("元の形式（{}）を読めません", original.ext),
        issues: Vec::new(),
    })?;
    let opts = graph
        .inputs
        .first()
        .map(|slot| slot.decode.clone())
        .unwrap_or_default();
    let options_digest = anchors::options_digest(&opts)?;
    let anchor = db.read(|c| anchors::find(c, &original.hash, &options_digest))?;
    let layout = env.library.layout();
    let pixel_art = db
        .read(|c| crate::asset::get(c, item_id))
        .map(|a| a.effective.pixel_art)
        .unwrap_or_else(|err| {
            tracing::warn!(%err, item_id, "ドット絵の判定を読めません（ドット絵でないとして開きます）");
            false
        });

    // ここからは閉じる・ほかを開くと止まる（エディタを置き場に入れてから読む）
    let codecs = env.engine.codecs();
    let editor = Arc::new(Editor {
        id: session_id(),
        item_id,
        runtime: runtime.clone(),
        codecs: codecs.clone(),
        reservation: env.engine.reservation(),
        latest: Latest::new(),
        cancel: Arc::new(CancelFlag::default()),
        state: Mutex::new(None),
        published: Mutex::new(Published::default()),
        autosave: Autosave::start(env.library.clone(), item_id, draft_interval()),
    });
    env.engine.install_editor(editor.clone());
    let result = finish_open(
        &editor,
        OpenParts {
            graph,
            base,
            output,
            original_path: layout.content_path(&original.hash, &original.ext),
            source: Digest(original.hash),
            format,
            opts,
            anchor: anchor.map(|a| {
                let path = layout.content_path(&a.hash, &a.ext);
                (a, path)
            }),
            pixel_art,
            from_draft: info.opened,
        },
        &req.viewport,
    );
    match result {
        Ok((source, outcome)) => Ok(Opened {
            session: editor.id.clone(),
            item_id,
            base_version: base.version,
            source,
            pixel_art,
            draft: info,
            outcome,
        }),
        Err(err) => {
            if let Some(e) = env.engine.take_editor(Some(&editor.id)) {
                e.shutdown();
            } else {
                editor.shutdown();
            }
            Err(err)
        }
    }
}

/// 開くのに読んだもの。
struct OpenParts {
    graph: Graph,
    base: Base,
    output: OutputId,
    original_path: std::path::PathBuf,
    source: Digest,
    format: &'static str,
    opts: kura_engine::doc::DecodeOptions,
    anchor: Option<(anchors::Anchor, std::path::PathBuf)>,
    pixel_art: bool,
    from_draft: bool,
}

/// 元を読み、状態を作って最初の表示を描く。
fn finish_open(
    editor: &Editor,
    parts: OpenParts,
    vp: &Viewport,
) -> Result<(SourceInfo, StepOutcome), EditorError> {
    let runtime = &editor.runtime;
    let limits = runtime.preview(|e| e.limits().clone());
    let started = Instant::now();
    let read = source::read(&source::SourceCx {
        runtime,
        codecs: &editor.codecs,
        original: &parts.original_path,
        source: parts.source,
        format: parts.format,
        opts: &parts.opts,
        anchor: parts.anchor,
        limits: &limits,
        mon: editor.cancel.as_ref(),
    })?;
    let shape = read.value.shape();
    let source_info = source_info(&shape, read.value.clone());
    let input = FingerprintedInput::decoded(read.value, parts.source, &read.decoded);
    let inputs = BTreeMap::from([(InputId(0), input)]);
    let shapes = BTreeMap::from([(InputId(0), shape)]);
    let (session, plan) = runtime.preview(|e| {
        let mut session = EditSession::open(
            e,
            &parts.graph,
            Digest(parts.base.hash),
            HistoryLimits::default(),
        )
        .map_err(|err| EditorError::from_engine("文書を開けません", err))?;
        if parts.from_draft {
            // 下書きはまだ保存していない文書
            session.clear_saved();
        }
        let plan = e
            .compile_snapshot(session.current(), &shapes, None)
            .map_err(|issues| {
                EditorError::from_engine("文書を描けません", EngineError::Invalid(issues))
            })?;
        Ok::<_, EditorError>((session, plan))
    })?;
    let rev = session.rev();
    let reported = (session.current().clone(), rev);
    // 閉じる・ほかを開くのと競らないよう、錠の中で中止を確かめてから取り置き・確保を立てる
    let mut guard = editor.lock_state();
    if editor.cancel.canceled() {
        return Err(EditorError::Canceled);
    }
    let budget = runtime.budget();
    let mut cache = NodeCache::new(0);
    for v in inputs.values() {
        cache.register_external(&v.value);
    }
    let reserved = default_reserved(budget.total());
    editor
        .reservation
        .hold(budget, &editor.id, reserved)
        .map_err(|e| EditorError::from_engine("取り置きを立てられません", e))?;
    let lease = match runtime.open_cache(&mut cache, reserved) {
        Ok(lease) => lease,
        Err(e) => {
            editor.reservation.release(budget, &editor.id);
            return Err(if cache.bytes() > budget.total() {
                EditorError::TooLarge(e.to_string())
            } else {
                EditorError::Busy(e.to_string())
            });
        }
    };
    let mut st = State {
        session,
        inputs,
        shapes,
        cache,
        lease,
        output: parts.output,
        plans: vec![(rev, plan.clone())],
        plan,
        plan_rev: rev,
        view: None,
        reported,
        base: parts.base,
        pixel_art: parts.pixel_art,
        saving: None,
        needs_reopen: false,
    };
    let ticket = editor.latest.issue(editor.cancel.clone());
    let view = editor.render(&mut st, vp, &ticket);
    let full = doc_delta(None, st.session.current());
    let status = Editor::status(&st);
    *guard = Some(st);
    drop(guard);
    let view = view?;
    tracing::info!(
        session = editor.id,
        item_id = editor.item_id,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "エディタを開きました"
    );
    Ok((
        source_info,
        StepOutcome {
            status,
            created: Vec::new(),
            rejected: None,
            doc: full,
            view,
            notices: read.warnings,
        },
    ))
}

/// 元の形の要約。
fn source_info(shape: &Shape, value: kura_engine::Value) -> SourceInfo {
    let (width, height) = shape_size(shape);
    let format = PartialValue::full(value);
    match shape {
        Shape::Image { .. } => SourceInfo {
            kind: "image",
            width,
            height,
            frames: None,
            format: format_name(&format),
        },
        Shape::Frames { count, .. } => SourceInfo {
            kind: "frames",
            width,
            height,
            frames: Some(count.max()),
            format: format_name(&format),
        },
        Shape::Audio { .. } => SourceInfo {
            kind: "audio",
            width,
            height,
            frames: None,
            format: None,
        },
    }
}

/// テストの補助（描画の動作の JSON）。
#[cfg(test)]
pub(crate) mod tests_support {
    use kura_engine::geom::IPoint;
    use kura_engine::paint::stroke::{Brush, BrushShape};
    use kura_engine::paint::{Blend, PaintAction, PaintColor, PointList, Tool};

    /// ペンのストローク（1px・置き換え）。
    pub fn stroke(points: &[(i64, i64)], rgba: [u8; 4]) -> serde_json::Value {
        serde_json::to_value(PaintAction::Stroke {
            tool: Tool::Pen,
            color: Some(PaintColor::Rgba(rgba)),
            blend: Blend::Replace,
            brush: Brush {
                shape: BrushShape::Square,
                size: 1,
            },
            pixel_perfect: false,
            frame: 0,
            points: PointList(points.iter().map(|&(x, y)| IPoint { x, y }).collect()),
        })
        .unwrap()
    }
}
