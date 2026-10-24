/**
 * 画像エディタのセッション（M12。docs/editing.md §12.8・§12.12・§12.13、docs/architecture.md §3）。
 *
 * - 開く（`openEdit`）→ 手（`stepEdit`）・表示だけ（`viewEdit`）→ 閉じる（`closeEdit`）。表示のタイルは IPC で送らず、
 *   `src/ipc/media.ts` の `editTileUrl` で kura:// から読む（変わったタイルだけを、結果の今の rev・段で）。
 * - 段は Rust が決める（フロントは表示する範囲・倍率 × DPR を送るだけ）。
 * - 操作のカタログは bindings の定数 `EDIT_OPS`（表示の条件は `schema.params[].visible`）。
 */

import {
  commands,
  EDIT_OPS,
  type CommitMode,
  type EditChangedDto,
  type EditDocDeltaDto_Serialize,
  type EditDraftDto,
  type EditFrameDto,
  type EditNodeDto,
  type EditOpenedDto_Serialize,
  type EditProblemDto,
  type EditRectDto,
  type EditSampleDto,
  type EditSaveModeDto,
  type EditSaveStartedDto_Serialize,
  type EditSourceDto,
  type EditStatusDto,
  type EditStepDto_Deserialize,
  type EditStepResultDto_Serialize,
  type EditVersionDto,
  type EditViewDto,
  type EditViewOutcomeDto,
  type EditViewportDto,
  type Issue,
  type JsonValue,
} from './bindings';
import { unwrap } from './result';

export { EDIT_OPS };

export type {
  CommitMode,
  EditChangedDto,
  EditDraftDto,
  EditFrameDto,
  EditNodeDto,
  EditProblemDto,
  EditRectDto,
  EditSampleDto,
  EditSaveModeDto,
  EditSourceDto,
  EditStatusDto,
  EditVersionDto,
  EditViewDto,
  EditViewOutcomeDto,
  EditViewportDto,
  Issue,
  JsonValue,
};

/** 開いた結果（Rust が返す形）。 */
export type EditOpenedDto = EditOpenedDto_Serialize;
/** 手と表示の結果（Rust が返す形）。 */
export type EditStepResultDto = EditStepResultDto_Serialize;
/** 文書の差分（Rust が返す形）。 */
export type EditDocDeltaDto = EditDocDeltaDto_Serialize;
/** 保存を積んだ結果（Rust が返す形）。 */
export type EditSaveStartedDto = EditSaveStartedDto_Serialize;
/** 手 1 つ（フロントが送る形）。 */
export type EditStepDto = EditStepDto_Deserialize;
/** 出力の形式の指定（保存）。 */
export type EditFormatChoice = Parameters<typeof commands.editSave>[2];

/** エディタを開く（前のエディタは閉じる）。`fromDraft` が真で下書きがあれば下書きの文書で開く。画像だけ。 */
export function openEdit(
  itemId: number,
  viewport: EditViewportDto,
  fromDraft: boolean | null = null,
): Promise<EditOpenedDto> {
  return unwrap(commands.editOpen(itemId, viewport, fromDraft));
}

/** 手をまとめて当てて表示する（描くのは最後に 1 回）。 */
export function stepEdit(
  session: string,
  steps: EditStepDto[],
  viewport: EditViewportDto,
): Promise<EditStepResultDto> {
  return unwrap(commands.editStep(session, steps, viewport));
}

/** 表示だけ（パン・ズーム・フレームの切り替え。保存のジョブのあとの状態の取り直し）。 */
export function viewEdit(session: string, viewport: EditViewportDto): Promise<EditStepResultDto> {
  return unwrap(commands.editView(session, viewport));
}

/** スポイト：今の rev の等倍の 1 画素（キャンバスの座標）。画像の外は null。 */
export function sampleEdit(
  session: string,
  frame: number,
  x: number,
  y: number,
): Promise<EditSampleDto | null> {
  return unwrap(commands.editSample(session, frame, x, y));
}

/** エディタを閉じる（キャッシュ・予算の取り置きを外す）。今のセッションでなければ Rust は何もしない。 */
export function closeEdit(session: string): Promise<null> {
  return unwrap(commands.editClose(session));
}

/** 保存のジョブを積む（保存が待っている間は Busy）。 */
export function saveEdit(
  session: string,
  mode: EditSaveModeDto,
  format: EditFormatChoice = null,
): Promise<EditSaveStartedDto> {
  return unwrap(commands.editSave(session, mode, format));
}

/** アイテムの版の一覧（まだ保存していなければ空）。 */
export function listEditVersions(itemId: number): Promise<EditVersionDto[]> {
  return unwrap(commands.editVersions(itemId));
}

/** アイテムの下書きを捨てる（あったら真）。 */
export function discardEditDraft(itemId: number): Promise<boolean> {
  return unwrap(commands.editDraftDiscard(itemId));
}
