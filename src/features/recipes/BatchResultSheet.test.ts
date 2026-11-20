import { describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { groupNote } from './BatchResultSheet';

vi.mock('../../ipc/batch', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/batch')>()),
  batchRows: vi.fn(),
  batchResult: vi.fn(),
  undoBatch: vi.fn(),
}));

describe('結果の群の言葉', () => {
  it('1 通りならその言葉、何通りもあれば最初の言葉とほかの数', () => {
    expect(groupNote([])).toEqual({ first: null, note: null });
    expect(groupNote(['A', 'A', null])).toEqual({ first: 'A', note: 'A' });
    expect(groupNote(['A', 'B', 'C', 'A'])).toEqual({
      first: 'A',
      note: `A（${ja.batch.moreReasons('2')}）`,
    });
  });
});
