//! ライセンスとクレジット（M15 段 1。docs/data-model.md §4「ライセンスとクレジット（M15）」、ADR-202・203）：ライセンスの一覧・
//! 足す・変える・消す、アイテムのクレジットの読み・まとめて付ける・複数選択の集計、使ったアイテムのクレジットを集める。
//! ロジックは `kura_core::credits`。

use kura_core::credits::collect::{self, CreditLine, Credits, Flagged};
use kura_core::credits::item::{self, CreditPatch, CreditSummary, ItemCredit};
use kura_core::credits::licenses::{self, License, LicenseInput};
use kura_core::credits::{Attribution, Permission};
use kura_core::time::now_ms;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;

use super::blocking;
use crate::error::AppError;
use crate::state::AppState;

/// 表記義務（必要・任意・不要）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum AttributionDto {
    Required,
    Optional,
    None,
}

impl From<Attribution> for AttributionDto {
    fn from(a: Attribution) -> Self {
        match a {
            Attribution::Required => Self::Required,
            Attribution::Optional => Self::Optional,
            Attribution::None => Self::None,
        }
    }
}

impl From<AttributionDto> for Attribution {
    fn from(a: AttributionDto) -> Self {
        match a {
            AttributionDto::Required => Self::Required,
            AttributionDto::Optional => Self::Optional,
            AttributionDto::None => Self::None,
        }
    }
}

/// 商用・改変の可否。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDto {
    Yes,
    No,
    Unknown,
}

impl From<Permission> for PermissionDto {
    fn from(p: Permission) -> Self {
        match p {
            Permission::Yes => Self::Yes,
            Permission::No => Self::No,
            Permission::Unknown => Self::Unknown,
        }
    }
}

impl From<PermissionDto> for Permission {
    fn from(p: PermissionDto) -> Self {
        match p {
            PermissionDto::Yes => Self::Yes,
            PermissionDto::No => Self::No,
            PermissionDto::Unknown => Self::Unknown,
        }
    }
}

/// ライセンス 1 行。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LicenseDto {
    pub id: i64,
    /// 既定の行の鍵（`cc-by-4.0` など）。利用者が足した行は null（既定の行は変えられず消せない）。
    pub builtin: Option<String>,
    pub name: String,
    pub spdx: Option<String>,
    pub url: Option<String>,
    pub attribution: AttributionDto,
    pub commercial: PermissionDto,
    pub modify: PermissionDto,
    pub share_alike: bool,
    pub note: Option<String>,
    /// このライセンスを付けたアイテムの数。
    pub items: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<License> for LicenseDto {
    fn from(l: License) -> Self {
        Self {
            id: l.id,
            builtin: l.builtin,
            name: l.name,
            spdx: l.spdx,
            url: l.url,
            attribution: l.attribution.into(),
            commercial: l.commercial.into(),
            modify: l.modify.into(),
            share_alike: l.share_alike,
            note: l.note,
            items: l.items,
            created_at: l.created_at,
            updated_at: l.updated_at,
        }
    }
}

/// 足す・変えるライセンスの中身（空の文は「なし」）。
#[derive(Debug, Clone, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct LicenseInputDto {
    pub name: String,
    pub spdx: Option<String>,
    pub url: Option<String>,
    pub attribution: AttributionDto,
    pub commercial: PermissionDto,
    pub modify: PermissionDto,
    pub share_alike: bool,
    pub note: Option<String>,
}

impl From<LicenseInputDto> for LicenseInput {
    fn from(i: LicenseInputDto) -> Self {
        Self {
            name: i.name,
            spdx: i.spdx,
            url: i.url,
            attribution: i.attribution.into(),
            commercial: i.commercial.into(),
            modify: i.modify.into(),
            share_alike: i.share_alike,
            note: i.note,
        }
    }
}

/// 1 件のクレジット（null は未設定）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ItemCreditDto {
    pub license_id: Option<i64>,
    pub author: Option<String>,
    /// 作品・素材集の名前。
    pub work: Option<String>,
    /// 出典（null なら取り込み元の URL を使う）。
    pub url: Option<String>,
    /// 表記の文（null なら作者・作品・出典・ライセンスから組む）。
    pub credit: Option<String>,
}

impl From<ItemCredit> for ItemCreditDto {
    fn from(c: ItemCredit) -> Self {
        Self {
            license_id: c.license_id,
            author: c.author,
            work: c.work,
            url: c.url,
            credit: c.credit,
        }
    }
}

/// ライセンスの変え方。
#[derive(Debug, Clone, Copy, Deserialize, Type)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LicenseChoiceDto {
    /// 未設定に戻す。
    Unset,
    /// そのライセンスにする。
    Set { id: i64 },
}

