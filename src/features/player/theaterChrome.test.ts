import { describe, expect, it } from 'vitest';

import { canHideControls, hoverTracker, onFullscreenChange } from './theaterChrome';

const base = {
  playing: true,
  hovering: false,
  focusInControls: false,
  menuOpen: false,
  typing: false,
};

describe('canHideControls', () => {
  it('再生中で、操作に触れていなければ隠す', () => {
    expect(canHideControls(base)).toBe(true);
  });

  it('一時停止中・ポインタやフォーカスが操作の上・メニューやシート・文字の入力中は隠さない', () => {
    expect(canHideControls({ ...base, playing: false })).toBe(false);
    expect(canHideControls({ ...base, hovering: true })).toBe(false);
    expect(canHideControls({ ...base, focusInControls: true })).toBe(false);
    expect(canHideControls({ ...base, menuOpen: true })).toBe(false);
    expect(canHideControls({ ...base, typing: true })).toBe(false);
  });
});

describe('hoverTracker', () => {
  it('入ったら乗っていて、出たら乗っていない', () => {
    const row = document.body.appendChild(document.createElement('div'));
    const hover = hoverTracker();
    expect(hover.hovering()).toBe(false);
    hover.enter(row);
    expect(hover.hovering()).toBe(true);
    hover.leave();
    expect(hover.hovering()).toBe(false);
    row.remove();
  });

  it('乗せたまま部品が DOM から外れたら（pointerleave は来ない）乗っていないとみなす', () => {
    const row = document.body.appendChild(document.createElement('div'));
    const hover = hoverTracker();
    hover.enter(row);
    row.remove();
    expect(hover.hovering()).toBe(false);
  });
});

describe('onFullscreenChange', () => {
  const windowed = { fullscreen: false, panel: 'queue' as const, panelBeforeFullscreen: undefined };

  it('入るとパネルを閉じ、出ると入る前のパネルに戻す', () => {
    const inside = onFullscreenChange(windowed, true);
    expect(inside).toEqual({ fullscreen: true, panel: null, panelBeforeFullscreen: 'queue' });
    // フルスクリーン中に情報を開いても、出たら入る前の状態
    const opened = { ...inside, panel: 'info' as const };
    expect(onFullscreenChange(opened, false)).toEqual(windowed);
  });

  it('同じ状態の知らせは無視する', () => {
    expect(onFullscreenChange(windowed, false)).toBe(windowed);
  });
});
