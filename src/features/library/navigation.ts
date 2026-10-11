/**
 * ↑↓ で移る先を決める係。一覧を描いている部品（グリッド・リスト）が配置を知っているので、
 * 描いている側が登録し、キー操作（useLibraryKeys）が呼ぶ。
 */

/** `index` 番目から上（-1）・下（+1）へ移る先の番号。なければ null。 */
export type VerticalNavigator = (index: number, dir: -1 | 1) => number | null;

let navigator: VerticalNavigator | null = null;

/** 登録する（外すときは null）。 */
export function setVerticalNavigator(next: VerticalNavigator | null): void {
  navigator = next;
}

/** 移る先（一覧が出ていなければ null）。 */
export function verticalTarget(index: number, dir: -1 | 1): number | null {
  return navigator ? navigator(index, dir) : null;
}

/**
 * キーで選択を動かしたとき、キーボードのフォーカスも新しい行・タイルへ移す（読み上げも追いつくように）。
 * フォーカスが一覧の中にないとき（検索欄・インスペクタなど）は動かさない。
 */
export function focusEntry(id: number): void {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || !active.hasAttribute('data-entry')) {
    return;
  }
  const next = document.querySelector<HTMLElement>(`[data-entry="${id}"]`);
  if (next && next !== active) {
    next.focus({ preventScroll: true });
  }
}
