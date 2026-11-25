//! 画像エディタのセッション（M12 の段 4。docs/editing.md §12.8・§12.12・§12.13、docs/architecture.md §3）。ロジックは
//! kura-core の `edit::editor`。
//!
//! - どれも `spawn_blocking` で動かし、ジョブにしない（利用者がその場で待つ表示の用意。描くのはエディタのプレビューのプール、
//!   予算の確保はプールの外）。保存（`edit_save`）だけが保存のジョブを積む。
//! - `edit_open` → `{ session, …, result }`：前のエディタは閉じる。下書きがあれば `draft.exists`（`fromDraft: true` で開き直すと
//!   下書きの文書で開く）。
//! - `edit_step`（手をまとめて当て、描くのは最後に 1 回）・`edit_view`（パン・ズーム・段の変更だけ）→ 状態・文書の差分・表示。
//!   表示のタイルは kura:// の `/edit/{session}/{rev}/{level}/{frame}/{tx}/{ty}`（`.png` で PNG）で、`view.changed` のタイル
//!   だけを今の rev で取り直す。新しい要求が来ると前の描画は止まり、`view.type` が `superseded` になる（当てた手は残る）。
//! - 手は kura-engine の `Edit`（feature `specta` で TS に出す）。断った手は `rejected`（指摘はノード・引数つき）、描けなければ
//!   `view.type` が `failed`（`TooLarge`・`Busy`・`Invalid`・`ToolMissing`）。
//!
//! DTO はほかの班の dto.rs の変更とぶつからないよう、このファイルに置く（名前は `Edit` で始める）。

use kura_core::edit::editor::audio::handles::{AudioNodeView, Handle, HandleMode, Marks};
use kura_core::edit::editor::audio::measure::{LoudnessState, LoudnessValues};
use kura_core::edit::editor::audio::peaks::PeaksInfo;
use kura_core::edit::editor::audio::timeline::SourcePiece;
use kura_core::edit::editor::formats::EditFormats;
use kura_core::edit::editor::{
    self, AudioLength, AudioSourceInfo, AudioViewInfo, AudioViewport, ChangedSpans, ChangedTiles,
    DocDelta, DraftInfo, EditorEnv, EditorError, FrameInfo, ImageViewInfo, ImageViewport,
    NodeSummary, OpenRequest, Opened, Problem, ProblemCode, Rejected, Sampled, SaveMode,
    SaveStarted, SourceInfo, Status, Step, StepOutcome, VersionSummary, ViewInfo, ViewOutcome,
    Viewport,
};
use kura_core::edit::save::{PendingSave, PendingSaveMode, pending_saves};
use kura_engine::Issue;
use kura_engine::doc::{FormatChoice, NodeId, OutputSpec};
use kura_engine::geom::IRect;
use kura_engine::op::OpDescriptor;
use kura_engine::session::{CommitMode, Edit};
use kura_engine::specta_types::JsonValue;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use super::blocking;
use crate::dto::JobStateDto;
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

// ===== 入力 =====

/// 矩形（等倍のキャンバスの座標）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct EditRectDto {
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
}

/// 区間（フレームの半開区間 [start, end)）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct EditSpanDto {
    pub start: u64,
    pub end: u64,
}

/// 表示の要求（ビューポート）：種類ごとの和（`type` で分ける）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditViewportDto {
    Image(EditImageViewportDto),
    Audio(EditAudioViewportDto),
}

/// 音声の表示の要求。区間はどれも出力のフレーム。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditAudioViewportDto {
    /// 見えている区間（波形の段の作り直しを見えている所から進める。描く範囲には入れない）。
    pub visible: Option<EditSpanDto>,
    /// 描いて配ってほしい区間（4 つまで。先のものほど優先。`/edit/{session}/{rev}/pcm/…` で取る区間）。
    pub spans: Vec<EditSpanDto>,
    /// 操作中（ドラッグの途中）：波形の段を直さない・裏の仕事をしない。
    pub interacting: bool,
    /// 編集中のノード（キャッシュに固定する）。
    pub focus: Option<u32>,
    /// 裏の仕事（波形の段の作り直し・ラウドネスの計り）を少し進める。
    pub work: bool,
    /// スペクトルの頼み（2 つまで：前と後。M17 段 4。省けば頼まない）。
    #[serde(default)]
    pub spec: Vec<EditSpecRequestDto>,
    /// 試聴の頼み（一時的に外して聞く。M17 段 6。省けば頼まない）。
    #[serde(default)]
    pub audition: Option<EditAuditionRequestDto>,
}

/// 試聴の頼み（M17 段 6）：ノード `bypass` を外した音を、区間 `spans`（再生の先読み。長い音声だけ使う）で描いて配る。文書は
/// 変えない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditAuditionRequestDto {
    pub bypass: u32,
    pub spans: Vec<EditSpanDto>,
}

/// 試聴の様子（M17 段 6）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditAuditionPhaseDto {
    /// 外した音を配れる（`auditionId` で取る）。
    Ready,
    /// 外した音の流し解きを裏で進めている（その間の `auditionId` は `samplesId`：外していない音）。
    Solving { progress: f64 },
    /// 外せない（文書にないノード・外すと出力の形が変わる・計画を作れない）。
    Unavailable,
}

/// 試聴の情報（M17 段 6）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditAuditionDto {
    /// 外すノード。
    pub node: u32,
    pub phase: EditAuditionPhaseDto,
    /// 試聴の値の有効な区間（`/edit/{session}/audition/{auditionId}/pcm/…` で取れる所）。
    pub valid: Vec<EditSpanDto>,
    /// 外して聞く音のラウドネス（`edit_measure` の `audition` で頼む。`auditionId` で覚える）。
    pub loudness: EditLoudnessStateDto,
}

impl From<kura_core::edit::editor::audio::audition::AuditionInfo> for EditAuditionDto {
    fn from(a: kura_core::edit::editor::audio::audition::AuditionInfo) -> Self {
        use kura_core::edit::editor::audio::audition::AuditionPhase;
        Self {
            node: a.node.0,
            phase: match a.phase {
                AuditionPhase::Ready => EditAuditionPhaseDto::Ready,
                AuditionPhase::Solving(progress) => EditAuditionPhaseDto::Solving { progress },
                AuditionPhase::Unavailable => EditAuditionPhaseDto::Unavailable,
            },
            valid: a
                .valid
                .into_iter()
                .map(|(start, end)| EditSpanDto { start, end })
                .collect(),
            loudness: a.loudness.into(),
        }
    }
}

/// 計る音の役（M17 段 6。音量を合わせて聞く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum EditMeasureTargetDto {
    /// 今の出力（表示の `samplesId`）。
    Output,
    /// 元（全部のノードを外した計画の出力。入力の全体）。
    Input,
    /// 外して聞く音（`auditionId`。外して聞いていなければ invalid）。
    Audition,
}

impl From<EditMeasureTargetDto> for kura_core::edit::editor::audio::measure::MeasureTarget {
    fn from(t: EditMeasureTargetDto) -> Self {
        match t {
            EditMeasureTargetDto::Output => Self::Output,
            EditMeasureTargetDto::Input => Self::Input,
            EditMeasureTargetDto::Audition => Self::Audition,
        }
    }
}

/// スペクトルのチャンネル（M17 段 4）：番号（0 から）・M・S・和（各チャンネルのパワーの和）。中身のない変種も `{}` の形に
/// する（内部のタグの単位の変種は知らない項目を拒まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum EditSpecChannelDto {
    Index { index: u16 },
    Mid {},
    Side {},
    Sum {},
}

impl From<EditSpecChannelDto> for kura_engine::audio::spec::SpecChannel {
    fn from(c: EditSpecChannelDto) -> Self {
        match c {
            EditSpecChannelDto::Index { index } => Self::Index(index),
            EditSpecChannelDto::Mid {} => Self::Mid,
            EditSpecChannelDto::Side {} => Self::Side,
            EditSpecChannelDto::Sum {} => Self::Sum,
        }
    }
}

impl From<kura_engine::audio::spec::SpecChannel> for EditSpecChannelDto {
    fn from(c: kura_engine::audio::spec::SpecChannel) -> Self {
        use kura_engine::audio::spec::SpecChannel;
        match c {
            SpecChannel::Index(index) => Self::Index { index },
            SpecChannel::Mid => Self::Mid {},
            SpecChannel::Side => Self::Side {},
            SpecChannel::Sum => Self::Sum {},
        }
    }
}

/// スペクトルのポートの向き（ノードの入力＝前・出力＝後）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum EditSpecSideDto {
    Input,
    Output,
}

impl From<EditSpecSideDto> for kura_core::edit::editor::audio::spec::SpecSide {
    fn from(s: EditSpecSideDto) -> Self {
        match s {
            EditSpecSideDto::Input => Self::Input,
            EditSpecSideDto::Output => Self::Output,
        }
    }
}

impl From<kura_core::edit::editor::audio::spec::SpecSide> for EditSpecSideDto {
    fn from(s: kura_core::edit::editor::audio::spec::SpecSide) -> Self {
        use kura_core::edit::editor::audio::spec::SpecSide;
        match s {
            SpecSide::Input => Self::Input,
            SpecSide::Output => Self::Output,
        }
    }
}

/// スペクトルの頼み 1 つ（M17 段 4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditSpecRequestDto {
    /// ノード（null は文書の出力で、向きは `output`。ノードは出力の時間の写しの連鎖にあるものだけ）。
    pub node: Option<u32>,
    pub side: EditSpecSideDto,
    pub channel: EditSpecChannelDto,
    /// 出力のフレームの区間（先のものほど先に作る）。
    pub spans: Vec<EditSpanDto>,
}

/// 画像の表示の要求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditImageViewportDto {
    /// 表示するフレーム（1 枚の画像は 0）。
    pub frame: u32,
    /// 表示する範囲（等倍のキャンバスの座標）。null は全体。
    pub rect: Option<EditRectDto>,
    /// 等倍の 1 画素あたりの装置の画素（拡大率 × devicePixelRatio）。段は Rust が決める。
    pub scale: f64,
    /// 操作中（ドラッグ・ズームの途中）なら 1 段粗く描く。
    pub interacting: bool,
    /// 編集中のノード（キャッシュに固定する）。
    pub focus: Option<u32>,
}

impl From<EditViewportDto> for Viewport {
    fn from(v: EditViewportDto) -> Self {
        match v {
            EditViewportDto::Image(v) => Self::Image(ImageViewport {
                frame: v.frame,
                rect: v.rect.map(|r| IRect::new(r.x, r.y, r.w, r.h)),
                scale: v.scale,
                interacting: v.interacting,
                focus: v.focus.map(NodeId),
            }),
            EditViewportDto::Audio(v) => Self::Audio(AudioViewport {
                visible: v.visible.map(|s| (s.start, s.end)),
                spans: v.spans.into_iter().map(|s| (s.start, s.end)).collect(),
                interacting: v.interacting,
                focus: v.focus.map(NodeId),
                work: v.work,
                spec: v
                    .spec
                    .into_iter()
                    .map(|r| kura_core::edit::editor::audio::spec::SpecRequest {
                        node: r.node.map(NodeId),
                        side: r.side.into(),
                        channel: r.channel.into(),
                        spans: r.spans.into_iter().map(|s| (s.start, s.end)).collect(),
                    })
                    .collect(),
                audition: v.audition.map(|a| {
                    kura_core::edit::editor::audio::audition::AuditionRequest {
                        bypass: NodeId(a.bypass),
                        spans: a.spans.into_iter().map(|s| (s.start, s.end)).collect(),
                    }
                }),
            }),
        }
    }
}

