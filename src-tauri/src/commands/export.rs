//! 書き出し（FR-SYS-02。ADR-120）。M15 段 3 で名前の規則・形式・JSON・プロジェクトの設定（ADR-205）を足した。

use std::collections::BTreeSet;
use std::path::PathBuf;

use kura_core::export::names::{NameRule, NameStyle};
use kura_core::export::{
    ExportInput, ExportOptions, ExportPreset, FormatTarget, PreviewName, preview_entries,
    start_export,
};
use kura_core::import::MediaKind;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use super::blocking;
use crate::error::{AppError, AppErrorCode};
use crate::state::AppState;

/// 名前の書き方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum NameStyleDto {
    Keep,
    Snake,
    Kebab,
    Pascal,
    Camel,
}

impl From<NameStyle> for NameStyleDto {
    fn from(s: NameStyle) -> Self {
        match s {
            NameStyle::Keep => Self::Keep,
            NameStyle::Snake => Self::Snake,
            NameStyle::Kebab => Self::Kebab,
            NameStyle::Pascal => Self::Pascal,
            NameStyle::Camel => Self::Camel,
        }
    }
}

impl From<NameStyleDto> for NameStyle {
    fn from(s: NameStyleDto) -> Self {
        match s {
            NameStyleDto::Keep => Self::Keep,
            NameStyleDto::Snake => Self::Snake,
            NameStyleDto::Kebab => Self::Kebab,
            NameStyleDto::Pascal => Self::Pascal,
            NameStyleDto::Camel => Self::Camel,
        }
    }
}

/// 名前の規則（テンプレートの語は {名前}・{元の名前}・{番号}・{ユニット}・{幅}・{高さ}。null は元のファイル名）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct NameRuleDto {
    pub template: Option<String>,
    pub style: NameStyleDto,
    /// ASCII の文字だけにする（ほかを除き、空なら item-ID）。
    pub ascii: bool,
}

impl From<NameRule> for NameRuleDto {
    fn from(r: NameRule) -> Self {
        Self {
            template: r.template,
            style: r.style.into(),
            ascii: r.ascii,
        }
    }
}

impl From<NameRuleDto> for NameRule {
    fn from(r: NameRuleDto) -> Self {
        Self {
            template: r.template,
            style: r.style.into(),
            ascii: r.ascii,
        }
    }
}

/// 画像・音声の書く形式（`original` はそのまま写す（M9）、`playable` は変えない（書けない形式は PNG・Ogg Vorbis に変える）、
/// `to` はその形式）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FormatTargetDto {
    Original,
    Playable,
    To { format: String },
}

impl From<FormatTarget> for FormatTargetDto {
    fn from(t: FormatTarget) -> Self {
        match t {
            FormatTarget::Original => Self::Original,
            FormatTarget::Playable => Self::Playable,
            FormatTarget::To { format } => Self::To { format },
        }
    }
}

impl From<FormatTargetDto> for FormatTarget {
    fn from(t: FormatTargetDto) -> Self {
        match t {
            FormatTargetDto::Original => Self::Original,
            FormatTargetDto::Playable => Self::Playable,
            FormatTargetDto::To { format } => Self::To { format },
        }
    }
}

/// 書き出しの設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptionsDto {
    pub name: NameRuleDto,
    pub image: FormatTargetDto,
    pub audio: FormatTargetDto,
    /// `kura-export.json` を添える。
    pub manifest: bool,
    /// 登録したプロジェクトの中へ書き出す（書き出し先がその中であることを確かめ、終わったら照らし直す）。
    pub project: Option<i64>,
}

impl From<ExportOptionsDto> for ExportOptions {
    fn from(o: ExportOptionsDto) -> Self {
        Self {
            name: o.name.into(),
            image: o.image.into(),
            audio: o.audio.into(),
            manifest: o.manifest,
            project: o.project,
        }
    }
}

/// プロジェクトの持ち出しの既定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportPresetDto {
    pub name: NameRuleDto,
    pub image: FormatTargetDto,
    pub audio: FormatTargetDto,
    pub manifest: bool,
}

impl From<ExportPreset> for ExportPresetDto {
    fn from(p: ExportPreset) -> Self {
        Self {
            name: p.name.into(),
            image: p.image.into(),
            audio: p.audio.into(),
            manifest: p.manifest,
        }
    }
}

impl From<ExportPresetDto> for ExportPreset {
    fn from(p: ExportPresetDto) -> Self {
        Self {
            name: p.name.into(),
            image: p.image.into(),
            audio: p.audio.into(),
            manifest: p.manifest,
        }
    }
}

/// 書き出しで選べる形式（形式の表の順。今の環境で書けるもの）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportFormatsDto {
    pub image: Vec<String>,
    pub audio: Vec<String>,
}

