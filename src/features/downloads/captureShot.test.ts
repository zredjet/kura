import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import type { SnapshotResultDto } from '../../ipc/capture';
import { snapshotMessage } from './captureShot';

const t = ja.downloads.capture;

const result = (patch: Partial<SnapshotResultDto> = {}): SnapshotResultDto => ({
  title: 'ページ',
  url: 'https://example.com/',
  itemIds: [1],
  added: 1,
  ruleName: null,
  truncated: false,
  ...patch,
});

describe('snapshotMessage', () => {
  it('ルールなし・ルールあり・重複を言い分ける', () => {
    expect(snapshotMessage(result())).toBe(t.taken('ページ'));
    expect(snapshotMessage(result({ ruleName: 'pixiv' }))).toBe(t.takenWithRule('ページ', 'pixiv'));
    expect(snapshotMessage(result({ added: 0, ruleName: 'pixiv' }))).toBe(
      t.takenDuplicate('ページ'),
    );
  });

  it('途中までのときは書き添える', () => {
    expect(snapshotMessage(result({ truncated: true }))).toBe(
      `${t.taken('ページ')}${t.takenTruncated}`,
    );
  });
});