/// 手 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum EditStepDto {
    /// 確定する（`merge` は同じ引数を続けて変える手を 1 手にまとめる：キーボードの矢印・数値の入力）。
    Commit {
        edit: Edit,
        mode: CommitMode,
    },
    /// 仮の値（スライダー・ドラッグの途中。履歴に入らない）。
    Preview {
        edit: Edit,
    },
    /// 仮の値を確定する（ドラッグを離したら `step`）。
    CommitPreview {
        mode: CommitMode,
    },
    /// 仮の値を捨てる。
    CancelPreview,
    Undo,
    Redo,
    /// まとめている途中の手を閉じる（blur・Enter）。
    Seal,
    /// 描画の動作（`paint.actions` の要素）を 1 手で足す：末尾が有効な `paint.actions` ならそこへ追記、ほかなら末尾に足す。
    Paint {
        #[specta(type = Vec<JsonValue>)]
        actions: Vec<serde_json::Value>,
    },
    /// 音声のハンドル（表示している出力のフレーム）を、ノード `node` の引数にする（なければ末尾に足す）。Rust が下流の時間の
    /// 写しを逆にたどってノードの座標にする。外したノードは有効に戻す。写せなければ断る。
    Handle {
        handle: EditHandleDto,
        node: Option<u32>,
        mode: EditHandleModeDto,
    },
}

/// 音声のハンドル（どれも表示している出力のフレーム）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EditHandleDto {
    /// 切り詰めの枠 `[start, end)`。
    Trim { start: u64, end: u64 },
    /// フェードインの終わり。
    FadeIn { end: u64 },
    /// フェードアウトの始まり。
    FadeOut { start: u64 },
    /// ループの区間（null は消す）。
    Loop { range: Option<EditSpanDto> },
}

/// ハンドルの手の当て方（`preview` はドラッグの途中、`step`・`merge` は確定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum EditHandleModeDto {
    Preview,
    Step,
    Merge,
}

impl From<EditHandleDto> for Handle {
    fn from(h: EditHandleDto) -> Self {
        match h {
            EditHandleDto::Trim { start, end } => Self::Trim { start, end },
            EditHandleDto::FadeIn { end } => Self::FadeIn { end },
            EditHandleDto::FadeOut { start } => Self::FadeOut { start },
            EditHandleDto::Loop { range } => Self::Loop {
                range: range.map(|s| (s.start, s.end)),
            },
        }
    }
}

impl From<EditHandleModeDto> for HandleMode {
    fn from(m: EditHandleModeDto) -> Self {
        match m {
            EditHandleModeDto::Preview => Self::Preview,
            EditHandleModeDto::Step => Self::Commit(CommitMode::Step),
            EditHandleModeDto::Merge => Self::Commit(CommitMode::Merge),
        }
    }
}

impl From<EditStepDto> for Step {
    fn from(s: EditStepDto) -> Self {
        match s {
            EditStepDto::Commit { edit, mode } => Self::Commit { edit, mode },
            EditStepDto::Preview { edit } => Self::Preview { edit },
            EditStepDto::CommitPreview { mode } => Self::CommitPreview { mode },
            EditStepDto::CancelPreview => Self::CancelPreview,
            EditStepDto::Undo => Self::Undo,
            EditStepDto::Redo => Self::Redo,
            EditStepDto::Seal => Self::Seal,
            EditStepDto::Paint { actions } => Self::Paint { actions },
            EditStepDto::Handle { handle, node, mode } => Self::Handle {
                handle: handle.into(),
                node: node.map(NodeId),
                mode: mode.into(),
            },
        }
    }
}

/// 保存の向き。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum EditSaveModeDto {
    /// 上書き（アイテム ID を保ったまま版を足す）。
    Overwrite,
    /// 別名で保存（新しいアイテム。`stem` は元のファイル名の語幹、拡張子は出力の形式で決まる）。
    SaveAs { title: String, stem: String },
    /// 版 `to` に戻す（0 が元）。終わったら `status.needsReopen` が真になるので開き直す。
    Revert { to: i64 },
}

impl From<EditSaveModeDto> for SaveMode {
    fn from(m: EditSaveModeDto) -> Self {
        match m {
            EditSaveModeDto::Overwrite => Self::Overwrite,
            EditSaveModeDto::SaveAs { title, stem } => Self::SaveAs { title, stem },
            EditSaveModeDto::Revert { to } => Self::Revert { to },
        }
    }
}

// ===== 出力 =====

/// エディタの状態。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditStatusDto {
    /// 今の文書の番号（仮の値があればその番号）。タイルはこの番号で取る。
    pub rev: u64,
    /// 確定した頭の番号。
    pub head: u64,
    pub provisional: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    /// 確定した頭が保存したものと違う（閉じるときに確かめる）。
    pub dirty: bool,
    /// 動いている保存のジョブ（終わったら `edit_view` で状態を取り直す）。
    pub saving: Option<i64>,
    /// 版に戻したので開き直す。
    pub needs_reopen: bool,
}

impl From<Status> for EditStatusDto {
    fn from(s: Status) -> Self {
        Self {
            rev: s.rev,
            head: s.head,
            provisional: s.provisional,
            can_undo: s.can_undo,
            can_redo: s.can_redo,
            dirty: s.dirty,
            saving: s.saving,
            needs_reopen: s.needs_reopen,
        }
    }
}

/// 追記だけの配列の引数の数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct EditLogDto {
    pub field: String,
    pub count: u64,
}

/// ノードの要約。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct EditNodeDto {
    pub id: u32,
    pub op: String,
    pub v: u16,
    pub enabled: bool,
    /// 正規の形の引数（追記だけの配列は `[]`。要素の数は `log`）。
    #[specta(type = JsonValue)]
    pub params: serde_json::Value,
    pub log: Option<EditLogDto>,
}

impl From<NodeSummary> for EditNodeDto {
    fn from(n: NodeSummary) -> Self {
        Self {
            id: n.id.0,
            op: n.op,
            v: n.v,
            enabled: n.enabled,
            params: n.params,
            log: n.log.map(|l| EditLogDto {
                field: l.field,
                count: u64::try_from(l.count).unwrap_or(u64::MAX),
            }),
        }
    }
}

/// 文書の差分（前に返した文書から）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct EditDocDeltaDto {
    /// 差分の元の rev（null は全体：`nodes` にすべてのノード）。
    pub base: Option<u64>,
    /// 今のノードの並び。ここにないノードは消えた。
    pub order: Vec<u32>,
    /// 足した・変わったノード。
    pub nodes: Vec<EditNodeDto>,
    /// 出力（変わったときだけ）。
    pub outputs: Option<Vec<OutputSpec>>,
}

impl From<DocDelta> for EditDocDeltaDto {
    fn from(d: DocDelta) -> Self {
        Self {
            base: d.base,
            order: d.order.into_iter().map(|n| n.0).collect(),
            nodes: d.nodes.into_iter().map(Into::into).collect(),
            outputs: d.outputs,
        }
    }
}

/// フレームの矩形（段の座標）と表示時間。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditFrameDto {
    pub index: u32,
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
    pub duration_ms: Option<u32>,
}

impl From<FrameInfo> for EditFrameDto {
    fn from(f: FrameInfo) -> Self {
        Self {
            index: f.index,
            x: f.rect.x,
            y: f.rect.y,
            w: f.rect.w,
            h: f.rect.h,
            duration_ms: f.duration_ms,
        }
    }
}

/// フレームの変わったタイル（`[tx, ty]`。フレームの画像の座標）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct EditFrameTilesDto {
    pub frame: u32,
    pub tiles: Vec<[u32; 2]>,
}

/// 変わったタイル。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditChangedDto {
    /// すべて取り直す（最初の表示・段や形が変わった）。
    All,
    /// このタイルだけ取り直す（空なら何も変わっていない）。
    Tiles { frames: Vec<EditFrameTilesDto> },
}

impl From<ChangedTiles> for EditChangedDto {
    fn from(c: ChangedTiles) -> Self {
        match c {
            ChangedTiles::All => Self::All,
            ChangedTiles::Tiles(t) => Self::Tiles {
                frames: t
                    .into_iter()
                    .map(|(frame, tiles)| EditFrameTilesDto { frame, tiles })
                    .collect(),
            },
        }
    }
}

/// 表示した結果：種類ごとの和（`type` で分ける）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditViewDto {
    Image(EditImageViewDto),
    /// 音声（箱に入れる：画像の結果より大きい）。
    Audio(Box<EditAudioViewDto>),
}

impl From<ViewInfo> for EditViewDto {
    fn from(v: ViewInfo) -> Self {
        match v {
            ViewInfo::Image(v) => Self::Image(v.into()),
            ViewInfo::Audio(v) => Self::Audio(Box::new((*v).into())),
        }
    }
}

/// 出力のフレームの区間の変わり。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditSpansChangeDto {
    /// すべて取り直す（最初の表示・形が変わった）。
    All,
    /// この区間だけ取り直す（空なら何も変わっていない）。
    Spans { spans: Vec<EditSpanDto> },
}

impl From<ChangedSpans> for EditSpansChangeDto {
    fn from(c: ChangedSpans) -> Self {
        match c {
            ChangedSpans::All => Self::All,
            ChangedSpans::Spans(s) => Self::Spans {
                spans: s.into_iter().map(span).collect(),
            },
        }
    }
}

fn span((start, end): (u64, u64)) -> EditSpanDto {
    EditSpanDto { start, end }
}

/// 波形の段の状態（`/edit/{session}/{rev}/peaks/{bin}/{tile}` の `{rev}` は、この `rev`＝段の世代）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditPeaksInfoDto {
    /// 公開した今の段の世代（`/edit/{s}/{rev}/peaks/…` に使う番号。なければ null）。文書の rev とは別の数で、セッションの
    /// 中で公開のたびに増え、同じ番号のタイルの中身は変わらない（immutable）。直している途中（`pending`）は、前の文書の段か、
    /// 開いたときの仮の段（ライブラリの解析の段）のことがある。
    pub rev: Option<u64>,
    /// その段のレート・チャンネル数・長さ。
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// その段が持つ段のビンのフレーム数（細かい順。ライブラリの粗い段だけを仮に出しているときはその段だけ）。
    pub bins: Vec<u32>,
    /// タイル 1 枚のビンの数（1,024）。
    pub tile_bins: u32,
    /// 前に返した世代の段から変わった区間（この区間に掛かるタイルを取り直す）。
    pub changed: EditSpansChangeDto,
    /// 直している途中（古い段・仮の段を出している。手が止まったら `work: true` で頼み直す）。
    pub pending: bool,
}

impl From<PeaksInfo> for EditPeaksInfoDto {
    fn from(p: PeaksInfo) -> Self {
        Self {
            rev: p.rev,
            rate: p.rate,
            channels: p.channels,
            frames: p.frames,
            bins: p.bins,
            tile_bins: p.tile_bins,
            changed: p.changed.into(),
            pending: p.pending,
        }
    }
}

