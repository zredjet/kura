import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import { integritySummary } from './integrity';

const clean = {
  stagingRemoved: 4,
  missingFiles: 0,
  orphanFiles: 0,
  unreferencedBlobs: 0,
  keptVersions: null,
  overdueVersions: null,
};

describe('整合性チェックの結果', () => {
  it('どれも 0 なら一致している（書きかけを片付けた数は問題に数えない）', () => {
    expect(integritySummary(clean)).toEqual({
      healthy: true,
      text: ja.settings.integrityOk,
      notes: null,
    });
  });

  it('GC が残した版・7 日を過ぎた消し待ちの版（M12）は問題に数えず、件数だけを添える', () => {
    expect(integritySummary({ ...clean, keptVersions: 2, overdueVersions: 1234 })).toEqual({
      healthy: true,
      text: ja.settings.integrityOk,
      notes: '照合が合わずに残した版 2・7 日を過ぎた消し待ちの版 1,234（問題ではありません）',
    });
    expect(integritySummary({ ...clean, keptVersions: 0, overdueVersions: 3 }).notes).toBe(
      '7 日を過ぎた消し待ちの版 3（問題ではありません）',
    );
    // 起動時のチェック（数えていない）・0 件なら添えない
    expect(integritySummary({ ...clean, keptVersions: 0, overdueVersions: 0 }).notes).toBeNull();
    expect(integritySummary({ ...clean, missingFiles: 1, keptVersions: 2 })).toEqual({
      healthy: false,
      text: 'ファイルがない項目 1',
      notes: '照合が合わずに残した版 2（問題ではありません）',
    });
  });

  it('どの項目も使っていない実ファイル（台帳の行。M11）も問題として数える', () => {
    expect(integritySummary({ ...clean, unreferencedBlobs: 2 })).toEqual({
      healthy: false,
      text: 'どの項目も使っていないファイル 2',
      notes: null,
    });
  });

  it('0 でないものだけを並べる', () => {
    expect(integritySummary({ ...clean, missingFiles: 1, orphanFiles: 1234 }).text).toBe(
      'ファイルがない項目 1・DB にないファイル 1,234',
    );
    expect(
      integritySummary({ ...clean, missingFiles: 3, orphanFiles: 0, unreferencedBlobs: 5 }).text,
    ).toBe('ファイルがない項目 3・どの項目も使っていないファイル 5');
  });
});
