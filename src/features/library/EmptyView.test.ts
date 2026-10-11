import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import { emptyMessage } from './EmptyView';

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
});