/// 元への写しの 1 区分（出力の `[outStart, outEnd)` は元の `inStart + round((out − outStart) × inRate / outRate)`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditSourcePieceDto {
    pub out_start: u64,
    pub out_end: u64,
    pub in_start: u64,
    pub in_rate: u32,
    pub out_rate: u32,
}

impl From<SourcePiece> for EditSourcePieceDto {
    fn from(p: SourcePiece) -> Self {
        Self {
            out_start: p.out_start,
            out_end: p.out_end,
            in_start: p.in_start,
            in_rate: p.in_rate,
            out_rate: p.out_rate,
        }
    }
}

/// ノードの引数の位置を出力のフレームに写したもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EditAudioMarksDto {
    /// 切り詰めの枠（このノードを外して入力を出しているときの位置）。
    Trim { start: u64, end: u64 },
    /// フェード。`inEnd`・`outStart` は三角の位置（フェードインの終わり・フェードアウトの始まり）。`inStart`・`outEnd` は
    /// 曲線を引く見かけの始まり・終わり（フェードの区間の写しを延ばした位置。符号つきで、下流で頭・終わりを切られていれば
    /// 出力の範囲の外）。どれも長さ 0・写せなければ null（`inStart`・`outEnd` が null なら曲線を描かない）。
    Fade {
        in_start: Option<i64>,
        in_end: Option<u64>,
        out_start: Option<u64>,
        out_end: Option<i64>,
    },
    /// ループの区間。
    Loop { start: u64, end: u64 },
}

impl From<Marks> for EditAudioMarksDto {
    fn from(m: Marks) -> Self {
        match m {
            Marks::Trim { start, end } => Self::Trim { start, end },
            Marks::Fade {
                in_start,
                in_end,
                out_start,
                out_end,
            } => Self::Fade {
                in_start,
                in_end,
                out_start,
                out_end,
            },
            Marks::Loop { start, end } => Self::Loop { start, end },
        }
    }
}

/// 出力につながるノード 1 つ（入力 → 出力の順）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditAudioNodeDto {
    pub id: u32,
    pub op: String,
    pub enabled: bool,
    /// 入力 0 のレートと長さ（引数の欄で時間と標本を変換する）・チャンネルの数（イコライザの曲線・チャンネルの選び方。M17 段 5）。
    pub in_rate: u32,
    pub in_frames: u64,
    pub in_channels: u16,
    /// ハンドルの位置（写せない・印のない操作は null）。
    pub marks: Option<EditAudioMarksDto>,
}

impl From<AudioNodeView> for EditAudioNodeDto {
    fn from(n: AudioNodeView) -> Self {
        Self {
            id: n.id.0,
            op: n.op,
            enabled: n.enabled,
            in_rate: n.in_rate,
            in_frames: n.in_frames,
            in_channels: n.in_channels,
            marks: n.marks.map(Into::into),
        }
    }
}

/// ラウドネスの値（`AssetAudioFactsDto` の同じ名前の欄と同じ意味。計れない値は null）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditLoudnessDto {
    /// 積分ラウドネス（LUFS）。
    pub loudness: Option<f64>,
    /// 積分を計れなかった理由（`tooShort`・`silent`・`belowGate`・`lowRate`・`nonFinite`）。
    pub loudness_reason: Option<String>,
    /// ラウドネスレンジ（LU）。
    pub loudness_range: Option<f64>,
    /// トゥルーピーク（dBTP）。
    pub true_peak: Option<f64>,
    /// サンプルピーク（dBFS）。
    pub sample_peak: Option<f64>,
}

impl From<LoudnessValues> for EditLoudnessDto {
    fn from(v: LoudnessValues) -> Self {
        Self {
            loudness: v.loudness,
            loudness_reason: v.loudness_reason,
            loudness_range: v.loudness_range,
            true_peak: v.true_peak,
            sample_peak: v.sample_peak,
        }
    }
}

/// ラウドネスの状態（`samplesId` は計った出力の指紋。今の表示の `samplesId` と違えば古い）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EditLoudnessStateDto {
    /// まだ計っていない（`edit_measure` で頼む）。
    Idle,
    /// 計っている（裏の仕事で進む）。
    Measuring {
        samples_id: String,
        progress: f64,
    },
    Ready {
        samples_id: String,
        loudness: EditLoudnessDto,
    },
    Failed {
        samples_id: String,
        message: String,
    },
}

impl From<LoudnessState> for EditLoudnessStateDto {
    fn from(s: LoudnessState) -> Self {
        match s {
            LoudnessState::Idle => Self::Idle,
            LoudnessState::Measuring {
                samples_id,
                progress,
            } => Self::Measuring {
                samples_id,
                progress,
            },
            LoudnessState::Ready { samples_id, values } => Self::Ready {
                samples_id,
                loudness: values.into(),
            },
            LoudnessState::Failed {
                samples_id,
                message,
            } => Self::Failed {
                samples_id,
                message,
            },
        }
    }
}

/// マーカー（出力のフレームの位置と名前）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct EditMarkerDto {
    pub pos: u64,
    pub name: String,
}

/// 音声の表示した結果（`/edit/{session}/{rev}/pcm/{a}-{b}` で、`valid` の区間の PCM を取る）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditAudioViewDto {
    pub rev: u64,
    /// 今の出力のレート・チャンネル数・長さ（フレーム）。
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// 今の出力のループ点（終わりは含まない）。
    #[serde(rename = "loop")]
    pub loop_range: Option<EditSpanDto>,
    pub markers: Vec<EditMarkerDto>,
    /// 出力の指紋（量子化を含む）の先頭 8 バイトの 16 進。PCM の kura チャンクのデコーダ `kura-edit/1:{samplesId}`。
    pub samples_id: String,
    /// 前回の表示から変わった区間（出力のフレーム。この区間の PCM・波形を捨てる）。
    pub changed: EditSpansChangeDto,
    pub meta_changed: bool,
    /// 公開した表示の値の有効な区間（PCM を取れる所）。
    pub valid: Vec<EditSpanDto>,
    /// 波形の段の状態。
    pub peaks: EditPeaksInfoDto,
    /// 出力のフレームを元のフレームへ写す区分（写せなければ null：聞き比べを出さない）。
    pub source_map: Option<Vec<EditSourcePieceDto>>,
    /// 出力につながるノード。
    pub nodes: Vec<EditAudioNodeDto>,
    /// ラウドネスの状態（覚えているもの。`edit_measure` で頼む）。
    pub loudness: EditLoudnessStateDto,
    /// 裏の仕事が残っている（手が止まったら `work: true` で頼み直す）。
    pub work: bool,
    /// この `work: true` の頼みでは、誤り・予算の断りのために裏の仕事が 1 チャンクも進まなかった（中止は含めない）。連鎖をやめて
    /// 待ってから頼み直す。
    pub work_stalled: bool,
    pub warnings: Vec<Issue>,
    pub drawn_nodes: u64,
    /// 描くのにかかった時間（ミリ秒）。
    pub ms: f64,
    /// 流し解きの進み（0〜1。M17 段 3）。null でなければ、頭の文書の流し解きを裏で進めていて、この表示（`rev`）は最後に
    /// 公開した rev の計画で描いた頭より古いもの。フロントは古い表示として当て（手元の値・枠を戻さない）、`work: true` を
    /// 頼み続ける（ドラッグ中も手が止まったら）。解き終えると新しい rev の表示になる。
    pub solving: Option<f64>,
    /// 頼んだスペクトルのポートの状態（M17 段 4。`/edit/{session}/spec/{key}/{channel}/{chunk}` でタイルを取る）。
    pub spec: Vec<EditSpecPortDto>,
    /// 最後に頼んだスペクトルの区間の平均（向きごとに 1 つ）の状態（値は載せない。`ready` になったら `edit_spec_average` を
    /// 頼み直すと覚えた値が返る。`idle` は出力が変わって予約を捨てたので、頼み直す）。
    pub spec_average: Vec<EditSpecAverageStatusDto>,
    /// 鳴らす音の指紋（M17 段 6）。試聴の重ねがなければ `samplesId`。違えば、再生は
    /// `/edit/{session}/audition/{auditionId}/pcm/{a}-{b}`（デコーダ `kura-edit/1:{auditionId}`）を `audition.valid` の区間で取る。
    pub audition_id: String,
    /// 試聴の情報（頼みに試聴があるときだけ）。
    pub audition: Option<EditAuditionDto>,
    /// 元のラウドネス（M17 段 6。`edit_measure` の `input` で頼む）。
    pub input_loudness: EditLoudnessStateDto,
}

/// スペクトルの区間の平均の頼み（M17 段 4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditSpecAverageRequestDto {
    /// ノード（null は文書の出力で、向きは `output`。ノードは出力の時間の写しの連鎖にあるものだけ）。
    pub node: Option<u32>,
    pub side: EditSpecSideDto,
    pub channel: EditSpecChannelDto,
    /// 出力のフレームの区間（中心がこの中に入るフレームを数える）。
    pub start: u64,
    pub end: u64,
}

impl From<EditSpecAverageRequestDto>
    for kura_core::edit::editor::audio::spec_average::SpecAverageRequest
{
    fn from(r: EditSpecAverageRequestDto) -> Self {
        Self {
            node: r.node.map(NodeId),
            side: r.side.into(),
            channel: r.channel.into(),
            start: r.start,
            end: r.end,
        }
    }
}

impl From<kura_core::edit::editor::audio::spec_average::SpecAverageRequest>
    for EditSpecAverageRequestDto
{
    fn from(r: kura_core::edit::editor::audio::spec_average::SpecAverageRequest) -> Self {
        Self {
            node: r.node.map(|n| n.0),
            side: r.side.into(),
            channel: r.channel.into(),
            start: r.start,
            end: r.end,
        }
    }
}

/// スペクトルの区間の平均の状態（M17 段 4）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditSpecAverageDto {
    /// 予約した（裏の仕事で求める。フロントは `work: true` を頼み続ける）。
    Measuring {
        progress: f64,
    },
    /// 帯ごとの平均（パワーの領域）と最大（dB。−120 で止める）。`frames` が 0 なら空。
    Ready {
        frames: u64,
        mean: Vec<f32>,
        max: Vec<f32>,
    },
    Failed {
        message: String,
    },
}

impl From<kura_core::edit::editor::audio::spec_average::SpecAverageState> for EditSpecAverageDto {
    fn from(s: kura_core::edit::editor::audio::spec_average::SpecAverageState) -> Self {
        use kura_core::edit::editor::audio::spec_average::SpecAverageState;
        match s {
            SpecAverageState::Measuring { progress } => Self::Measuring { progress },
            SpecAverageState::Ready(v) => Self::Ready {
                frames: v.frames,
                mean: v.mean,
                max: v.max,
            },
            SpecAverageState::Failed { message } => Self::Failed { message },
        }
    }
}

/// 表示の情報に載せるスペクトルの区間の平均の状態（値は載せない）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditSpecAveragePhaseDto {
    Idle,
    Measuring { progress: f64 },
    Ready,
    Failed { message: String },
}

