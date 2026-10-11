import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../../i18n/ja';
import type { UnitDetailDto, UnitPatchDto } from '../../../ipc/units';
import { UnitInspector } from './UnitInspector';

const stored = { orderReverse: false, innerShuffle: false };

function detail(): UnitDetailDto {
  return {
    unit: {
      id: 1,
      kind: 'set',
      name: '夜の素材',
      cover: null,
      count: 2,
      durationMs: 16000,
      kinds: ['image'],
      rating: 0,
      ownRating: 0,
      origin: 'user',
      createdAt: 0,
      lastPlayedAt: null,
    },
    orderMode: 'manual',
    orderReverse: stored.orderReverse,
    innerShuffle: stored.innerShuffle,
    imageDurationMs: null,
    note: null,
    sourceUrl: null,
    tags: [],
    folders: [],
    playCount: 0,
    lastPlayedAt: null,
  };
}

vi.mock(import('../../../ipc/units'), async (importOriginal) => ({
  ...(await importOriginal()),
  getUnitDetail: vi.fn(async () => detail()),
  updateUnit: vi.fn(async (_id: number, patch: UnitPatchDto) => {
    stored.orderReverse = patch.orderReverse ?? stored.orderReverse;
    stored.innerShuffle = patch.innerShuffle ?? stored.innerShuffle;
    return null;
  }),
}));
vi.mock(import('../../../ipc/items'), async (importOriginal) => ({
  ...(await importOriginal()),
  queryIds: vi.fn(async () => ({
    ids: new Uint32Array([1, 2]),
    aspects: new Uint16Array([1000, 1000]),
    groups: [],
  })),
  selectionInfo: vi.fn(async () => ({
    count: 2,
    rating: 0,
    sameNote: true,
    note: null,
    tags: [],
    folders: [],
  })),
}));
vi.mock('../../../stores/sidebarStore', () => ({
  useSidebarStore: { getState: () => ({ load: async () => undefined, folders: [] }) },
}));

describe('UnitInspector', () => {
  beforeEach(() => {
    stored.orderReverse = false;
    stored.innerShuffle = false;
  });

  it('逆順・ユニット内シャッフルのスイッチは、切り替えたあとの値を出す', async () => {
    render(<UnitInspector unitId={1} opened />);
    const reverse = await screen.findByRole('switch', { name: ja.unitInspector.orderReverse });
    expect(reverse).not.toBeChecked();

    fireEvent.click(reverse);
    await waitFor(() => expect(stored.orderReverse).toBe(true));
    await waitFor(() => expect(reverse).toBeChecked());
    // 見た目（つまみの位置と色）は data-checked で決まる
    expect(reverse.parentElement).toHaveAttribute('data-checked', 'true');

    const shuffle = screen.getByRole('switch', { name: ja.unitInspector.innerShuffle });
    fireEvent.click(shuffle);
    await waitFor(() => expect(stored.innerShuffle).toBe(true));
    await waitFor(() => expect(shuffle).toBeChecked());
    expect(shuffle.parentElement).toHaveAttribute('data-checked', 'true');
  });
});
