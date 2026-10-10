import { describe, expect, it } from 'vitest';

import type { ItemSummaryDto } from '../../ipc/items';
import { itemMeta } from './describeItem';

const base: ItemSummaryDto = {
  id: 1,
  hash: 'ab',
  ext: 'jpg',
  kind: 'image',
  width: 6000,
  height: 4000,
  durationMs: null,
  sizeBytes: 8_400_000,
  title: '夕方の海',
  rating: 4,
  sourceSite: 'local',
  importedAt: 0,
  capturedAt: null,
  thumbState: 'ready',
  proxyState: 'not_needed',
};

describe('itemMeta', () => {
  it('モックと同じ並び', () => {
    expect(itemMeta(base)).toBe('画像 · JPG · 6000×4000 · 8.4 MB');
    expect(
      itemMeta({
        ...base,
        kind: 'video',
        ext: 'mp4',
        width: 3840,
        height: 2160,
        durationMs: 4_360_000,
        sizeBytes: 14_200_000_000,
      }),
    ).toBe('動画 · MP4 · 3840×2160 · 1:12:40 · 14.2 GB');
    expect(
      itemMeta({
        ...base,
        kind: 'audio',
        ext: 'flac',
        width: null,
        height: null,
        durationMs: 258_000,
        sizeBytes: 98_000_000,
      }),
    ).toBe('音楽 · FLAC · 4:18 · 98.0 MB');
  });
});