/// 最後に頼んだ区間の状態。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditSpecAverageStatusDto {
    pub request: EditSpecAverageRequestDto,
    /// 求めているポートの鍵（スペクトルのポートの `key` と同じ）。
    pub key: String,
    pub phase: EditSpecAveragePhaseDto,
}

impl From<kura_core::edit::editor::audio::spec_average::SpecAverageStatus>
    for EditSpecAverageStatusDto
{
    fn from(s: kura_core::edit::editor::audio::spec_average::SpecAverageStatus) -> Self {
        use kura_core::edit::editor::audio::spec_average::SpecAveragePhase;
        Self {
            request: s.request.into(),
            key: s.key,
            phase: match s.phase {
                SpecAveragePhase::Idle => EditSpecAveragePhaseDto::Idle,
                SpecAveragePhase::Measuring { progress } => {
                    EditSpecAveragePhaseDto::Measuring { progress }
                }
                SpecAveragePhase::Ready => EditSpecAveragePhaseDto::Ready,
                SpecAveragePhase::Failed { message } => EditSpecAveragePhaseDto::Failed { message },
            },
        }
    }
}

/// スペクトルのポート 1 つの状態（M17 段 4）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditSpecPortDto {
    pub node: Option<u32>,
    pub side: EditSpecSideDto,
    pub channel: EditSpecChannelDto,
    /// ポートの鍵（32 字の 16 進。URL に入れる）。
    pub key: String,
    /// ポートのレート・長さ（フレーム）。
    pub rate: u32,
    pub frames: u64,
    /// 式 `kura-spec/1` の窓の大きさ・hop・帯の数。
    pub n: u32,
    pub hop: u32,
    pub bands: u32,
    /// 出力のフレーム → ポートのフレームの写しの区分（`in` がポート）。
    pub map: Vec<EditSourcePieceDto>,
    /// 1 回の頼みで受け付けるチャンクの数（置き場に収まる数。超えた分は頼みの順で切り、`ready`・`pending` に入らない）。
    pub max_chunks: u64,
    /// 置き場にあるチャンクの番号の区間（半開）。
    pub ready: Vec<EditSpanDto>,
    /// まだ作っていないチャンクがある（フロントは `work: true` を頼む）。
    pub pending: bool,
}

impl From<kura_core::edit::editor::audio::spec::SpecPortInfo> for EditSpecPortDto {
    fn from(p: kura_core::edit::editor::audio::spec::SpecPortInfo) -> Self {
        Self {
            node: p.node.map(|n| n.0),
            side: p.side.into(),
            channel: p.channel.into(),
            key: p.key,
            rate: p.rate,
            frames: p.frames,
            n: p.n,
            hop: p.hop,
            bands: p.bands,
            map: p.map.into_iter().map(Into::into).collect(),
            max_chunks: p.max_chunks,
            ready: p.ready.into_iter().map(span).collect(),
            pending: p.pending,
        }
    }
}

impl From<AudioViewInfo> for EditAudioViewDto {
    fn from(v: AudioViewInfo) -> Self {
        Self {
            rev: v.rev,
            rate: v.rate,
            channels: v.channels,
            frames: v.frames,
            loop_range: v.loop_range.map(span),
            markers: v
                .markers
                .into_iter()
                .map(|(pos, name)| EditMarkerDto { pos, name })
                .collect(),
            samples_id: v.samples_id,
            changed: v.changed.into(),
            meta_changed: v.meta_changed,
            valid: v.valid.into_iter().map(span).collect(),
            peaks: v.peaks.into(),
            source_map: v
                .source_map
                .map(|m| m.into_iter().map(Into::into).collect()),
            nodes: v.nodes.into_iter().map(Into::into).collect(),
            loudness: v.loudness.into(),
            work: v.work,
            work_stalled: v.work_stalled,
            warnings: v.warnings,
            drawn_nodes: v.drawn_nodes,
            ms: v.ms,
            solving: v.solving,
            spec: v.spec.into_iter().map(Into::into).collect(),
            spec_average: v.spec_average.into_iter().map(Into::into).collect(),
            audition_id: v.audition_id,
            audition: v.audition.map(Into::into),
            input_loudness: v.input_loudness.into(),
        }
    }
}

/// 画像の表示した結果。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditImageViewDto {
    pub rev: u64,
    /// 描いた段（0 は等倍。段 k は各軸 ⌈n/2^k⌉）。タイルの URL の `{level}`。
    pub level: u8,
    /// 求めた段から段 0 に落ちた理由。
    pub fallback: Option<String>,
    /// 等倍の出力の大きさ。
    pub full_width: u64,
    pub full_height: u64,
    /// 段のキャンバスの大きさ。
    pub width: u64,
    pub height: u64,
    pub frames: Vec<EditFrameDto>,
    /// 画素の形式（`rgba8` など）。タイルは形式によらず RGBA8。
    pub format: Option<String>,
    /// タイルの 1 辺（256）。
    pub tile: u32,
    pub changed: EditChangedDto,
    pub meta_changed: bool,
    pub warnings: Vec<Issue>,
    pub drawn_nodes: u64,
    /// 描くのにかかった時間（ミリ秒）。
    pub ms: f64,
}

impl From<ImageViewInfo> for EditImageViewDto {
    fn from(v: ImageViewInfo) -> Self {
        Self {
            rev: v.rev,
            level: v.level,
            fallback: v.fallback,
            full_width: v.full.0,
            full_height: v.full.1,
            width: v.canvas.0,
            height: v.canvas.1,
            frames: v.frames.into_iter().map(Into::into).collect(),
            format: v.format,
            tile: v.tile,
            changed: v.changed.into(),
            meta_changed: v.meta_changed,
            warnings: v.warnings,
            drawn_nodes: v.drawn_nodes,
            ms: v.ms,
        }
    }
}

/// 断り・失敗（指摘はノード・引数つき）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct EditProblemDto {
    pub code: AppErrorCode,
    pub message: String,
    pub issues: Vec<Issue>,
}

fn problem_code(c: ProblemCode) -> AppErrorCode {
    match c {
        ProblemCode::Invalid => AppErrorCode::Invalid,
        ProblemCode::TooLarge => AppErrorCode::TooLarge,
        ProblemCode::Busy => AppErrorCode::Busy,
        ProblemCode::ToolMissing => AppErrorCode::ToolMissing,
        ProblemCode::Conflict => AppErrorCode::Conflict,
        ProblemCode::Failed => AppErrorCode::Internal,
    }
}

impl From<Problem> for EditProblemDto {
    fn from(p: Problem) -> Self {
        Self {
            code: problem_code(p.code),
            message: p.message,
            issues: p.issues,
        }
    }
}

/// 表示の結果。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum EditViewOutcomeDto {
    Shown {
        view: EditViewDto,
    },
    /// 新しい要求に取って代わられた（当てた手は残る。新しい要求の結果を待つ）。
    Superseded,
    /// 描けない（手は当たっている。取り消せる）。
    Failed {
        problem: EditProblemDto,
    },
}

impl From<ViewOutcome> for EditViewOutcomeDto {
    fn from(v: ViewOutcome) -> Self {
        match v {
            ViewOutcome::Shown(view) => Self::Shown { view: view.into() },
            ViewOutcome::Superseded => Self::Superseded,
            ViewOutcome::Failed(p) => Self::Failed { problem: p.into() },
        }
    }
}

/// 断った手（それより前の手は当たり、後の手は当てていない）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct EditRejectedDto {
    pub index: u32,
    pub problem: EditProblemDto,
}

impl From<Rejected> for EditRejectedDto {
    fn from(r: Rejected) -> Self {
        Self {
            index: u32::try_from(r.index).unwrap_or(u32::MAX),
            problem: r.problem.into(),
        }
    }
}

/// 手（と表示）の結果。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct EditStepResultDto {
    pub status: EditStatusDto,
    /// 足したノード。
    pub created: Vec<u32>,
    pub rejected: Option<EditRejectedDto>,
    pub doc: EditDocDeltaDto,
    pub view: EditViewOutcomeDto,
    /// 知らせ（保存が終わった・失敗した、など）。
    pub notices: Vec<String>,
}

impl From<StepOutcome> for EditStepResultDto {
    fn from(o: StepOutcome) -> Self {
        Self {
            status: o.status.into(),
            created: o.created.into_iter().map(|n| n.0).collect(),
            rejected: o.rejected.map(Into::into),
            doc: o.doc.into(),
            view: o.view.into(),
            notices: o.notices,
        }
    }
}

/// 下書きの有無。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditDraftDto {
    pub exists: bool,
    /// このまま保存できる（偽なら、保存すると Conflict：置いたあとに別の保存でアイテムが変わった）。
    pub fresh: bool,
    pub updated_at: Option<i64>,
    /// 下書きから開いた。
    pub opened: bool,
}

impl From<DraftInfo> for EditDraftDto {
    fn from(d: DraftInfo) -> Self {
        Self {
            exists: d.exists,
            fresh: d.fresh,
            updated_at: d.updated_at,
            opened: d.opened,
        }
    }
}

/// 音声の元の形。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct EditAudioSourceDto {
    pub rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// 元のファイルの標本のビット数（分かれば）と浮動小数か。
    pub bits: Option<u16>,
    pub float: bool,
    /// 元のループ点。
    #[serde(rename = "loop")]
    pub loop_range: Option<EditSpanDto>,
    /// 値を読んだデコーダの識別子。
    pub decoder: String,
    /// 遅延の源で開いた（全体を読んでいない）。
    pub lazy: bool,
}

impl From<AudioSourceInfo> for EditAudioSourceDto {
    fn from(a: AudioSourceInfo) -> Self {
        Self {
            rate: a.rate,
            channels: a.channels,
            frames: a.frames,
            bits: a.bits,
            float: a.float,
            loop_range: a.loop_range.map(span),
            decoder: a.decoder,
            lazy: a.lazy,
        }
    }
}

/// 元の形。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
pub struct EditSourceDto {
    /// `image`・`frames`・`audio`。
    pub kind: String,
    pub width: u64,
    pub height: u64,
    pub frames: Option<u64>,
    /// 画素の形式（`rgba8`・`indexed8` など）。ファイルの形式ではない（元のファイルの形式は `EditOpenedDto.originalFormat`）。
    pub format: Option<String>,
    /// 音声の元（音声だけ）。
    pub audio: Option<EditAudioSourceDto>,
}

impl From<SourceInfo> for EditSourceDto {
    fn from(s: SourceInfo) -> Self {
        Self {
            kind: s.kind.into(),
            width: s.width,
            height: s.height,
            frames: s.frames,
            format: s.format,
            audio: s.audio.map(Into::into),
        }
    }
}

/// 開いた結果。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditOpenedDto {
    /// セッション（kura:// の `/edit/{session}/…` と、ほかのコマンドに渡す）。
    pub session: String,
    pub item_id: i64,
    pub base_version: i64,
    pub source: EditSourceDto,
    /// 元のファイルの形式の ID（`png`・`mp3` など。出力の形式の「元と同じ」が書く形式で、アイテムの今の拡張子とは違うことが
    /// ある：HEIC の元を PNG で保存したあとなど）。
    pub original_format: String,
    /// ドット絵として表示する（段は 0、拡大は最近傍・装置の画素の整数倍）。
    pub pixel_art: bool,
    pub draft: EditDraftDto,
    /// 開いたときの状態・文書（全体）・表示。
    pub result: EditStepResultDto,
    /// レシピの見本のモードで開いた（`edit_open_recipe`）。
    pub recipe: Option<EditRecipeDto>,
}

