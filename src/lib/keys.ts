/**
 * キーの受け方で共有する判定（メニューの「取り消す」と画像エディタのキーで同じ判定を使う）。
 *
 * - 文字を打つ所（`isTextEntry`）：文字を打つ `<input>`（text・search・number など）・`<textarea>`・contenteditable。
 *   ⌘Z は文字の取り消しにする。チェックボックス・スライダー・ラジオ・ボタンの `<input>` と `<select>` は文字を打たない
 *   ので含めない（⌘Z・⌘S はエディタへ）。
 * - メニューが送ったキー（`markReplayed`・`isReplayed`）：メニューの項目はキーを押したことにして keydown だけを送る
 *   （離したキーは来ない）。押している間だけの道具（Space の手のひら・⌥ のスポイト）はこのキーでは替えない。
 */

/** 文字を打たない `<input>` の種類。 */
const NON_TEXT_INPUTS: ReadonlySet<string> = new Set([
  'button',
  'checkbox',
  'color',
  'file',
  'hidden',
  'image',
  'radio',
  'range',
  'reset',
  'submit',
]);

/** 文字を打つ所か（文字を打つ入力欄・テキストエリア・contenteditable）。 */
export function isTextEntry(target: EventTarget | null): boolean {
  if (target instanceof HTMLInputElement) return !NON_TEXT_INPUTS.has(target.type);
  if (target instanceof HTMLTextAreaElement) return true;
  return target instanceof HTMLElement && target.isContentEditable === true;
}

/** 文字は打たないが、矢印・Space などのキーを自分で使う部品か（選択・スライダー・チェックボックスなど）。 */
export function isKeyControl(target: EventTarget | null): boolean {
  if (target instanceof HTMLSelectElement) return true;
  return target instanceof HTMLInputElement && NON_TEXT_INPUTS.has(target.type);
}

const replayed = new WeakSet<Event>();

/** メニューが送ったキーの印を付ける。 */
export function markReplayed(e: Event): void {
  replayed.add(e);
}

/** メニューが送ったキーか（本当に押したキーではない。離したキーは来ない）。 */
export function isReplayed(e: Event): boolean {
  return replayed.has(e);
}
