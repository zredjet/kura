import { fireEvent, render, screen } from '@testing-library/react';
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { TagListEntryDto } from '../../ipc/tags';
import { useViewStore } from '../../stores/viewStore';
import { TagsView, usageText } from './TagsView';

const entry = (
  over: Partial<TagListEntryDto> & { name: string; namespace?: string },
): TagListEntryDto => ({
  tag: { id: over.name.length, namespace: over.namespace ?? '', name: over.name, color: null },
  items: over.items ?? 0,
  auto: over.auto ?? 0,
  units: over.units ?? 0,
  clause: over.clause ?? over.name,
});

vi.mock('../../ipc/tags', () => ({
  listTags: vi.fn(async () => [
    entry({ namespace: '作者', name: 'KR', items: 3, auto: 2, clause: '作者:KR' }),
    entry({ name: '夕景', items: 1 }),
  ]),
}));

const t = ja.tagsView;

describe('usageText', () => {
  it('件数と自動・手動、ユニット。どこにも付いていなければそう言う', () => {
    expect(usageText(entry({ name: 'a', items: 128, auto: 120, units: 2 }))).toBe(
      `${ja.library.itemCount('128')} · ${t.sources('120', '8')} · ${t.units('2')}`,
    );
    expect(usageText(entry({ name: 'a', units: 1 }))).toBe(t.units('1'));
    expect(usageText(entry({ name: 'a' }))).toBe(t.unused);
  });
});

describe('TagsView', () => {
  // jsdom は大きさを測れないので、仮想スクロールが行を描けるよう大きさを与える
  const height = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetHeight');
  const width = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'offsetWidth');
  beforeAll(() => {
    Object.defineProperty(HTMLElement.prototype, 'offsetHeight', {
      configurable: true,
      value: 600,
    });
    Object.defineProperty(HTMLElement.prototype, 'offsetWidth', { configurable: true, value: 800 });
  });
  afterAll(() => {
    if (height) {
      Object.defineProperty(HTMLElement.prototype, 'offsetHeight', height);
    }
    if (width) {
      Object.defineProperty(HTMLElement.prototype, 'offsetWidth', width);
    }
  });

  beforeEach(() => {
    useViewStore.setState({ tags: true, view: { kind: 'all' }, filter: [] });
  });

  it('名前空間ごとに並べ、押すとそのタグですべての一覧を絞り込む', async () => {
    render(<TagsView />);
    expect(await screen.findByText(t.count('2'))).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /^KR、/ }));
    expect(useViewStore.getState()).toMatchObject({
      tags: false,
      view: { kind: 'all' },
      filter: ['作者:KR'],
    });
  });

  it('絞り込みに合うものがなければ、そう言う', async () => {
    render(<TagsView />);
    await screen.findByText(t.count('2'));
    fireEvent.change(screen.getByLabelText(t.filterLabel), { target: { value: 'ない' } });
    expect(screen.getByText(t.noMatch('ない'))).toBeInTheDocument();
  });
});
