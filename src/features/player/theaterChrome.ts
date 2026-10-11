/**
 * シアターの操作（上の操作・再生バー）の出し方の決まり（純粋関数。design.md §3.1・ADR-128）。
 * - 再生中にマウスを `HIDE_AFTER_MS` 動かさなければ隠す。一時停止中・操作の上にポインタやフォーカスが
 *   あるとき・メニューやシートを開いているとき・入力欄で文字を入れているときは隠さない。
 * - フルスクリーンに入ったら右のパネルを閉じ、出たら入る前の状態に戻す。
 */

import type { PlayerPanel } from './playerStore';

/** 操作を隠すまでの、マウスを動かしていない時間。 */
export const HIDE_AFTER_MS = 3000;

export interface HideContext {
  playing: boolean;
  /** ポインタが操作の上にあるか。 */
  hovering: boolean;
  /** キーボードのフォーカスが操作の中にあるか。 */
  focusInControls: boolean;
  /** メニュー・シートが開いているか。 */
  menuOpen: boolean;
  /** 入力欄で文字を入れているか（右のパネルのタグの入力など）。 */
  typing: boolean;
}

/** いま操作を隠してよいか。 */
export function canHideControls(c: HideContext): boolean {
  return c.playing && !c.hovering && !c.focusInControls && !c.menuOpen && !c.typing;
}

export interface FullscreenPanels {
  fullscreen: boolean;
  panel: PlayerPanel;
  /** フルスクリーンに入る前のパネル（出たら戻す。フルスクリーンでないときは undefined）。 */
  panelBeforeFullscreen: PlayerPanel | undefined;
}

/** ウィンドウのフルスクリーンが `on` になったときのパネル。 */
export function onFullscreenChange(state: FullscreenPanels, on: boolean): FullscreenPanels {
  if (on === state.fullscreen) {
    return state;
  }
  if (on) {
    return { fullscreen: true, panel: null, panelBeforeFullscreen: state.panel };
  }
  return {
    fullscreen: false,
    panel: state.panelBeforeFullscreen === undefined ? state.panel : state.panelBeforeFullscreen,
    panelBeforeFullscreen: undefined,
  };
}
