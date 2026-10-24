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
    expect(integritySummary(clean)).toEqual({ healthy: true, text: ja.settings.integrityOk });
  });

  it('GC が残した版・7 日を過ぎた消し待ちの版（M12）は問題に数えない', () => {
    expect(integritySummary({ ...clean, keptVersions: 2, overdueVersions: 3 })).toEqual({
      healthy: true,
      text: ja.settings.integrityOk,
    });
  });

  it('どの項目も使っていない実ファイル（台帳の行。M11）も問題として数える', () => {
    expect(integritySummary({ ...clean, unreferencedBlobs: 2 })).toEqual({
      healthy: false,
      text: 'どの項目も使っていないファイル 2',
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