/// まとめて付ける項目（省いた項目は変えない。文の空文字は消す）。
#[derive(Debug, Clone, Default, Deserialize, Type)]
#[serde(rename_all = "camelCase", default)]
pub struct CreditPatchDto {
    pub license: Option<LicenseChoiceDto>,
    pub author: Option<String>,
    pub work: Option<String>,
    pub url: Option<String>,
    pub credit: Option<String>,
}

/// 空文字を「消す」として読む（`Some("")` → `Some(None)`。整えは kura-core が行う）。
fn clearable(value: Option<String>) -> Option<Option<String>> {
    value.map(|v| (!v.trim().is_empty()).then_some(v))
}

impl From<CreditPatchDto> for CreditPatch {
    fn from(p: CreditPatchDto) -> Self {
        Self {
            license: p.license.map(|c| match c {
                LicenseChoiceDto::Unset => None,
                LicenseChoiceDto::Set { id } => Some(id),
            }),
            author: clearable(p.author),
            work: clearable(p.work),
            url: clearable(p.url),
            credit: clearable(p.credit),
        }
    }
}

/// 複数選択の 1 項目（`mixed` なら食い違い、そうでなければ全件が `value`）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SharedDto<T> {
    pub mixed: bool,
    pub value: Option<T>,
}

fn shared<T>(v: Option<Option<T>>) -> SharedDto<T> {
    match v {
        Some(value) => SharedDto {
            mixed: false,
            value,
        },
        None => SharedDto {
            mixed: true,
            value: None,
        },
    }
}

/// 複数選択のクレジットの集計。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreditSummaryDto {
    /// 対象のアイテムの数（ユニットはメンバーを数える）。
    pub count: u32,
    pub license_id: SharedDto<i64>,
    pub author: SharedDto<String>,
    pub work: SharedDto<String>,
    pub url: SharedDto<String>,
    pub credit: SharedDto<String>,
}

impl From<CreditSummary> for CreditSummaryDto {
    fn from(s: CreditSummary) -> Self {
        Self {
            count: u32::try_from(s.count).unwrap_or(u32::MAX),
            license_id: shared(s.license),
            author: shared(s.author),
            work: shared(s.work),
            url: shared(s.url),
            credit: shared(s.credit),
        }
    }
}

/// まとめて付けた結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreditsSetDto {
    /// 対象のアイテムの数（ユニットはメンバーを数える）。
    pub items: u32,
    /// 変えた件数（中身が同じものは数えない）。
    pub changed: u32,
}

/// 集めたクレジットの 1 行（同じ表記をまとめたもの）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreditLineDto {
    pub license_id: i64,
    pub work: Option<String>,
    pub author: Option<String>,
    /// 出典（クレジットの URL、なければ取り込み元の URL）。
    pub url: Option<String>,
    pub credit: Option<String>,
    /// このクレジットを持つアイテム（来歴の元を含む）。
    pub items: Vec<i64>,
    /// それにたどり着いた、使ったアイテム。
    pub used: Vec<i64>,
}

impl From<CreditLine> for CreditLineDto {
    fn from(l: CreditLine) -> Self {
        Self {
            license_id: l.key.license_id,
            work: l.key.work,
            author: l.key.author,
            url: l.key.url,
            credit: l.key.credit,
            items: l.items,
            used: l.used,
        }
    }
}

/// 警告の 1 件（未設定・元が消えた）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreditFlagDto {
    pub item: i64,
    pub used: Vec<i64>,
}

impl From<Flagged> for CreditFlagDto {
    fn from(f: Flagged) -> Self {
        Self {
            item: f.item,
            used: f.used,
        }
    }
}

/// 集めたクレジット（行はライセンスの並び → 作品・作者・出典・表記の順）。
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CreditsDto {
    pub lines: Vec<CreditLineDto>,
    /// 行が使うライセンス（並びの順）。
    pub licenses: Vec<LicenseDto>,
    /// 来歴の元がなく、ライセンスも未設定のアイテム。
    pub unset: Vec<CreditFlagDto>,
    /// 来歴の元が消えて、たどれないアイテム。
    pub lost: Vec<CreditFlagDto>,
}

impl From<Credits> for CreditsDto {
    fn from(c: Credits) -> Self {
        Self {
            lines: c.lines.into_iter().map(Into::into).collect(),
            licenses: c.licenses.into_iter().map(Into::into).collect(),
            unset: c.unset.into_iter().map(Into::into).collect(),
            lost: c.lost.into_iter().map(Into::into).collect(),
        }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// ライセンスの一覧（並びの順。既定の行が先。付けた件数つき）。
#[tauri::command]
#[specta::specta]
pub async fn licenses_list(state: State<'_, AppState>) -> Result<Vec<LicenseDto>, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(licenses::list)?
            .into_iter()
            .map(Into::into)
            .collect())
    })
    .await
}

