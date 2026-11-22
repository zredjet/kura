import { describe, expect, it } from 'vitest';

import type { EntryRowDto } from '../../ipc/playlists';
import { enterRow, type ListRow, listRows, playFromOf } from './rows';

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

describe('playFromOf', () => {
  const withCover = {
    rows: [
      { ...row(1, { kind: 'item', id: 10 }, 1), coverItemId: 10 },
      { ...row(2, { kind: 'unit', id: 5 }, 2), coverItemId: 11 },
      row(3, { kind: 'folder', id: 7, recursive: true }, 0),
    ],
  };

  it('アイテムの行はそのアイテム、エントリの行・見出しは先頭のメンバーを、どれも行のエントリと渡す', () => {
    expect(playFromOf({ kind: 'item', entry: 1, itemId: 12, indent: true }, withCover)).toEqual({
      itemId: 12,
      entryId: 2,
    });
    expect(playFromOf({ kind: 'entry', entry: 1 }, withCover)).toEqual({ itemId: 11, entryId: 2 });
    expect(playFromOf({ kind: 'header', entry: 1 }, withCover)).toEqual({ itemId: 11, entryId: 2 });
    expect(playFromOf({ kind: 'item', entry: 0, itemId: 10, indent: false }, withCover)).toEqual({
      itemId: 10,
      entryId: 1,
    });
  });

  it('メンバーのないエントリ・知らない行は渡さない（先頭から）', () => {
    expect(playFromOf({ kind: 'entry', entry: 2 }, withCover)).toBeUndefined();
    expect(playFromOf({ kind: 'entry', entry: 9 }, withCover)).toBeUndefined();
  });
});

describe('enterRow', () => {
  // エントリ 0（単体）・エントリ 1（開いた中身 2 行）・エントリ 2
  const rows: ListRow[] = [
    { kind: 'entry', entry: 0 },
    { kind: 'entry', entry: 1 },
    { kind: 'item', entry: 1, itemId: 11, indent: true },
    { kind: 'item', entry: 1, itemId: 12, indent: true },
    { kind: 'entry', entry: 2 },
  ];
  const selectedEntries = (entries: number[]) => (row: ListRow) => entries.includes(row.entry);

  it('フォーカスのある行が選んだエントリの行なら、その行（先頭のメンバーでない行も。M11 の 3 回目の確認 P1）', () => {
    expect(enterRow(rows, rows[3], selectedEntries([1]))).toBe(rows[3]);
    expect(enterRow(rows, rows[4], selectedEntries([1, 2]))).toBe(rows[4]);
  });

  it('フォーカスが行の外・選択の外なら、選んだエントリの最初の行。選んでいなければ undefined', () => {
    expect(enterRow(rows, undefined, selectedEntries([1]))).toBe(rows[1]);
    expect(enterRow(rows, rows[0], selectedEntries([1, 2]))).toBe(rows[1]);
    expect(enterRow(rows, rows[2], selectedEntries([]))).toBeUndefined();
  });
});
