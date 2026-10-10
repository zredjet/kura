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
});
