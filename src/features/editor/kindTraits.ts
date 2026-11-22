/**
 * エディタで開ける種類と、種類ごとの性質（純粋なデータ。M13 の段 6a・6）。
 *
 * - 核（`editorStore.ts`・`editorSave.ts`）が種類を知らずに使えるよう、種類ごとの違いのうちデータで済むもの（形式の表・
 *   足せる操作の入力）だけをここに置く。開く口・画面の部品・キーは `editorKinds.ts`（状態を持つ種類のモジュールを読む）。
 * - 種類はアイテムの種類（`src/ipc/kinds.ts`）から選ぶ。エディタの種類（`EditorKind`・`EDITOR_KINDS`）と、利用者が開ける
 *   種類（`OPENABLE`）を分ける（音声のエディタは部品とテストを先に積み、開く口は M13 の段 6 の G9 で開けた）。
 */

import type { ValueKind } from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import type { ItemKind } from '../../ipc/kinds';
import { AUDIO_FORMATS } from './audio/audioFormats';
import type { FormatTable } from './formats';
import { IMAGE_FORMATS } from './image/imageFormats';

/** エディタの種類（画像・音声。種類の型は kinds.ts から引く）。 */
export type EditorKind = Extract<ItemKind, 'image'> | Extract<ItemKind, 'audio'>;

/** 種類ごとの性質。 */
interface KindTraits {
  /** 形式の表（元のファイルの形式・保存で書ける形式）。 */
  formats: FormatTable;
  /** 「操作を追加」に出す操作の入力（最初の入力がこのどれかを受けるもの）。 */
  inputs: readonly ValueKind[];
}

const TRAITS: Readonly<Record<EditorKind, KindTraits>> = {
  image: { formats: IMAGE_FORMATS, inputs: ['image', 'frames'] },
  audio: { formats: AUDIO_FORMATS, inputs: ['audio'] },
};

/** エディタの種類（並びは表の順）。 */
export const EDITOR_KINDS = Object.keys(TRAITS) as EditorKind[];

/** 利用者が開ける種類（いまはエディタの種類のすべて。音声は M13 の段 6 の G9 から）。 */
export const OPENABLE: readonly EditorKind[] = EDITOR_KINDS;

/** アイテムの種類のエディタ（開けるかによらない。形式の表を引く）。なければ null。 */
function kindOfItem(item: Pick<ItemSummaryDto, 'kind'> | null | undefined): EditorKind | null {
  const kind = item?.kind;
  return kind !== undefined && (EDITOR_KINDS as readonly string[]).includes(kind)
    ? (kind as EditorKind)
    : null;
}

/** アイテムを開くエディタの種類（開けなければ null。ゴミ箱・ユニットは呼ぶ側で外す）。 */
export function editorKindOf(
  item: Pick<ItemSummaryDto, 'kind'> | null | undefined,
): EditorKind | null {
  const kind = kindOfItem(item);
  return kind !== null && OPENABLE.includes(kind) ? kind : null;
}

/** 編集できるアイテムか（ゴミ箱・ユニットは呼ぶ側で外す）。 */
export function canEdit(item: Pick<ItemSummaryDto, 'kind'> | null | undefined): boolean {
  return editorKindOf(item) !== null;
}

/** 種類の形式の表。 */
export function formatsOf(kind: EditorKind): FormatTable {
  return TRAITS[kind].formats;
}

/** アイテムの形式の表（元のファイルの形式はアイテムの拡張子をこの表で読む。エディタのない種類は null）。 */
export function formatsFor(
  item: Pick<ItemSummaryDto, 'kind'> | null | undefined,
): FormatTable | null {
  const kind = kindOfItem(item);
  return kind === null ? null : formatsOf(kind);
}

/** 「操作を追加」に出す操作の入力。 */
export function opInputsOf(kind: EditorKind): readonly ValueKind[] {
  return TRAITS[kind].inputs;
}
