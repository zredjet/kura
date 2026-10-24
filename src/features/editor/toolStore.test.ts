import { describe, expect, it } from 'vitest';

import {
  DEFAULT_TOOLS,
  normalizeColor,
  parseSettings,
  pushRecent,
  RECENT_COLORS,
  useToolStore,
} from './toolStore';

const c = (r: number, index: number | null = null) => ({
  rgba: [r, 0, 0, 255] as [number, number, number, number],
  index,
});

describe('道具の設定', () => {
  it('最近の色は先頭に置き、同じ色は 1 つ、8 色まで', () => {
    let recent = [c(1), c(2)];
    recent = pushRecent(recent, c(2));
    expect(recent).toEqual([c(2), c(1)]);
    for (let i = 10; i < 20; i++) recent = pushRecent(recent, c(i));
    expect(recent).toHaveLength(RECENT_COLORS);
    expect(recent[0]).toEqual(c(19));
    // 番号が違えば別の色
    expect(pushRecent([c(5, 1)], c(5, 2))).toHaveLength(2);
  });

  it('保存した設定は項目ごとに確かめ、だめなものは既定にする', () => {
    expect(parseSettings(null)).toEqual(DEFAULT_TOOLS);
    expect(parseSettings('{')).toEqual(DEFAULT_TOOLS);
    const s = parseSettings(
      JSON.stringify({
        tool: 'fill',
        shape: 'triangle',
        size: 999,
        pixelPerfect: true,
        blend: 'replace',
        tolerance: -5,
        connectivity: 'eight',
        contiguous: false,
        color: { rgba: [300, -1, 10.6, 255], index: 3 },
        recent: [{ rgba: [1, 2, 3, 4] }, 'bad', { rgba: [1] }],
      }),
    );
    expect(s).toEqual({
      tool: 'fill',
      shape: DEFAULT_TOOLS.shape,
      size: 255,
      pixelPerfect: true,
      blend: 'replace',
      tolerance: 0,
      connectivity: 'eight',
      contiguous: false,
      color: { rgba: [255, 0, 11, 255], index: 3 },
      recent: [{ rgba: [1, 2, 3, 4], index: null }],
    });
    expect(normalizeColor({ rgba: 'x' })).toBeNull();
  });

  it('筆の大きさは 1〜255 に収める', () => {
    useToolStore.setState({ ...DEFAULT_TOOLS });
    useToolStore.getState().stepSize(-5);
    expect(useToolStore.getState().size).toBe(1);
    useToolStore.getState().setSize(1000);
    expect(useToolStore.getState().size).toBe(255);
    useToolStore.getState().rememberColor(c(9));
    expect(useToolStore.getState().recent).toEqual([c(9)]);
  });
});