/// レシピの見本のモードの印（M14 段 7）：見本のアイテムの元にレシピを継ぎ足して開いた。下書きは置かず、アイテムに保存しない
/// （保存は `recipe_replace` でレシピへ）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditRecipeDto {
    pub id: i64,
    pub name: String,
    pub description: String,
    /// レシピの結び付け（ノードはエディタの文書の ID に付け替えたもの）。
    pub bindings: Vec<kura_engine::recipe::Binding>,
    /// レシピが出力の形式を決めている（「元と同じ」でない）。
    pub with_format: bool,
}

impl From<editor::RecipeSample> for EditRecipeDto {
    fn from(r: editor::RecipeSample) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.description,
            bindings: r.bindings,
            with_format: r.with_format,
        }
    }
}

impl From<Opened> for EditOpenedDto {
    fn from(o: Opened) -> Self {
        Self {
            session: o.session,
            item_id: o.item_id,
            base_version: o.base_version,
            source: o.source.into(),
            original_format: o.original_format,
            pixel_art: o.pixel_art,
            draft: o.draft.into(),
            result: o.outcome.into(),
            recipe: o.recipe.map(Into::into),
        }
    }
}

/// スポイトで拾った 1 画素。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub struct EditSampleDto {
    /// ストレートの RGBA8。
    pub rgba: [u8; 4],
    /// パレットの番号（Indexed8）。
    pub index: Option<u8>,
}

impl From<Sampled> for EditSampleDto {
    fn from(s: Sampled) -> Self {
        Self {
            rgba: s.rgba,
            index: s.index,
        }
    }
}

/// 音声の長さ（フレームと標本化周波数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub struct EditAudioLengthDto {
    pub frames: u64,
    pub rate: u32,
}

impl From<AudioLength> for EditAudioLengthDto {
    fn from(l: AudioLength) -> Self {
        Self {
            frames: l.frames,
            rate: l.rate,
        }
    }
}

/// 保存の前（アイテムの今）と後の音声の長さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub struct EditAudioLengthsDto {
    pub before: EditAudioLengthDto,
    pub after: EditAudioLengthDto,
}

/// 保存を積んだ結果。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct EditSaveStartedDto {
    /// 保存のジョブ（進み具合はジョブのイベント）。
    pub job: i64,
    pub status: EditStatusDto,
    /// 出力の形式を変えたときの文書の差分。
    pub doc: EditDocDeltaDto,
    /// 音声の上書き・版に戻すの前後の長さ（素材の設定が新しい長さに合うかを比べる。ms では 1 ms 未満の切り詰めもレート変換も
    /// 同じになる）。画像・別名は null。文書を描けなくても null。
    pub lengths: Option<EditAudioLengthsDto>,
}

impl From<SaveStarted> for EditSaveStartedDto {
    fn from(s: SaveStarted) -> Self {
        Self {
            job: s.job,
            status: s.status.into(),
            doc: s.doc.into(),
            lengths: s.lengths.map(|l| EditAudioLengthsDto {
                before: l.before.into(),
                after: l.after.into(),
            }),
        }
    }
}

/// 版の一覧の行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditVersionDto {
    pub version: i64,
    /// `original`・`edit`・`revert`。
    pub kind: String,
    pub revert_to: Option<i64>,
    pub ext: String,
    pub format: Option<String>,
    pub size_bytes: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    /// `hold`・`waiting`・`kept`・`released`（実ファイルを持っているか）。
    pub file_state: String,
    pub created_at: i64,
}

impl From<VersionSummary> for EditVersionDto {
    fn from(v: VersionSummary) -> Self {
        Self {
            version: v.version,
            kind: v.kind.into(),
            revert_to: v.revert_to,
            ext: v.ext,
            format: v.format,
            size_bytes: v.size_bytes,
            width: v.width,
            height: v.height,
            duration_ms: v.duration_ms,
            file_state: v.file_state.into(),
            created_at: v.created_at,
        }
    }
}

impl From<EditorError> for AppError {
    fn from(err: EditorError) -> Self {
        let code = match &err {
            EditorError::NoSession | EditorError::NotFound(_) => AppErrorCode::NotFound,
            EditorError::Unavailable(_) => AppErrorCode::Internal,
            EditorError::Invalid { .. } | EditorError::Failed(_) => AppErrorCode::Invalid,
            EditorError::TooLarge(_) => AppErrorCode::TooLarge,
            EditorError::Busy(_) => AppErrorCode::Busy,
            EditorError::ToolMissing(_) => AppErrorCode::ToolMissing,
            EditorError::Conflict(_) | EditorError::Mismatch(_) | EditorError::Canceled => {
                AppErrorCode::Conflict
            }
            // 保存する形式をこの環境で書けない（M13 段 3。フロントは形式を選ぶシートを出す）
            EditorError::FormatUnavailable { .. } => AppErrorCode::FormatUnavailable,
            // 前の保存が終わっていない（フロントは「前の保存が終わっていません」のシートを出す。ADR-189）
            EditorError::SavePending { .. } => AppErrorCode::SavePending,
            EditorError::Core(_) => AppErrorCode::Internal,
        };
        match err {
            EditorError::Core(core) => core.into(),
            other => Self::new(code, other.to_string()),
        }
    }
}

/// イコライザ（`audio.eq` v1）の範囲と既定（M17 段 5。フロントのスペクトルの面・バンドの表は手で写さずにこれを読む）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditEqLimitsDto {
    /// バンドの数・2 次の段の数の上限。
    pub max_bands: u32,
    pub max_sections: u32,
    /// 周波数の範囲（Hz）と、止める上限（レートに対する比）。
    pub freq_min: f64,
    pub freq_max: f64,
    pub nyquist_guard: f64,
    /// 利得の範囲（±dB。バンドと出力）。
    pub gain_max: f64,
    /// Q の範囲とシェルフの上限・既定。
    pub q_min: f64,
    pub q_max: f64,
    pub shelf_q_max: f64,
    pub default_q: f64,
    /// カットの傾き（dB/oct）と既定。
    pub slopes: Vec<u32>,
    pub default_slope: u32,
}

/// `EQ_LIMITS` の値。
pub fn eq_limits() -> EditEqLimitsDto {
    use kura_engine::ops::audio::eq as e;
    EditEqLimitsDto {
        max_bands: e::MAX_BANDS as u32,
        max_sections: e::MAX_SECTIONS as u32,
        freq_min: e::FREQ_MIN,
        freq_max: e::FREQ_MAX,
        nyquist_guard: e::NYQUIST_GUARD,
        gain_max: e::GAIN_MAX,
        q_min: e::Q_MIN,
        q_max: e::Q_MAX,
        shelf_q_max: e::SHELF_Q_MAX,
        default_q: e::DEFAULT_Q,
        slopes: e::SLOPES.to_vec(),
        default_slope: e::DEFAULT_SLOPE,
    }
}

/// 操作のカタログ（bindings の定数 `EDIT_OPS`。今の版の操作の ID・版・入力・引数の記述（表示の条件つき））。
pub fn edit_ops() -> Vec<OpDescriptor> {
    match kura_engine::ops::registry() {
        Ok(r) => r.catalog(),
        Err(err) => {
            tracing::error!(%err, "操作の登録表を作れません");
            Vec::new()
        }
    }
}

// ===== コマンド =====

/// エディタを開く（前のエディタは閉じる）。`fromDraft` が真で下書きがあれば、下書きの文書と土台で開く。画像と音声。
#[tauri::command]
#[specta::specta]
pub async fn edit_open(
    state: State<'_, AppState>,
    item_id: i64,
    viewport: EditViewportDto,
    from_draft: Option<bool>,
) -> Result<EditOpenedDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&session)?;
        let opened = editor::open(
            &env,
            &OpenRequest {
                item_id,
                viewport: viewport.into(),
                from_draft: from_draft.unwrap_or(false),
            },
        )?;
        Ok(opened.into())
    })
    .await
}

/// レシピ `recipe` の見本のモードでアイテム `itemId` を開く（M14 段 7。見本の元にレシピを継ぎ足す。下書きは置かず・開かず、
/// アイテムに保存しない。N → 1 のレシピ・当てられないレシピは Invalid）。
#[tauri::command]
#[specta::specta]
pub async fn edit_open_recipe(
    state: State<'_, AppState>,
    item_id: i64,
    viewport: EditViewportDto,
    recipe: i64,
) -> Result<EditOpenedDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&session)?;
        let opened = editor::open_recipe(
            &env,
            &OpenRequest {
                item_id,
                viewport: viewport.into(),
                from_draft: false,
            },
            recipe,
        )?;
        Ok(opened.into())
    })
    .await
}

/// 手をまとめて当てて表示する（描くのは最後に 1 回）。
#[tauri::command]
#[specta::specta]
pub async fn edit_step(
    state: State<'_, AppState>,
    session: String,
    steps: Vec<EditStepDto>,
    viewport: EditViewportDto,
) -> Result<EditStepResultDto, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        let steps = steps.into_iter().map(Into::into).collect();
        Ok(editor::step(&env, &session, steps, &viewport.into())?.into())
    })
    .await
}

/// 表示だけ（パン・ズーム・段の変更。保存が終わったあとの状態もこれで取り直す）。
#[tauri::command]
#[specta::specta]
pub async fn edit_view(
    state: State<'_, AppState>,
    session: String,
    viewport: EditViewportDto,
) -> Result<EditStepResultDto, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        Ok(editor::view(&env, &session, &viewport.into())?.into())
    })
    .await
}

/// スポイト：今の rev の等倍の値の 1 画素（キャンバスの座標）。画像の外は null。
#[tauri::command]
#[specta::specta]
pub async fn edit_sample(
    state: State<'_, AppState>,
    session: String,
    frame: u32,
    x: i64,
    y: i64,
) -> Result<Option<EditSampleDto>, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        Ok(editor::sample(&env, &session, frame, x, y)?.map(Into::into))
    })
    .await
}

/// 音声のエディタ：今の rev の出力（量子化のあと）のラウドネス。出力の指紋ごとに覚え、短い音声はその場で計って `ready` を
/// 返す（長い音声は計りを予約して `measuring` を返し、`edit_view { work: true }` の裏の仕事で計る）。描いている途中の表示を止めない。
/// `target` で元・外して聞く音も計れる（M17 段 6。省けば今の出力）。
#[tauri::command]
#[specta::specta]
pub async fn edit_measure(
    state: State<'_, AppState>,
    session: String,
    target: Option<EditMeasureTargetDto>,
) -> Result<EditLoudnessStateDto, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        let target = target.map_or(
            kura_core::edit::editor::audio::measure::MeasureTarget::Output,
            Into::into,
        );
        Ok(editor::audio::measure::measure_target(&env, &session, target)?.into())
    })
    .await
}