/// 利用者のライセンスを足す（並びの最後）。ID を返す。同じ名前（大文字小文字を問わない）があれば Conflict。
#[tauri::command]
#[specta::specta]
pub async fn license_create(
    state: State<'_, AppState>,
    input: LicenseInputDto,
) -> Result<i64, AppError> {
    let session = state.session()?;
    let input: LicenseInput = input.into();
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| licenses::create(tx, &input, now))?)
    })
    .await
}

/// 利用者のライセンスを変える（既定の行は Invalid）。
#[tauri::command]
#[specta::specta]
pub async fn license_update(
    state: State<'_, AppState>,
    id: i64,
    input: LicenseInputDto,
) -> Result<(), AppError> {
    let session = state.session()?;
    let input: LicenseInput = input.into();
    blocking(move || {
        let now = now_ms();
        Ok(session
            .library()
            .db()
            .write(move |tx| licenses::update(tx, id, &input, now))?)
    })
    .await
}

/// 利用者のライセンスを消す（既定の行は Invalid）。使っていたアイテムは未設定に戻り、その件数を返す（画面は消す前に
/// `items` の件数で確かめる）。
#[tauri::command]
#[specta::specta]
pub async fn license_delete(state: State<'_, AppState>, id: i64) -> Result<i64, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .write(move |tx| licenses::delete(tx, id))?)
    })
    .await
}

/// 1 件のクレジット（付けていなければ全部 null）。アイテムがなければ NotFound。
#[tauri::command]
#[specta::specta]
pub async fn item_credit_get(
    state: State<'_, AppState>,
    item: i64,
) -> Result<ItemCreditDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(move |c| item::get(c, item))?
            .into())
    })
    .await
}

/// 選んだアイテムとユニット（メンバー全員）のクレジットの集計（複数選択のインスペクタ）。
#[tauri::command]
#[specta::specta]
pub async fn item_credits_summary(
    state: State<'_, AppState>,
    item_ids: Vec<i64>,
    unit_ids: Vec<i64>,
) -> Result<CreditSummaryDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(move |c| {
                let items = item::expand(c, &item_ids, &unit_ids)?;
                item::summary(c, &items)
            })?
            .into())
    })
    .await
}

/// 選んだアイテムとユニット（メンバー全員）にクレジットの項目をまとめて付ける（省いた項目はアイテムごとのまま）。
#[tauri::command]
#[specta::specta]
pub async fn item_credits_set(
    state: State<'_, AppState>,
    item_ids: Vec<i64>,
    unit_ids: Vec<i64>,
    patch: CreditPatchDto,
) -> Result<CreditsSetDto, AppError> {
    let session = state.session()?;
    let patch: CreditPatch = patch.into();
    blocking(move || {
        let now = now_ms();
        Ok(session.library().db().write(move |tx| {
            let items = item::expand(tx, &item_ids, &unit_ids)?;
            let changed = item::set(tx, &items, &patch, now)?;
            Ok(CreditsSetDto {
                items: count(items.len()),
                changed: count(changed),
            })
        })?)
    })
    .await
}

/// 使ったアイテムのクレジットを、来歴の元をたどって集める（同じ表記の行は 1 つにまとめる）。
#[tauri::command]
#[specta::specta]
pub async fn credits_collect(
    state: State<'_, AppState>,
    items: Vec<i64>,
) -> Result<CreditsDto, AppError> {
    let session = state.session()?;
    blocking(move || {
        Ok(session
            .library()
            .db()
            .read(move |c| collect::collect(c, &items))?
            .into())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_reads_empty_text_as_clear_and_keeps_omitted_fields() {
        let dto: CreditPatchDto = serde_json::from_str(
            r#"{"license":{"type":"set","id":3},"author":"  ","work":"森の素材集"}"#,
        )
        .unwrap();
        let p: CreditPatch = dto.into();
        assert_eq!(p.license, Some(Some(3)));
        assert_eq!(p.author, Some(None));
        assert_eq!(p.work, Some(Some("森の素材集".into())));
        assert_eq!((p.url, p.credit), (None, None));
        let dto: CreditPatchDto = serde_json::from_str(r#"{"license":{"type":"unset"}}"#).unwrap();
        assert_eq!(CreditPatch::from(dto).license, Some(None));
        let dto: CreditPatchDto = serde_json::from_str("{}").unwrap();
        assert_eq!(CreditPatch::from(dto), CreditPatch::default());
    }

    #[test]
    fn summary_marks_mixed_fields() {
        let dto: CreditSummaryDto = CreditSummary {
            count: 2,
            license: Some(Some(1)),
            author: None,
            work: Some(None),
            url: Some(None),
            credit: None,
        }
        .into();
        assert_eq!(
            dto.license_id,
            SharedDto {
                mixed: false,
                value: Some(1)
            }
        );
        assert!(dto.author.mixed);
        assert_eq!(
            dto.work,
            SharedDto {
                mixed: false,
                value: None
            }
        );
    }
}