/// 書き出す名前の見本の 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PreviewNameDto {
    pub item_id: i64,
    pub name: String,
    /// 形式を変えるならその形式。
    pub convert: Option<String>,
}

impl From<PreviewName> for PreviewNameDto {
    fn from(p: PreviewName) -> Self {
        Self {
            item_id: p.item_id,
            name: p.name,
            convert: p.convert,
        }
    }
}

/// 今の環境で書ける形式（エンジンの部品がなければ None）。
fn writable_formats(
    session: &kura_core::library::LibrarySession,
) -> Option<Vec<(MediaKind, String)>> {
    let engine = session.engine()?;
    Some(
        engine
            .codecs()
            .capabilities()
            .into_iter()
            .filter(|c| !c.write.is_empty())
            .map(|c| (MediaKind::from(c.item_kind), c.format.to_string()))
            .collect(),
    )
}

/// 書き出しで選べる形式（画像・音声。形式の表の順）。
#[tauri::command]
#[specta::specta]
pub async fn export_formats(state: State<'_, AppState>) -> Result<ExportFormatsDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        let list = writable_formats(&session).unwrap_or_default();
        let of = |kind: MediaKind| -> Vec<String> {
            list.iter()
                .filter(|(k, _)| *k == kind)
                .map(|(_, f)| f.clone())
                .collect()
        };
        Ok(ExportFormatsDto {
            image: of(MediaKind::Image),
            audio: of(MediaKind::Audio),
        })
    })
    .await
}

/// 書き出しの見本（先頭の 50 件と、書き出す件数）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ExportPreviewDto {
    pub names: Vec<PreviewNameDto>,
    /// 書き出す件数（ゴミ箱の中・見つからないものを除く。ユニットのメンバーはユニットごとに数える）。
    pub total: u32,
}

/// 選んだもの（一覧の ID。ユニットは印付き）の書き出す名前と形式の見本。書き出しのジョブと同じ並び・番号・ユニットの
/// フォルダ（名前は `フォルダ/名前`）で、先頭の 50 件と件数（M15 段 6 の検証の直し）。
#[tauri::command]
#[specta::specta]
pub async fn export_preview(
    state: State<'_, AppState>,
    entries: Vec<i64>,
    options: ExportOptionsDto,
) -> Result<ExportPreviewDto, AppError> {
    let session = state.session()?;
    let options: ExportOptions = options.into();
    blocking(move || {
        let writable: Option<BTreeSet<String>> =
            writable_formats(&session).map(|l| l.into_iter().map(|(_, f)| f).collect());
        let check = |f: &str| writable.as_ref().is_some_and(|w| w.contains(f));
        let writable_ref: Option<&dyn Fn(&str) -> bool> =
            writable.as_ref().map(|_| &check as &dyn Fn(&str) -> bool);
        let p = session
            .library()
            .db()
            .read(move |c| preview_entries(c, &entries, &options, writable_ref, 50))?;
        Ok(ExportPreviewDto {
            names: p.names.into_iter().map(Into::into).collect(),
            total: u32::try_from(p.total).unwrap_or(u32::MAX),
        })
    })
    .await
}

/// 選んだもの（一覧の ID。ユニットは印付き）を `dest` に書き出すジョブを積む。
/// 書き出し先はあるフォルダで、ライブラリの中は受け付けない。`options` が null なら M9 と同じ（元の名前・形式はそのまま・
/// JSON を添える）。`options.project` があれば、書き出し先がそのプロジェクトのフォルダの中であることを確かめる。
#[tauri::command]
#[specta::specta]
pub async fn export_start(
    state: State<'_, AppState>,
    dest: String,
    entries: Vec<i64>,
    options: Option<ExportOptionsDto>,
) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        let dest = PathBuf::from(dest);
        let missing = || {
            AppError::new(
                AppErrorCode::Invalid,
                "書き出し先のフォルダが見つかりません",
            )
        };
        if !dest.is_absolute() || !dest.is_dir() {
            return Err(missing());
        }
        let dest = dest.canonicalize().map_err(|_| missing())?;
        let root = session
            .library()
            .layout()
            .root()
            .canonicalize()
            .map_err(|e| AppError::internal(e.to_string()))?;
        if dest.starts_with(&root) {
            return Err(AppError::new(
                AppErrorCode::Invalid,
                "ライブラリの中には書き出せません。ほかのフォルダを選んでください",
            ));
        }
        let options: ExportOptions = options.map(Into::into).unwrap_or_default();
        if let Some(project) = options.project {
            let p = session
                .library()
                .db()
                .read(move |c| kura_core::project::get(c, project))?;
            kura_core::project::check_dest(&p, &dest)?;
        }
        Ok(start_export(
            session.jobs(),
            &ExportInput {
                dest,
                entries,
                options,
            },
        )?)
    })
    .await
}
