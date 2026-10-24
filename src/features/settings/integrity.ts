import { ja } from '../../i18n/ja';
import type { IntegrityDto } from '../../ipc/library';
import { formatCount } from '../../lib/format';

const t = ja.settings;

/**
 * 整合性チェックの結果の一行（設定のライブラリ）。ファイルがない項目・DB にないファイル・どの項目も使っていない
 * ファイル（台帳の行。M11）のうち 0 でないものを並べる。書きかけを片付けた数（`stagingRemoved`）と、GC が照合が合わずに
 * 残した版・7 日を過ぎた消し待ちの版（`keptVersions`・`overdueVersions`。M12 の段 3。表示は段 5）は問題に数えない。
 */
export function integritySummary(integrity: IntegrityDto): { healthy: boolean; text: string } {
  const issues = [
    [integrity.missingFiles, t.integrityMissing],
    [integrity.orphanFiles, t.integrityOrphans],
    [integrity.unreferencedBlobs, t.integrityUnreferenced],
  ] as const;
  const parts = issues.filter(([n]) => n > 0).map(([n, label]) => label(formatCount(n)));
  return parts.length === 0
    ? { healthy: true, text: t.integrityOk }
    : { healthy: false, text: parts.join('・') };
}
