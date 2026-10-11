import { describe, expect, it, vi } from 'vitest';

vi.mock('../../ipc/playlists', () => ({}));

import { movedOrder } from './playlistActions';

describe('movedOrder', () => {
  it('対象の前・後ろへ移す', () => {
    expect(movedOrder([1, 2, 3, 4], 4, 2, 'before')).toEqual([1, 4, 2, 3]);
    expect(movedOrder([1, 2, 3, 4], 1, 3, 'after')).toEqual([2, 3, 1, 4]);
    expect(movedOrder([1, 2, 3], 1, 3, 'after')).toEqual([2, 3, 1]);
  });

  it('自分自身や、並びにないものなら元のまま', () => {
    expect(movedOrder([1, 2, 3], 2, 2, 'before')).toEqual([1, 2, 3]);
    expect(movedOrder([1, 2, 3], 9, 2, 'before')).toEqual([1, 2, 3]);
    expect(movedOrder([1, 2, 3], 1, 9, 'after')).toEqual([1, 2, 3]);
  });
});
