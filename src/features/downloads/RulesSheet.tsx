import { useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { CloseIcon } from '../../components/icons/CloseIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { ja } from '../../i18n/ja';
import { type DownloadRuleDto, type ToolDto, toolCommand } from '../../ipc/downloads';
import { notify } from '../../stores/noticeStore';
import styles from './Downloads.module.css';

const t = ja.downloads.rules;

/** 編集中の 1 件（引数は空白区切りの文字で持つ）。 */
interface Draft {
  key: number;
  name: string;
  pattern: string;
  tool: ToolDto;
  args: string;
}

/** 空白で区切る（引用符で囲んだ部分は 1 つ）。 */
export function splitArgs(text: string): string[] {
  const out: string[] = [];
  const re = /"([^"]*)"|'([^']*)'|(\S+)/g;
  for (const m of text.matchAll(re)) {
    out.push(m[1] ?? m[2] ?? m[3] ?? '');
  }
  return out;
}

/** 引数を 1 行の文字にする（空白を含むものは引用符で囲む）。 */
export function joinArgs(args: readonly string[]): string {
  return args.map((a) => (/\s/.test(a) || a === '' ? `"${a}"` : a)).join(' ');
}

let nextKey = 1;

function toDraft(rule: DownloadRuleDto): Draft {
  return { key: nextKey++, ...rule, args: joinArgs(rule.args) };
}

/** 取得の規則を編集するシート（名前・URL の正規表現・ツール・追加の引数）。保存で確かめる。 */
export function RulesSheet({
  rules,
  onSave,
  onClose,
}: {
  rules: readonly DownloadRuleDto[];
  onSave: (rules: DownloadRuleDto[]) => Promise<unknown>;
  onClose: () => void;
}) {
  const id = useId();
  const [drafts, setDrafts] = useState<Draft[]>(() => rules.map(toDraft));
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const change = (key: number, patch: Partial<Draft>) =>
    setDrafts((list) => list.map((d) => (d.key === key ? { ...d, ...patch } : d)));

  const save = () => {
    setSaving(true);
    setError(null);
    const next = drafts.map((d) => ({
      name: d.name.trim(),
      pattern: d.pattern.trim(),
      tool: d.tool,
      args: splitArgs(d.args),
    }));
    onSave(next)
      .then(() => {
        notify(t.saved);
        onClose();
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setSaving(false));
  };

  return (
    <Dialog labelledBy={id} onClose={onClose}>
      <div className={styles.sheet}>
        <h2 id={id} className={styles.sheetTitle}>
          {t.title}
        </h2>
        <p className={styles.sheetText}>{t.hint}</p>
        <div className={styles.rules}>
          {drafts.map((d) => (
            <div key={d.key} className={styles.rule}>
              <label className={styles.field}>
                {t.name}
                <input
                  className={styles.input}
                  value={d.name}
                  onChange={(e) => change(d.key, { name: e.target.value })}
                />
              </label>
              <label className={styles.field}>
                {t.tool}
                <select
                  className={styles.select}
                  value={d.tool}
                  onChange={(e) => change(d.key, { tool: e.target.value as ToolDto })}
                >
                  {(['ytDlp', 'galleryDl'] as const).map((tool) => (
                    <option key={tool} value={tool}>
                      {toolCommand(tool)}
                    </option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                className={`${styles.toolMenu} ${styles.ruleRemove}`}
                aria-label={t.remove}
                onClick={() => setDrafts((list) => list.filter((x) => x.key !== d.key))}
              >
                <CloseIcon size={12} strokeWidth={2.4} />
              </button>
              <label className={`${styles.field} ${styles.fieldWide}`}>
                {t.pattern}
                <input
                  className={`${styles.input} ${styles.mono}`}
                  value={d.pattern}
                  spellCheck={false}
                  onChange={(e) => change(d.key, { pattern: e.target.value })}
                />
              </label>
              <label className={`${styles.field} ${styles.fieldWide}`}>
                {t.args}
                <input
                  className={`${styles.input} ${styles.mono}`}
                  value={d.args}
                  spellCheck={false}
                  onChange={(e) => change(d.key, { args: e.target.value })}
                />
              </label>
            </div>
          ))}
        </div>
        <div>
          <Button
            onClick={() =>
              setDrafts((list) => [
                ...list,
                toDraft({ name: t.newName, pattern: '', tool: 'galleryDl', args: [] }),
              ])
            }
          >
            <PlusIcon size={12} strokeWidth={2.4} />
            {t.add}
          </Button>
        </div>
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
        <div className={styles.sheetActions}>
          <Button onClick={onClose}>{t.cancel}</Button>
          <Button variant="primary" disabled={saving} onClick={save}>
            {t.save}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
