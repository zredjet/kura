import { describe, expect, it, vi } from 'vitest';

import { applyAppearance, syncAppearance, watchAppearance } from './appearance';

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

describe('watchAppearance', () => {
  it('ウィンドウが前面に戻ったら読み直し、やめたら読まない', async () => {
    const root = document.createElement('html');
    let reduce = false;
    const fetch = vi.fn(() => Promise.resolve({ reduceTransparency: reduce }));
    const stop = watchAppearance(root, fetch);

    reduce = true;
    window.dispatchEvent(new Event('focus'));
    await vi.waitFor(() => expect(root.getAttribute('data-reduce-transparency')).toBe('true'));

    stop();
    reduce = false;
    window.dispatchEvent(new Event('focus'));
    await Promise.resolve();
    expect(fetch).toHaveBeenCalledOnce();
  });
});
