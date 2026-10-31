/**
 * 種類ごとのエディタの表と、アイテムを開く口（M13 の段 6a）。
 *
 * - 種類ごとのモジュール（画像は `image/imageKind.ts`）は、核のアダプタの作り方・画面の部品・キーをまとめて持つ。核
 *   （`editorStore.ts`）はここを読まない（種類のモジュールが核を読むので、輪にしない）。画面の殻（`Editor.tsx`）とキー
 *   （`editorKeys.ts`）は、開いている種類（`useEditorStore` の `kind`）でここから部品を引く。
 * - 種類を足すときは `kindTraits.ts` の `EditorKind`・性質と、ここの表に 1 行足す（音声は M13 の段 6。開けるのは
 *   `kindTraits.ts` の `OPENABLE` にある種類だけ）。
 */

import type { ComponentType } from 'react';

import type { ItemSummaryDto } from '../../ipc/items';
import { AUDIO_EDITOR } from './audio/audioKind';
import type { EditorKindKeys } from './editorKeys';
import { type EditorAdapterFactory, openEditorAs } from './editorStore';
import { IMAGE_EDITOR } from './image/imageKind';
import { canEdit, EDITOR_KINDS, type EditorKind, editorKindOf } from './kindTraits';

export { canEdit };

/** 種類ごとの画面の部品（殻の決まった位置に置く。どれも引数を取らず、ストアから読む）。 */
export interface EditorKindUi {
  /** 表示の面（キャンバス・波形）。舞台の先頭。 */
  Stage: ComponentType;
  /** 上部の道具のカプセル（名前と「情報」の間）。 */
  Tools: ComponentType;
  /** 上部の名前の下の一行（「64 × 64 · PNG · 未保存」）。 */
  Meta: ComponentType;
  /** 下のガラスのカプセル（開いている間だけ）。 */
  Capsule: ComponentType;
  /** 左下・右下の表示（座標・倍率など）。 */
  Status: ComponentType;
  /** 右パネルの「操作」（核の `OpsPanel` に、引数の欄の大きさ・出力の行を種類ごとに渡す）。 */
  Ops: ComponentType;
}

/** 種類ごとのエディタ。 */
export interface EditorKindModule {
  adapter: EditorAdapterFactory;
  ui: EditorKindUi;
  keys: EditorKindKeys;
}

const MODULES: Readonly<Record<EditorKind, EditorKindModule>> = {
  image: IMAGE_EDITOR,
  audio: AUDIO_EDITOR,
};

/** 種類のモジュール（閉じていれば null）。 */
export function editorKindModule(kind: EditorKind | null): EditorKindModule | null {
  return kind === null ? null : MODULES[kind];
}

/** 種類のキー（閉じていれば null）。 */
export function editorKindKeys(kind: EditorKind | null): EditorKindKeys | null {
  return editorKindModule(kind)?.keys ?? null;
}

/** すべての種類のキー（押している間だけの道具を戻すとき）。 */
export function everyEditorKindKeys(): EditorKindKeys[] {
  return EDITOR_KINDS.map((kind) => MODULES[kind].keys);
}

/** エディタを開く（開ける種類だけ。前のエディタは閉じる）。 */
export function openEditor(
  item: ItemSummaryDto,
  options: { fromDraft?: boolean } = {},
): Promise<void> {
  const kind = editorKindOf(item);
  if (kind === null) return Promise.resolve();
  return openEditorAs(MODULES[kind].adapter, item, options);
}
