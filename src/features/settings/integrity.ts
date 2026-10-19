import { ja } from '../../i18n/ja';
import type { IntegrityDto } from '../../ipc/library';
import { formatCount } from '../../lib/format';

const t = ja.settings;

/**
 * 整合性チェックの結果の一行（設定のライブラリ）。ファイルがない項目・DB にないファイル・どの項目も使っていない
 * ファイル（台帳の行。M11）のうち 0 でないものを並べる。書きかけを片付けた数（`stagingRemoved`）は問題に数えない。
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
