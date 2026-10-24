import { ja } from '../../i18n/ja';
import type { IntegrityDto } from '../../ipc/library';
import { formatCount } from '../../lib/format';

const t = ja.settings;

/**
 * 整合性チェックの結果の一行（設定のライブラリ）。ファイルがない項目・DB にないファイル・どの項目も使っていない
 * ファイル（台帳の行。M11）のうち 0 でないものを並べる。書きかけを片付けた数（`stagingRemoved`）は数えない。GC が照合が合わずに
 * 残した版・7 日を過ぎた消し待ちの版（`keptVersions`・`overdueVersions`。M12 の段 3。「いま確かめる」ときだけ数え、起動時は
 * null）は問題に数えず、0 でなければ `notes` に件数を並べる（「照合が合わずに残した版 2・7 日を過ぎた消し待ちの版 3（問題では
 * ありません）」）。
 */
export function integritySummary(integrity: IntegrityDto): {
  healthy: boolean;
  text: string;
  notes: string | null;
} {
  const issues = [
    [integrity.missingFiles, t.integrityMissing],
    [integrity.orphanFiles, t.integrityOrphans],
    [integrity.unreferencedBlobs, t.integrityUnreferenced],
  ] as const;
  const parts = issues.filter(([n]) => n > 0).map(([n, label]) => label(formatCount(n)));
  const counted = [
    [integrity.keptVersions, t.integrityKept],
    [integrity.overdueVersions, t.integrityOverdue],
  ] as const;
  const noteParts = counted
    .filter((entry): entry is readonly [number, (n: string) => string] => (entry[0] ?? 0) > 0)
    .map(([n, label]) => label(formatCount(n)));
  const notes = noteParts.length > 0 ? t.integrityNotes(noteParts.join('・')) : null;
  return parts.length === 0
    ? { healthy: true, text: t.integrityOk, notes }
    : { healthy: false, text: parts.join('・'), notes };
}
