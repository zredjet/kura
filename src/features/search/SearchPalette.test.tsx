import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { useSearchStore } from '../../stores/searchStore';
import { useViewStore } from '../../stores/viewStore';
import { SearchPalette } from './SearchPalette';

vi.mock('../../ipc/search', () => ({
  // 「(」があればエラー、なければ空白で区切ってチップにする（本物の解析は Rust）
  queryParse: vi.fn(async (text: string) =>
    text.includes('(')
      ? {
          error: {
            start: text.indexOf('('),
            end: text.indexOf('(') + 1,
            message: 'かっこが閉じていません',
          },
          clauses: [],
          counts: null,
        }
      : {
          error: null,
          clauses: text
            .split(' ')
            .filter(Boolean)
            .map((t, i) => ({
              start: i,
              end: i + 1,
              text: t,
              negated: t.startsWith('-'),
              field: null,
            })),
          counts: { image: 3, video: 1, audio: 0, font: 0, model: 0, total: 4 },
          // 解析で決まるフィールド（ここでは「透過」）を使うときだけ数が来る（0 もある）
          unanalyzed: text.includes('透過:') ? (text.includes('種類:音楽') ? 0 : 1234) : null,
        },
  ),
  querySuggest: vi.fn(async (word: string) =>
    word === '作者:も'
      ? [
          {
            kind: 'tag',
            insert: '作者:もりの',
            prefix: '作者:',
            label: 'もりの',
            detail: null,
            count: 312,
            id: null,
          },
        ]
      : [],
  ),
  saveSmartFolder: vi.fn(),
  updateSmartFolder: vi.fn(),
  deleteSmartFolder: vi.fn(),
  smartFolders: vi.fn(async () => []),
}));

function input(): HTMLInputElement {
  return screen.getByRole('combobox', { name: ja.search.inputLabel }) as HTMLInputElement;
}

function type(text: string) {
  fireEvent.change(input(), { target: { value: text } });
}

describe('SearchPalette', () => {
  beforeEach(() => {
    useViewStore.setState({ view: { kind: 'all' }, filter: [] });
    act(() => useSearchStore.getState().openPalette());
  });

  it('turns words into chips and applies them with return', async () => {
    render(<SearchPalette />);
    type('夕景');
    fireEvent.keyDown(input(), { key: ' ' });
    expect(screen.getByText('夕景')).toBeInTheDocument();
    expect(input().value).toBe('');

    type('-未整理');
    await waitFor(() => expect(screen.getByText('4')).toBeInTheDocument());
    expect(screen.getByText(/画像 3 · 動画 1/)).toBeInTheDocument();
    fireEvent.keyDown(input(), { key: 'Enter' });
    await waitFor(() => expect(useSearchStore.getState().open).toBe(false));
    expect(useViewStore.getState().filter).toEqual(['夕景', '-未整理']);
  });

  it('removes the last chip with backspace and shows parse errors', async () => {
    render(<SearchPalette />);
    type('海');
    fireEvent.keyDown(input(), { key: ' ' });
    fireEvent.keyDown(input(), { key: 'Backspace' });
    expect(screen.queryByText('海')).not.toBeInTheDocument();

    type('(海');
    expect(await screen.findByRole('alert')).toHaveTextContent('かっこが閉じていません');
    // エラーがあれば閉じない
    fireEvent.keyDown(input(), { key: 'Enter' });
    await waitFor(() => expect(useSearchStore.getState().open).toBe(true));
  });

  it('accepts a suggestion with tab', async () => {
    render(<SearchPalette />);
    type('作者:も');
    expect(await screen.findByText('もりの')).toBeInTheDocument();
    fireEvent.keyDown(input(), { key: 'Tab' });
    expect(screen.getByText('作者:もりの')).toBeInTheDocument();
    expect(input().value).toBe('');
  });

  it('shows how many items are not analyzed yet when asset fields are used', async () => {
    render(<SearchPalette />);
    type('夕景');
    await waitFor(() => expect(screen.getByText('4')).toBeInTheDocument());
    expect(screen.queryByText(ja.search.unanalyzed('1,234'))).not.toBeInTheDocument();

    type('透過:あり');
    expect(await screen.findByText(ja.search.unanalyzed('1,234'))).toBeInTheDocument();

    // 0 件なら出さない
    type('種類:音楽 透過:あり');
    await waitFor(() =>
      expect(screen.queryByText(ja.search.unanalyzed('1,234'))).not.toBeInTheDocument(),
    );
    expect(screen.queryByText(ja.search.unanalyzed('0'))).not.toBeInTheDocument();
  });

  it('closes with escape', () => {
    render(<SearchPalette />);
    fireEvent.keyDown(input(), { key: 'Escape' });
    expect(useSearchStore.getState().open).toBe(false);
  });
});
