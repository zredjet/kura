import { act, fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { Menu } from './Menu';
import { openMenuAt, useMenuStore } from './menuStore';

describe('Menu', () => {
  it('focuses the first item, moves with arrow keys and closes with escape', async () => {
    const first = vi.fn();
    const second = vi.fn();
    render(<Menu />);
    act(() =>
      openMenuAt(10, 10, '項目の操作', [
        { label: '一つ目', onSelect: first },
        'separator',
        { label: '無効', disabled: true, onSelect: vi.fn() },
        { label: '二つ目', shortcut: '⌘G', onSelect: second },
      ]),
    );

    const items = await screen.findAllByRole('menuitem');
    expect(document.activeElement).toBe(items[0]);
    fireEvent.keyDown(items[0] as HTMLElement, { key: 'ArrowDown' });
    // 無効の項目は飛ばす
    expect(document.activeElement).toBe(screen.getByRole('menuitem', { name: /二つ目/ }));

    fireEvent.click(screen.getByRole('menuitem', { name: /二つ目/ }));
    expect(second).toHaveBeenCalledOnce();
    expect(useMenuStore.getState().menu).toBeNull();

    act(() => openMenuAt(0, 0, '項目の操作', [{ label: '一つ目', onSelect: first }]));
    fireEvent.keyDown(await screen.findByRole('menu'), { key: 'Escape' });
    expect(useMenuStore.getState().menu).toBeNull();
    expect(first).not.toHaveBeenCalled();
  });

  it('▸ の項目は横に開き、→ で入って ← と esc で戻る。選んだら全体を閉じる', async () => {
    const yes = vi.fn();
    const auto = vi.fn();
    render(<Menu />);
    act(() =>
      openMenuAt(10, 10, '項目の操作', [
        { label: '再生', onSelect: vi.fn() },
        {
          label: 'ドット絵として表示',
          submenu: [
            { label: '自動', checked: true, onSelect: auto },
            { label: 'はい', checked: false, onSelect: yes },
            { label: 'いいえ', checked: false, onSelect: vi.fn() },
          ],
        },
      ]),
    );
    const parent = await screen.findByRole('menuitem', { name: /ドット絵として表示/ });
    expect(parent).toHaveAttribute('aria-haspopup', 'menu');
    expect(parent).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('menuitemradio')).toBeNull();

    // → で開いて最初の選択肢へ。チェックはそろっている値に付く
    parent.focus();
    fireEvent.keyDown(parent, { key: 'ArrowRight' });
    const choices = await screen.findAllByRole('menuitemradio');
    expect(choices.map((c) => c.getAttribute('aria-checked'))).toEqual(['true', 'false', 'false']);
    expect(parent).toHaveAttribute('aria-expanded', 'true');
    expect(document.activeElement).toBe(choices[0]);
    // ↑↓ は開いた中だけを回る
    fireEvent.keyDown(choices[0] as HTMLElement, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(choices[1]);
    fireEvent.keyDown(choices[1] as HTMLElement, { key: 'ArrowUp' });
    fireEvent.keyDown(choices[0] as HTMLElement, { key: 'ArrowUp' });
    expect(document.activeElement).toBe(choices[2]);
    // ← で閉じて元の行へ（メニューは開いたまま）
    fireEvent.keyDown(choices[2] as HTMLElement, { key: 'ArrowLeft' });
    expect(screen.queryByRole('menuitemradio')).toBeNull();
    expect(document.activeElement).toBe(parent);
    expect(useMenuStore.getState().menu).not.toBeNull();
    // esc も開いた中だけを閉じる
    fireEvent.click(parent);
    const again = await screen.findAllByRole('menuitemradio');
    fireEvent.keyDown(again[0] as HTMLElement, { key: 'Escape' });
    expect(screen.queryByRole('menuitemradio')).toBeNull();
    expect(useMenuStore.getState().menu).not.toBeNull();
    // ポインタを載せても開き、選ぶと全体を閉じる
    fireEvent.pointerEnter(parent);
    fireEvent.click(await screen.findByRole('menuitemradio', { name: /はい/ }));
    expect(yes).toHaveBeenCalledOnce();
    expect(auto).not.toHaveBeenCalled();
    expect(useMenuStore.getState().menu).toBeNull();
  });
});
