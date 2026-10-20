import { render, screen, waitFor } from '@testing-library/react';
import { createElement } from 'react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { queryParse } from '../../ipc/search';
import { useViewStore } from '../../stores/viewStore';
import { EmptyView, emptyMessage } from './EmptyView';

vi.mock('../../ipc/search', () => ({
  // 解析で決まるフィールド（ここでは「透過」）を使うときだけ、未解析の数が来る
  queryParse: vi.fn(async (text: string) => ({
    error: null,
    clauses: [],
    counts: { image: 0, video: 0, audio: 0, font: 0, model: 0, total: 0 },
    unanalyzed: text.includes('透過:') ? 1234 : null,
  })),
}));

const t = ja.library.emptyView;

describe('emptyMessage', () => {
  it('一覧の種類ごとに言い分け、すべてはドロップと ⌘O の案内', () => {
    expect(emptyMessage({ kind: 'all' }, false)).toEqual({
      title: ja.library.emptyTitle,
      hint: ja.library.emptyHint,
    });
    expect(emptyMessage({ kind: 'recent' }, false).title).toBe(t.recent);
    expect(emptyMessage({ kind: 'untagged' }, false).title).toBe(t.untagged);
    expect(emptyMessage({ kind: 'smart', id: 3 }, false).hint).toBe(t.smartHint);
    expect(emptyMessage({ kind: 'random', seed: 1 }, false).title).toBe(t.other);
  });

  it('検索の条件が掛かっていれば、条件に合わないことを先に言う', () => {
    expect(emptyMessage({ kind: 'all' }, true)).toEqual({
      title: t.filtered,
      hint: t.filteredHint,
    });
  });

  it('解析がまだのものがあれば、解析が終わると一致しうることを言う', () => {
    expect(emptyMessage({ kind: 'all' }, true, 1234)).toEqual({
      title: t.filtered,
      hint: t.filteredUnanalyzed('1,234'),
    });
    expect(emptyMessage({ kind: 'all' }, true, 0).hint).toBe(t.filteredHint);
  });
});

describe('EmptyView', () => {
  beforeEach(() => {
    vi.mocked(queryParse).mockClear();
  });

  it('解析で決まる条件なら、未解析の数を出す', async () => {
    useViewStore.setState({ filter: ['透過:あり'] });
    render(createElement(EmptyView, { view: { kind: 'all' } }));
    expect(await screen.findByText(t.filteredUnanalyzed('1,234'))).toBeInTheDocument();
    expect(queryParse).toHaveBeenCalledWith('透過:あり', { kind: 'all' });
  });

  it('ほかの条件では今までどおりの案内', async () => {
    useViewStore.setState({ filter: ['評価>=4'] });
    render(createElement(EmptyView, { view: { kind: 'all' } }));
    await waitFor(() => expect(queryParse).toHaveBeenCalled());
    expect(screen.getByText(t.filteredHint)).toBeInTheDocument();
  });

  it('条件がなければ数えない', () => {
    useViewStore.setState({ filter: [] });
    render(createElement(EmptyView, { view: { kind: 'all' } }));
    expect(queryParse).not.toHaveBeenCalled();
  });
});
