import { describe, expect, it } from 'vitest';

import type { EntryRowDto } from '../../ipc/playlists';
import { listRows } from './rows';

function row(entryId: number, target: EntryRowDto['target'], itemCount: number): EntryRowDto {
  return {
    entryId,
    target,
    name: `e${entryId}`,
    itemCount,
    durationMs: 0,
    kinds: [],
    coverItemId: null,
    imageDurationMs: null,
  };
}

const contents = {
  rows: [
    row(1, { kind: 'item', id: 10 }, 1),
    row(2, { kind: 'unit', id: 5 }, 2),
    row(3, { kind: 'folder', id: 7, recursive: true }, 0),
    row(4, { kind: 'item', id: 13 }, 1),
  ],
  flatItemIds: [10, 11, 12, 13],
};

describe('listRows', () => {
  it('ファイル単位は平らに、展開したエントリの前に見出し。空のエントリは出さない', () => {
    expect(listRows(contents, 'file', new Set())).toEqual([
      { kind: 'item', entry: 0, itemId: 10, indent: false },
      { kind: 'header', entry: 1 },
      { kind: 'item', entry: 1, itemId: 11, indent: false },
      { kind: 'item', entry: 1, itemId: 12, indent: false },
      { kind: 'item', entry: 3, itemId: 13, indent: false },
    ]);
  });

  it('ユニット単位はエントリごと、開いたものだけ中身を下げて出す', () => {
    expect(listRows(contents, 'unit', new Set([2]))).toEqual([
      { kind: 'entry', entry: 0 },
      { kind: 'entry', entry: 1 },
      { kind: 'item', entry: 1, itemId: 11, indent: true },
      { kind: 'item', entry: 1, itemId: 12, indent: true },
      { kind: 'entry', entry: 2 },
      { kind: 'entry', entry: 3 },
    ]);
  });
});
