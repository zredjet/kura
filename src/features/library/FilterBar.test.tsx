import { fireEvent, render, screen, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { useViewStore } from '../../stores/viewStore';
import { FilterBar } from './FilterBar';

vi.mock('../../ipc/search', () => ({
  querySuggest: vi.fn(async () => []),
}));

function segments(): HTMLElement[] {
  return within(screen.getByRole('group', { name: ja.filterBar.kinds })).getAllByRole('button');
}

beforeEach(() => {
  useViewStore.setState({ filter: [] });
});

describe('フィルタバーの種類（design.md §3.1）', () => {
  it('すべて・画像・動画・音楽・フォント・3D の 6 区切りで、どれにもアイコンがある', () => {
    render(<FilterBar />);
    const buttons = segments();
    expect(buttons.map((b) => b.textContent)).toEqual([
      'すべて',
      '画像',
      '動画',
      '音楽',
      'フォント',
      '3D',
    ]);
    for (const b of buttons.slice(1)) {
      expect(b.querySelector('svg')).not.toBeNull();
    }
    expect(buttons[0]).toHaveAttribute('aria-pressed', 'true');
  });

  it('押すと検索の語（ラベルではなく）で種類の条件を置き、もう一度押すと外す', () => {
    render(<FilterBar />);
    fireEvent.click(screen.getByRole('button', { name: 'フォント' }));
    expect(useViewStore.getState().filter).toEqual(['種類:フォント']);
    expect(screen.getByRole('button', { name: 'フォント' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    expect(screen.getByRole('button', { name: 'すべて' })).toHaveAttribute('aria-pressed', 'false');

    fireEvent.click(screen.getByRole('button', { name: '3D' }));
    expect(useViewStore.getState().filter).toEqual(['種類:3D']);

    fireEvent.click(screen.getByRole('button', { name: '3D' }));
    expect(useViewStore.getState().filter).toEqual([]);
  });

  it('⌘K で書いた種類の条件（英語・小文字）も、その区切りが押された形で出す', () => {
    useViewStore.setState({ filter: ['type:model'] });
    render(<FilterBar />);
    expect(screen.getByRole('button', { name: '3D' })).toHaveAttribute('aria-pressed', 'true');
    // 種類の条件はセグメントで表すので、別のチップにしない
    expect(screen.queryByText('type:model')).toBeNull();
  });

  it('ほかの条件は残したまま種類だけを差し替える', () => {
    useViewStore.setState({ filter: ['種類:画像', '評価>=4'] });
    render(<FilterBar />);
    fireEvent.click(screen.getByRole('button', { name: '音楽' }));
    expect([...useViewStore.getState().filter].sort()).toEqual(['種類:音楽', '評価>=4'].sort());
  });
});
