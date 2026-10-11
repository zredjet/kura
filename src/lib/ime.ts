/** キーのイベントの、日本語入力の判定に要るところ。 */
interface KeyLike {
  isComposing: boolean;
  keyCode: number;
}

/**
 * 日本語入力（IME）の変換中・確定のキーか。こういうキーで Enter の確定や Esc の取り消しをしない。
 * WebKit では、変換を確定する Enter が compositionend のあとに `isComposing = false`・`keyCode = 229` で
 * 届くので、両方を見る（React のイベントは `nativeEvent` を見る）。
 */
export function isImeKey(e: KeyLike | { nativeEvent: KeyLike }): boolean {
  const native = 'nativeEvent' in e ? e.nativeEvent : e;
  return native.isComposing || native.keyCode === 229;
}
