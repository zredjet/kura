import {
  type AttributionDto,
  commands,
  type CreditFlagDto,
  type CreditLineDto,
  type CreditPatchDto,
  type CreditsDto,
  type CreditsSetDto,
  type CreditSummaryDto,
  type ItemCreditDto,
  type LicenseChoiceDto,
  type LicenseDto,
  type LicenseInputDto,
  type PermissionDto,
  type SharedDto,
} from './bindings';
import { unwrap } from './result';

export type {
  AttributionDto,
  CreditFlagDto,
  CreditLineDto,
  CreditPatchDto,
  CreditsDto,
  CreditsSetDto,
  CreditSummaryDto,
  ItemCreditDto,
  LicenseChoiceDto,
  LicenseDto,
  LicenseInputDto,
  PermissionDto,
  SharedDto,
};

/** ライセンスの一覧（並びの順。既定の行が先。付けた件数つき）。 */
export function listLicenses(): Promise<LicenseDto[]> {
  return unwrap(commands.licensesList());
}

/** 利用者のライセンスを足す。ID を返す（同じ名前があれば `Conflict`）。 */
export function createLicense(input: LicenseInputDto): Promise<number> {
  return unwrap(commands.licenseCreate(input));
}

/** 利用者のライセンスを変える（既定の行は `Invalid`）。 */
export function updateLicense(id: number, input: LicenseInputDto): Promise<null> {
  return unwrap(commands.licenseUpdate(id, input));
}

/** 利用者のライセンスを消す。使っていたアイテムは未設定に戻り、その件数を返す（消す前に `items` で確かめる）。 */
export function deleteLicense(id: number): Promise<number> {
  return unwrap(commands.licenseDelete(id));
}

/** 1 件のクレジット（付けていなければ全部 null）。 */
export function getItemCredit(item: number): Promise<ItemCreditDto> {
  return unwrap(commands.itemCreditGet(item));
}

/** 選んだアイテムとユニット（メンバー全員）のクレジットの集計。 */
export function summarizeCredits(itemIds: number[], unitIds: number[]): Promise<CreditSummaryDto> {
  return unwrap(commands.itemCreditsSummary(itemIds, unitIds));
}

/** 選んだアイテムとユニット（メンバー全員）にクレジットをまとめて付ける（省いた項目は変えない。文の空文字は消す）。 */
export function setCredits(
  itemIds: number[],
  unitIds: number[],
  patch: CreditPatchDto,
): Promise<CreditsSetDto> {
  return unwrap(commands.itemCreditsSet(itemIds, unitIds, patch));
}

/** 使ったアイテムのクレジットを、来歴の元をたどって集める（同じ表記の行は 1 つ）。 */
export function collectCredits(items: number[]): Promise<CreditsDto> {
  return unwrap(commands.creditsCollect(items));
}