/// 音声のエディタ：今の rev の出力の区間のスペクトルの帯ごとの平均と最大（M17 段 4。`kura-spec/1`）。求めるチャンクが少なければ
/// その場で `ready` を返し、多ければ予約して `measuring` を返す（`edit_view { work: true }` の裏の仕事で求める。表示の情報の
/// `specAverage` が `ready` になったら頼み直す）。描いている途中の表示を止めない。
#[tauri::command]
#[specta::specta]
pub async fn edit_spec_average(
    state: State<'_, AppState>,
    session: String,
    request: EditSpecAverageRequestDto,
) -> Result<EditSpecAverageDto, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        Ok(editor::audio::spec_average::spec_average(&env, &session, &request.into())?.into())
    })
    .await
}

/// ゼロ交差に寄せた位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
pub struct EditSnapDto {
    /// 寄せた位置（見つからなければ頼んだ位置）。
    pub frame: u64,
    pub found: bool,
}

/// 音声のエディタ：今の rev の出力で、`frame` に最も近いゼロ交差（半径 `radius` フレーム。4,096 で切る）。描いている途中の表示を
/// 止めない（表示の外なら区間だけを描いて読む）。
#[tauri::command]
#[specta::specta]
pub async fn edit_snap(
    state: State<'_, AppState>,
    session: String,
    frame: u64,
    radius: u32,
) -> Result<EditSnapDto, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        let s = editor::audio::snap::snap(&env, &session, frame, radius)?;
        Ok(EditSnapDto {
            frame: s.frame,
            found: s.found,
        })
    })
    .await
}

/// イコライザの曲線のバンド 1 つ（M17 段 2）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditEqBandCurveDto {
    /// 引数のバンドの番号（外した・段のないバンドは出さない）。
    pub index: u32,
    /// `freqs` の点ごとの利得（dB）。
    pub db: Vec<f64>,
    /// 段の係数 `[b0, b1, b2, a1, a2]`。
    pub sections: Vec<Vec<f64>>,
}

/// イコライザの曲線（M17 段 2。`edit_eq_curve`）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditEqCurveDto {
    /// 周波数（Hz。20 Hz〜ナイキストの対数の格子）。
    pub freqs: Vec<f64>,
    /// 合成（全バンドの和 ＋ 出力の利得。dB）。
    pub total: Vec<f64>,
    pub bands: Vec<EditEqBandCurveDto>,
}

/// イコライザの曲線の頼み（M17 段 2）。
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditEqCurveRequestDto {
    /// `audio.eq` の引数（ドラッグの仮の値でよい）。
    #[specta(type = JsonValue)]
    pub params: serde_json::Value,
    /// そのノードの入力のレート（表示の `nodes[].inRate`）。
    pub rate: u32,
    pub channels: u16,
    /// 点の数（2〜2,048）。
    pub points: u32,
}

/// イコライザの曲線（M17 段 2。docs/editing.md §18.5）：`audio.eq` の引数とレート・チャンネルの数で、`points` 点（2〜2,048）の
/// 対数の格子のバンドごと・合成の利得と係数を返す。描かず、セッションも描画の錠も待たない（ドラッグの間に点・曲線を毎回の
/// 描画で追う）。引数が読めない・範囲の外・推論で断るなら `Invalid`。
#[tauri::command]
#[specta::specta]
pub async fn edit_eq_curve(request: EditEqCurveRequestDto) -> Result<EditEqCurveDto, AppError> {
    blocking(move || {
        let EditEqCurveRequestDto {
            params,
            rate,
            channels,
            points,
        } = request;
        let c = editor::audio::eq_curve::eq_curve(&params, rate, channels, points)?;
        Ok(EditEqCurveDto {
            freqs: c.freqs,
            total: c.total,
            bands: c
                .bands
                .into_iter()
                .map(|b| EditEqBandCurveDto {
                    index: u32::try_from(b.index).unwrap_or(u32::MAX),
                    db: b.db,
                    sections: b.sections.iter().map(|s| s.to_vec()).collect(),
                })
                .collect(),
        })
    })
    .await
}

/// 書ける形式 1 つ（形式の表の行。M13 段 4C）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditWritableFormatDto {
    /// 形式の ID（`edit_save` の `format: { type: "to", format }` に渡す）。
    pub format: String,
    /// 代表の拡張子。
    pub ext: String,
    /// 今の環境（今のコーデックの組）で、今の出力の形を書けるか（MP3 は ffmpeg がなければ偽）。
    pub available: bool,
    /// 書けない理由が道具のないことなら、その道具（`"ffmpeg"`：外部の道具に頼る形式で、書き手が今の組にない）。書き手は
    /// あるが今の出力の形（レート・チャンネル数・大きさ）が合わない・書けるときは null。
    pub missing_tool: Option<String>,
    /// 形式の表の保存できるもの：可逆・ループ点・マーカー・16 ビットより細かい標本。
    pub lossless: bool,
    pub loop_points: bool,
    pub markers: bool,
    pub high_bit_depth: bool,
}

/// 書ける形式の一覧（形式の表の順で、アイテムの種類の行すべて）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditFormatsDto {
    pub formats: Vec<EditWritableFormatDto>,
    /// 今の出力の形式（Rust が解いた形式：「元と同じ」は元の形式（アイテムの今の拡張子ではない）、ほかはその形式）。保存が
    /// `FormatUnavailable` で断られた直後なら、断った形式。
    pub current: String,
    /// 今の出力の形式に書けないときに保存が使う形式（音声は Ogg Vorbis、書けなければ WAV。画像は PNG）。書ければ null。
    pub fallback: Option<String>,
}

impl From<EditFormats> for EditFormatsDto {
    fn from(f: EditFormats) -> Self {
        Self {
            formats: f
                .formats
                .into_iter()
                .map(|w| EditWritableFormatDto {
                    format: w.format,
                    ext: w.ext,
                    available: w.available,
                    missing_tool: w.missing_tool.map(str::to_string),
                    lossless: w.caps.lossless,
                    loop_points: w.caps.loop_points,
                    markers: w.caps.markers,
                    high_bit_depth: w.caps.high_bit_depth,
                })
                .collect(),
            current: f.current,
            fallback: f.fallback,
        }
    }
}

/// 書ける形式の一覧（確定した頭の出力の形で、保存のジョブと同じ今のコーデックの組で確かめる）。保存が `FormatUnavailable` で
/// 断られたときに、フロントが形式を選ぶシートに使う。
#[tauri::command]
#[specta::specta]
pub async fn edit_formats(
    state: State<'_, AppState>,
    session: String,
) -> Result<EditFormatsDto, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        Ok(editor::formats::formats(&env, &session)?.into())
    })
    .await
}

/// エディタを閉じる（キャッシュ・予算の取り置きを外す。待っている下書きは置く）。今のセッションでなければ何もしない。
#[tauri::command]
#[specta::specta]
pub async fn edit_close(state: State<'_, AppState>, session: String) -> Result<(), AppError> {
    let Some(lib) = state.current() else {
        return Ok(());
    };
    blocking(move || {
        if let Some(engine) = lib.engine() {
            editor::close(engine, &session);
        }
        Ok(())
    })
    .await
}

/// 開く途中のエディタを止める（`edit_open` が返る前に閉じたとき。セッションはまだ分からないのでアイテムで指す）。そのアイテムを
/// 開いている途中なら止め、止められた `edit_open` は `Conflict`（中止）で返る（作業用のファイルの書きかけは消える）。開き終えた
/// エディタ・別のアイテム・ライブラリがないときは何もしない。
#[tauri::command]
#[specta::specta]
pub async fn edit_cancel_open(state: State<'_, AppState>, item_id: i64) -> Result<(), AppError> {
    let Some(lib) = state.current() else {
        return Ok(());
    };
    blocking(move || {
        if let Some(engine) = lib.engine() {
            editor::cancel_open(engine, item_id);
        }
        Ok(())
    })
    .await
}

/// 保存のジョブを積む（確定した頭を保存する。`format` を渡すと先に出力の形式を変える手を確定する）。保存が待っている間は
/// Busy。
#[tauri::command]
#[specta::specta]
pub async fn edit_save(
    state: State<'_, AppState>,
    session: String,
    mode: EditSaveModeDto,
    format: Option<FormatChoice>,
) -> Result<EditSaveStartedDto, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        Ok(editor::save(&env, &session, mode.into(), format)?.into())
    })
    .await
}

/// 終わっていない保存のジョブの向き。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "type"
)]
pub enum EditPendingSaveModeDto {
    Overwrite,
    /// 別名で保存（新しいアイテムの表示名）。
    SaveAs {
        title: String,
    },
    Revert,
}

/// 終わっていない保存のジョブ 1 件。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct EditPendingSaveDto {
    pub job: i64,
    pub item_id: i64,
    /// 保存しているアイテムの表示名（別名で保存では元にしたアイテム。行がなければ null）。
    pub item_title: Option<String>,
    pub mode: EditPendingSaveModeDto,
    pub state: JobStateDto,
    pub progress: Option<f64>,
    pub message: Option<String>,
}

impl From<PendingSave> for EditPendingSaveDto {
    fn from(p: PendingSave) -> Self {
        Self {
            job: p.job,
            item_id: p.item_id,
            item_title: p.item_title,
            mode: match p.mode {
                PendingSaveMode::Overwrite => EditPendingSaveModeDto::Overwrite,
                PendingSaveMode::SaveAs { title } => EditPendingSaveModeDto::SaveAs { title },
                PendingSaveMode::Revert => EditPendingSaveModeDto::Revert,
            },
            state: p.state.into(),
            progress: p.progress,
            message: p.message,
        }
    }
}

/// アイテムの版の一覧（版の順。まだ保存していなければ空）。
#[tauri::command]
#[specta::specta]
pub async fn edit_versions(
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<Vec<EditVersionDto>, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let list = lib
            .library()
            .db()
            .read(|c| editor::version_list(c, item_id))?;
        Ok(list.into_iter().map(Into::into).collect())
    })
    .await
}

/// 終わっていない保存のジョブ（古い順。エディタを閉じたあとのカプセル・開き直したエディタの保存のボタン。ADR-189）。
#[tauri::command]
#[specta::specta]
pub async fn edit_pending_saves(
    state: State<'_, AppState>,
) -> Result<Vec<EditPendingSaveDto>, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let list = lib.library().db().read(pending_saves)?;
        Ok(list.into_iter().map(Into::into).collect())
    })
    .await
}

