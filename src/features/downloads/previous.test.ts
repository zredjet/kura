import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import type { PreviousDownloadDto } from '../../ipc/downloads';
import { previousMessage } from './previous';

const t = ja.downloads.previous;

const previous = (patch: Partial<PreviousDownloadDto>): PreviousDownloadDto => ({
  jobId: 1,
  title: '夏祭りダイジェスト',
  at: new Date(2026, 9, 11, 21, 2).getTime(),
  items: 1,
  inLibrary: 0,
  ...patch,
});

const now = new Date(2026, 9, 12, 10, 0).getTime();

describe('previousMessage', () => {
  it('tells when it was downloaded and whether it is still in the library', () => {
    expect(previousMessage(previous({}), now)).toBe(
      `「夏祭りダイジェスト」は 昨日 21:02 にダウンロードしています。${t.noneInLibrary}\n${t.explain}`,
    );
    expect(previousMessage(previous({ inLibrary: 1 }), now)).toContain(t.allInLibrary);
    expect(previousMessage(previous({ items: 10, inLibrary: 3 }), now)).toContain(
      '取得した 10 件のうち 3 件がライブラリにあります。',
    );
    expect(previousMessage(previous({ title: null }), now)).toMatch(
      /^この URL は 昨日 21:02 にダウンロードしています。/,
    );
  });

  it('only says it was already fetched when the job just skipped', () => {
    expect(previousMessage(previous({ items: 0 }), now)).toBe(
      `「夏祭りダイジェスト」は取得済みです。\n${t.explain}`,
    );
    expect(previousMessage(previous({ items: 0, title: null }), now)).toBe(
      `${t.skippedOnlyUntitled}\n${t.explain}`,
    );
  });
});
