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

/** 操作（上部・再生バー・アニメの操作の行）の上にポインタがあるかを覚えるもの。 */
export interface HoverTracker {
  /** ポインタが部品に入った。 */
  enter(element: Element): void;
  /** ポインタが部品から出た（シアターを閉じた・一時停止したときも呼ぶ）。 */
  leave(): void;
  /** いま操作の上にあるか。入った部品が DOM から外れていれば（ポインタを乗せたまま行が消えた）ない。 */
  hovering(): boolean;
}

/**
 * ポインタの出入りを覚える。部品がポインタの下で DOM から外れると pointerleave が来ないので（アイテムを替えて
 * アニメの操作の行が消えたなど）、入った部品を持っておき、DOM に残っているときだけ乗っているとみなす。
 */
export function hoverTracker(): HoverTracker {
  let over: Element | null = null;
  return {
    enter: (element) => {
      over = element;
    },
    leave: () => {
      over = null;
    },
    hovering: () => over?.isConnected === true,
  };
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