/// アイテムの下書きを捨てる（あったら真）。
#[tauri::command]
#[specta::specta]
pub async fn edit_draft_discard(
    state: State<'_, AppState>,
    item_id: i64,
) -> Result<bool, AppError> {
    let lib = state.session()?;
    blocking(move || {
        let env = EditorEnv::of(&lib)?;
        Ok(editor::discard_draft(&env, item_id)?)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use kura_core::edit::editor::audio::spec::{SpecPortInfo, SpecRequest, SpecSide};
    use kura_core::edit::editor::audio::spec_average::{
        SpecAveragePhase, SpecAverageRequest, SpecAverageState, SpecAverageStatus,
        SpecAverageValues,
    };
    use kura_engine::audio::spec::SpecChannel;
    use serde_json::json;

    /// フロントが送る手の JSON を読める（手は kura-engine の `Edit`。種は文字列）。知らない種類・項目は断る。
    #[test]
    fn steps_are_read_from_the_front_json() {
        let steps: Vec<EditStepDto> = serde_json::from_value(json!([
            { "type": "commit", "mode": "step",
              "edit": { "type": "addNode", "at": 0, "op": "image.flip", "v": 1,
                        "params": { "axis": "vertical" }, "seed": null, "extraInputs": [] } },
            { "type": "preview", "edit": { "type": "setParam", "node": 1, "key": "axis", "value": "horizontal" } },
            { "type": "commitPreview", "mode": "merge" },
            { "type": "cancelPreview" },
            { "type": "undo" },
            { "type": "redo" },
            { "type": "seal" },
            { "type": "paint", "actions": [{ "type": "fill", "at": { "x": 1, "y": 2 },
                "color": { "rgba": [1, 2, 3, 255] }, "tolerance": 0, "connectivity": "four",
                "contiguous": true, "frame": 0 }] },
            { "type": "handle", "handle": { "type": "trim", "start": 5, "end": 900 }, "node": 3,
              "mode": "step" },
            { "type": "handle", "handle": { "type": "loop", "range": null }, "node": null,
              "mode": "preview" },
            { "type": "handle", "handle": { "type": "fadeOut", "start": 70 }, "mode": "merge" },
        ]))
        .unwrap();
        assert_eq!(steps.len(), 11);
        let steps: Vec<Step> = steps.into_iter().map(Into::into).collect();
        assert!(matches!(
            &steps[0],
            Step::Commit {
                edit: Edit::AddNode { seed: None, .. },
                mode: CommitMode::Step
            }
        ));
        assert!(matches!(
            &steps[2],
            Step::CommitPreview {
                mode: CommitMode::Merge
            }
        ));
        assert!(matches!(&steps[7], Step::Paint { actions } if actions.len() == 1));
        assert_eq!(
            steps[8],
            Step::Handle {
                handle: Handle::Trim { start: 5, end: 900 },
                node: Some(NodeId(3)),
                mode: HandleMode::Commit(CommitMode::Step)
            }
        );
        assert_eq!(
            steps[9],
            Step::Handle {
                handle: Handle::Loop { range: None },
                node: None,
                mode: HandleMode::Preview
            }
        );
        assert_eq!(
            steps[10],
            Step::Handle {
                handle: Handle::FadeOut { start: 70 },
                node: None,
                mode: HandleMode::Commit(CommitMode::Merge)
            }
        );
        for bad in [
            json!({ "type": "jump" }),
            json!({ "type": "commit", "edit": { "type": "removeNode", "node": 1 } }),
            json!({ "type": "commit", "mode": "step", "edit": { "type": "removeNode", "node": 1, "x": 0 } }),
            json!({ "type": "paint", "actions": [], "extra": 0 }),
            json!({ "type": "handle", "handle": { "type": "trim", "start": 0 }, "mode": "step" }),
            json!({ "type": "handle", "handle": { "type": "fadeIn", "end": 1, "x": 0 }, "mode": "step" }),
            json!({ "type": "handle", "handle": { "type": "loop", "range": null }, "mode": "commit" }),
        ] {
            assert!(
                serde_json::from_value::<EditStepDto>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let vp: EditViewportDto = serde_json::from_value(json!({
            "type": "image", "frame": 0, "rect": { "x": -4, "y": 8, "w": 100, "h": 50 }, "scale": 0.5,
            "interacting": true, "focus": 3
        }))
        .unwrap();
        let Viewport::Image(vp) = Viewport::from(vp) else {
            panic!("画像のビューポートのはず");
        };
        assert_eq!(vp.rect, Some(IRect::new(-4, 8, 100, 50)));
        assert_eq!(vp.focus, Some(NodeId(3)));
        let vp: EditViewportDto = serde_json::from_value(json!({
            "type": "audio", "visible": { "start": 0, "end": 480000 },
            "spans": [{ "start": 65536, "end": 131072 }], "interacting": false, "focus": null, "work": true
        }))
        .unwrap();
        assert_eq!(
            Viewport::from(vp),
            Viewport::Audio(AudioViewport {
                visible: Some((0, 480_000)),
                spans: vec![(65_536, 131_072)],
                interacting: false,
                focus: None,
                work: true,
                spec: Vec::new(),
                audition: None,
            })
        );
        // 試聴の頼み（M17 段 6）：外すノードと区間。知らない項目は拒む
        let vp: EditViewportDto = serde_json::from_value(json!({
            "type": "audio", "visible": null, "spans": [], "interacting": false, "focus": null, "work": false,
            "audition": { "bypass": 4, "spans": [{ "start": 0, "end": 65536 }] }
        }))
        .unwrap();
        let Viewport::Audio(a) = Viewport::from(vp) else {
            panic!("音声の頼みではありません");
        };
        assert_eq!(
            a.audition,
            Some(kura_core::edit::editor::audio::audition::AuditionRequest {
                bypass: NodeId(4),
                spans: vec![(0, 65_536)],
            })
        );
        assert!(
            serde_json::from_value::<EditViewportDto>(json!({
                "type": "audio", "visible": null, "spans": [], "interacting": false, "focus": null, "work": false,
                "audition": { "bypass": 4, "spans": [], "solo": 1 }
            }))
            .is_err()
        );
        // スペクトルの頼み（M17 段 4）
        let vp: EditViewportDto = serde_json::from_value(json!({
            "type": "audio", "visible": null, "spans": [], "interacting": false, "focus": null, "work": false,
            "spec": [
                { "node": null, "side": "output", "channel": { "type": "sum" }, "spans": [{ "start": 5, "end": 9 }] },
                { "node": 3, "side": "input", "channel": { "type": "index", "index": 1 }, "spans": [] }
            ]
        }))
        .unwrap();
        let Viewport::Audio(a) = Viewport::from(vp) else {
            panic!("音声の頼みではありません");
        };
        assert_eq!(
            a.spec,
            vec![
                SpecRequest {
                    node: None,
                    side: SpecSide::Output,
                    channel: SpecChannel::Sum,
                    spans: vec![(5, 9)],
                },
                SpecRequest {
                    node: Some(NodeId(3)),
                    side: SpecSide::Input,
                    channel: SpecChannel::Index(1),
                    spans: Vec::new(),
                },
            ]
        );
        for bad in [
            json!({ "node": null, "side": "output", "channel": { "type": "index" }, "spans": [] }),
            json!({ "node": null, "side": "after", "channel": { "type": "sum" }, "spans": [] }),
            json!({ "node": null, "side": "output", "channel": { "type": "sum", "x": 1 }, "spans": [] }),
            json!({ "node": null, "side": "output", "channel": { "type": "sum" }, "spans": [], "x": 1 }),
        ] {
            assert!(
                serde_json::from_value::<EditSpecRequestDto>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let avg: EditSpecAverageRequestDto = serde_json::from_value(json!({
            "node": null, "side": "output", "channel": { "type": "mid" }, "start": 10, "end": 20
        }))
        .unwrap();
        assert_eq!(
            SpecAverageRequest::from(avg),
            SpecAverageRequest {
                node: None,
                side: SpecSide::Output,
                channel: SpecChannel::Mid,
                start: 10,
                end: 20,
            }
        );
        assert_eq!(
            serde_json::to_value(EditSpecAverageDto::from(SpecAverageState::Ready(
                SpecAverageValues {
                    frames: 3,
                    mean: vec![-6.5],
                    max: vec![-1.0],
                }
            )))
            .unwrap(),
            json!({ "type": "ready", "frames": 3, "mean": [-6.5], "max": [-1.0] })
        );
        assert_eq!(
            serde_json::to_value(EditSpecAverageDto::from(SpecAverageState::Measuring {
                progress: 0.5
            }))
            .unwrap(),
            json!({ "type": "measuring", "progress": 0.5 })
        );
        for bad in [
            json!({ "frame": 0, "rect": null, "scale": 1.0, "interacting": false, "focus": null }),
            json!({ "type": "image", "frame": 0, "rect": null, "scale": 1.0, "interacting": false,
                    "focus": null, "extra": 1 }),
            json!({ "type": "audio", "visible": null, "spans": [{ "start": 0, "end": 1, "x": 0 }],
                    "interacting": false, "focus": null, "work": false }),
            json!({ "type": "video" }),
        ] {
            assert!(
                serde_json::from_value::<EditViewportDto>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let mode: EditSaveModeDto =
            serde_json::from_value(json!({ "type": "saveAs", "title": "写し", "stem": "copy" }))
                .unwrap();
        assert_eq!(
            SaveMode::from(mode),
            SaveMode::SaveAs {
                title: "写し".into(),
                stem: "copy".into()
            }
        );
    }

    /// 結果の JSON の形（camelCase・`type` で分ける）。
    #[test]
    fn results_serialize_for_the_front() {
        let out = StepOutcome {
            status: Status {
                rev: 3,
                head: 3,
                provisional: false,
                can_undo: true,
                can_redo: false,
                dirty: true,
                saving: None,
                needs_reopen: false,
            },
            created: vec![NodeId(2)],
            rejected: Some(Rejected {
                index: 1,
                problem: Problem {
                    code: ProblemCode::Invalid,
                    message: "x".into(),
                    issues: vec![
                        Issue::error(kura_engine::IssueCode::OutOfRange, "y").param("axis"),
                    ],
                },
            }),
            doc: DocDelta {
                base: Some(2),
                order: vec![NodeId(2)],
                nodes: Vec::new(),
                outputs: None,
            },
            view: ViewOutcome::Shown(ViewInfo::Image(ImageViewInfo {
                rev: 3,
                level: 1,
                fallback: None,
                full: (600, 400),
                canvas: (300, 200),
                frames: vec![FrameInfo {
                    index: 0,
                    rect: IRect::new(0, 0, 300, 200),
                    duration_ms: None,
                }],
                format: Some("rgba8".into()),
                tile: 256,
                changed: ChangedTiles::Tiles(vec![(0, vec![[1, 0]])]),
                meta_changed: false,
                warnings: Vec::new(),
                drawn_nodes: 2,
                ms: 1.5,
            })),
            notices: Vec::new(),
        };
        // 開いた結果は元のファイルの形式を持つ（`originalFormat`。画素の形式 `source.format` とは別）
        let opened = serde_json::to_value(EditOpenedDto::from(Opened {
            session: "aa01".into(),
            item_id: 7,
            base_version: 1,
            source: SourceInfo {
                kind: "image",
                width: 600,
                height: 400,
                frames: None,
                format: Some("rgba8".into()),
                audio: None,
            },
            original_format: "heic".into(),
            pixel_art: false,
            draft: kura_core::edit::editor::DraftInfo {
                exists: false,
                fresh: true,
                updated_at: None,
                opened: false,
            },
            outcome: out.clone(),
            recipe: None,
        }))
        .unwrap();
        assert_eq!(opened["originalFormat"], json!("heic"));
        assert_eq!(opened["source"]["format"], json!("rgba8"));
        let json = serde_json::to_value(EditStepResultDto::from(out)).unwrap();
        assert_eq!(json["status"]["canUndo"], json!(true));
        assert_eq!(json["status"]["needsReopen"], json!(false));
        assert_eq!(json["created"], json!([2]));
        assert_eq!(json["rejected"]["problem"]["code"], json!("Invalid"));
        assert_eq!(
            json["rejected"]["problem"]["issues"][0]["param"],
            json!("axis")
        );
        assert_eq!(json["view"]["type"], json!("shown"));
        assert_eq!(json["view"]["view"]["type"], json!("image"));
        assert_eq!(json["view"]["view"]["fullWidth"], json!(600));
        assert_eq!(
            json["view"]["view"]["changed"],
            json!({ "type": "tiles", "frames": [{ "frame": 0, "tiles": [[1, 0]] }] })
        );
        assert_eq!(
            serde_json::to_value(EditViewOutcomeDto::from(ViewOutcome::Superseded)).unwrap(),
            json!({ "type": "superseded" })
        );
        // 音声の表示
        let audio = EditViewOutcomeDto::from(ViewOutcome::Shown(ViewInfo::Audio(Box::new(
            AudioViewInfo {
                rev: 4,
                rate: 48_000,
                channels: 2,
                frames: 96_000,
                loop_range: Some((10, 90_000)),
                markers: vec![(5, "a".into())],
                samples_id: "0123456789abcdef".into(),
                changed: ChangedSpans::Spans(vec![(0, 65_536)]),
                meta_changed: false,
                valid: vec![(0, 96_000)],
                peaks: PeaksInfo {
                    rev: Some(3),
                    rate: 48_000,
                    channels: 2,
                    frames: 96_000,
                    bins: vec![256, 1_024],
                    tile_bins: 1_024,
                    changed: ChangedSpans::All,
                    pending: true,
                },
                source_map: Some(vec![SourcePiece {
                    out_start: 0,
                    out_end: 96_000,
                    in_start: 100,
                    in_rate: 48_000,
                    out_rate: 48_000,
                }]),
                nodes: vec![AudioNodeView {
                    id: NodeId(2),
                    op: "audio.fade".into(),
                    enabled: true,
                    in_rate: 48_000,
                    in_frames: 96_000,
                    in_channels: 2,
                    marks: Some(Marks::Fade {
                        in_start: Some(-5),
                        in_end: Some(10),
                        out_start: None,
                        out_end: None,
                    }),
                }],
                loudness: LoudnessState::Ready {
                    samples_id: "0123456789abcdef".into(),
                    values: LoudnessValues {
                        loudness: Some(-14.25),
                        loudness_reason: None,
                        loudness_range: None,
                        true_peak: Some(-1.0),
                        sample_peak: Some(-1.5),
                    },
                },
                work: true,
                work_stalled: true,
                warnings: Vec::new(),
                drawn_nodes: 1,
                ms: 2.0,
                solving: Some(0.25),
                spec: vec![SpecPortInfo {
                    node: None,
                    side: SpecSide::Output,
                    channel: SpecChannel::Side,
                    key: "00112233445566778899aabbccddeeff".into(),
                    rate: 48_000,
                    frames: 96_000,
                    n: 4096,
                    hop: 1024,
                    bands: 491,
                    map: Vec::new(),
                    max_chunks: 135,
                    ready: vec![(0, 2)],
                    pending: false,
                }],
                spec_average: vec![SpecAverageStatus {
                    request: SpecAverageRequest {
                        node: None,
                        side: SpecSide::Output,
                        channel: SpecChannel::Index(0),
                        start: 0,
                        end: 100,
                    },
                    key: "00112233445566778899aabbccddeeff".into(),
                    phase: SpecAveragePhase::Measuring { progress: 0.25 },
                }],
                audition_id: "fedcba9876543210".into(),
                audition: Some(kura_core::edit::editor::audio::audition::AuditionInfo {
                    node: NodeId(3),
                    phase: kura_core::edit::editor::audio::audition::AuditionPhase::Solving(0.5),
                    valid: vec![(0, 65_536)],
                    loudness: LoudnessState::Idle,
                }),
                input_loudness: LoudnessState::Idle,
            },
        ))));
        let json = serde_json::to_value(audio).unwrap();
        assert_eq!(json["view"]["type"], json!("audio"));
        assert_eq!(json["view"]["loop"], json!({ "start": 10, "end": 90000 }));
        assert_eq!(json["view"]["solving"], json!(0.25));
        assert_eq!(
            json["view"]["spec"],
            json!([{ "node": null, "side": "output", "channel": { "type": "side" },
                     "key": "00112233445566778899aabbccddeeff", "rate": 48000, "frames": 96000, "n": 4096,
                     "hop": 1024, "bands": 491, "map": [], "maxChunks": 135, "ready": [{ "start": 0, "end": 2 }],
                     "pending": false }])
        );
        assert_eq!(
            json["view"]["specAverage"],
            json!([{ "request": { "node": null, "side": "output", "channel": { "type": "index", "index": 0 },
                                  "start": 0, "end": 100 },
                     "key": "00112233445566778899aabbccddeeff",
                     "phase": { "type": "measuring", "progress": 0.25 } }])
        );
        assert_eq!(json["view"]["samplesId"], json!("0123456789abcdef"));
        assert_eq!(json["view"]["auditionId"], json!("fedcba9876543210"));
        assert_eq!(
            json["view"]["audition"],
            json!({ "node": 3, "phase": { "type": "solving", "progress": 0.5 },
                    "valid": [{ "start": 0, "end": 65536 }], "loudness": { "type": "idle" } })
        );
        assert_eq!(json["view"]["workStalled"], json!(true));
        assert_eq!(
            json["view"]["changed"],
            json!({ "type": "spans", "spans": [{ "start": 0, "end": 65536 }] })
        );
        assert_eq!(json["view"]["markers"], json!([{ "pos": 5, "name": "a" }]));
        assert_eq!(json["view"]["valid"], json!([{ "start": 0, "end": 96000 }]));
        assert_eq!(json["view"]["work"], json!(true));
        assert_eq!(json["view"]["peaks"]["tileBins"], json!(1024));
        assert_eq!(json["view"]["peaks"]["changed"], json!({ "type": "all" }));
        assert_eq!(
            json["view"]["sourceMap"],
            json!([{ "outStart": 0, "outEnd": 96000, "inStart": 100, "inRate": 48000, "outRate": 48000 }])
        );
        assert_eq!(
            json["view"]["nodes"][0]["marks"],
            json!({ "type": "fade", "inStart": -5, "inEnd": 10, "outStart": null, "outEnd": null })
        );
        assert_eq!(json["view"]["nodes"][0]["inFrames"], json!(96000));
        assert_eq!(json["view"]["nodes"][0]["inChannels"], json!(2));
        // イコライザの範囲と既定（EQ_LIMITS）
        assert_eq!(
            serde_json::to_value(eq_limits()).unwrap(),
            json!({ "maxBands": 24, "maxSections": 48, "freqMin": 10.0, "freqMax": 30000.0,
                    "nyquistGuard": 0.49, "gainMax": 30.0, "qMin": 0.1, "qMax": 40.0, "shelfQMax": 4.0,
                    "defaultQ": std::f64::consts::FRAC_1_SQRT_2, "slopes": [6, 12, 18, 24, 36, 48, 72, 96],
                    "defaultSlope": 12 })
        );
        assert_eq!(
            json["view"]["loudness"],
            json!({ "type": "ready", "samplesId": "0123456789abcdef", "loudness": {
                "loudness": -14.25, "loudnessReason": null, "loudnessRange": null,
                "truePeak": -1.0, "samplePeak": -1.5 } })
        );
        assert_eq!(
            serde_json::to_value(EditLoudnessStateDto::from(LoudnessState::Measuring {
                samples_id: "ab".into(),
                progress: 0.5
            }))
            .unwrap(),
            json!({ "type": "measuring", "samplesId": "ab", "progress": 0.5 })
        );
        let source = EditSourceDto::from(SourceInfo {
            kind: "audio",
            width: 0,
            height: 0,
            frames: None,
            format: None,
            audio: Some(AudioSourceInfo {
                rate: 44_100,
                channels: 1,
                frames: 10,
                bits: Some(16),
                float: false,
                loop_range: None,
                decoder: "wav-rs-dec/1".into(),
                lazy: true,
            }),
        });
        let json = serde_json::to_value(source).unwrap();
        assert_eq!(json["audio"]["loop"], json!(null));
        assert_eq!(json["audio"]["lazy"], json!(true));
    }

    /// 書ける形式の一覧の JSON の形（camelCase。保存できるものは形式の表の値。今の出力の形式と、書けない理由の道具）。
    #[test]
    fn formats_serialize_for_the_front() {
        use kura_core::edit::editor::formats::formats_for;
        use kura_engine::codec::CodecSet;
        let shape = kura_engine::Shape::Audio {
            rate: 44_100,
            channels: 2,
            frames: kura_engine::Dim::Exact(1_000),
        };
        let dto: EditFormatsDto =
            formats_for(&CodecSet::pinned(), &FormatChoice::Keep, "mp3", &shape).into();
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["fallback"], json!("ogg"));
        assert_eq!(json["current"], json!("mp3"));
        let rows = json["formats"].as_array().unwrap();
        let row = |id: &str| rows.iter().find(|r| r["format"] == id).unwrap().clone();
        assert_eq!(
            row("mp3"),
            json!({
                "format": "mp3",
                "ext": "mp3",
                "available": false,
                "missingTool": "ffmpeg",
                "lossless": false,
                "loopPoints": false,
                "markers": false,
                "highBitDepth": false,
            })
        );
        assert_eq!(
            row("flac"),
            json!({
                "format": "flac",
                "ext": "flac",
                "available": true,
                "missingTool": null,
                "lossless": true,
                "loopPoints": true,
                "markers": false,
                "highBitDepth": true,
            })
        );
    }

    /// エディタのエラーは画面で分ける code にする。
    #[test]
    fn editor_errors_map_to_codes() {
        for (err, code) in [
            (EditorError::NoSession, AppErrorCode::NotFound),
            (EditorError::TooLarge("x".into()), AppErrorCode::TooLarge),
            (EditorError::Busy("x".into()), AppErrorCode::Busy),
            (
                EditorError::SavePending { job: 9 },
                AppErrorCode::SavePending,
            ),
            (
                EditorError::ToolMissing("ffmpeg".into()),
                AppErrorCode::ToolMissing,
            ),
            (
                EditorError::Invalid {
                    message: "x".into(),
                    issues: Vec::new(),
                },
                AppErrorCode::Invalid,
            ),
            (EditorError::Mismatch("x".into()), AppErrorCode::Conflict),
            (
                EditorError::FormatUnavailable {
                    format: "mp3".into(),
                    fallback: Some("ogg".into()),
                },
                AppErrorCode::FormatUnavailable,
            ),
            (
                EditorError::Core(kura_core::CoreError::NotFound("x".into())),
                AppErrorCode::NotFound,
            ),
        ] {
            assert_eq!(AppError::from(err).code, code);
        }
    }

    /// 操作のカタログ：今の版の操作が入り、引数の記述に表示の条件がある。
    #[test]
    fn the_catalog_lists_current_ops_with_visibility() {
        let ops = edit_ops();
        let json = serde_json::to_value(&ops).unwrap();
        let convert = json
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["id"] == "image.convert")
            .unwrap();
        assert_eq!(
            convert["schema"]["params"][1]["visible"],
            json!({ "key": "format", "values": ["indexed8"] })
        );
        assert!(ops.iter().any(|o| o.id == "paint.actions"));
    }
}
