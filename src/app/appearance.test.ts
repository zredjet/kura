import { describe, expect, it, vi } from 'vitest';

import { applyAppearance, syncAppearance } from './appearance';

describe('applyAppearance', () => {
  it('「透明度を下げる」がオンなら data-reduce-transparency を付け、オフなら外す', () => {
    const root = document.createElement('html');

    applyAppearance(root, { reduceTransparency: true });
    expect(root.getAttribute('data-reduce-transparency')).toBe('true');

    applyAppearance(root, { reduceTransparency: false });
    expect(root.hasAttribute('data-reduce-transparency')).toBe(false);
  });
});

describe('syncAppearance', () => {
  it('取得した設定を反映する', async () => {
    const root = document.createElement('html');
    await syncAppearance(root, () => Promise.resolve({ reduceTransparency: true }));
    expect(root.getAttribute('data-reduce-transparency')).toBe('true');
  });

  it('取得に失敗しても例外を投げず、属性を変えない', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const root = document.createElement('html');

    await expect(
      syncAppearance(root, () => Promise.reject(new Error('Tauri の外'))),
    ).resolves.toBeUndefined();
    expect(root.hasAttribute('data-reduce-transparency')).toBe(false);
    expect(warn).toHaveBeenCalledOnce();

    warn.mockRestore();
  });
});
