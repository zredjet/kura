import { useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { askConfirm } from '../../components/Overlay/overlayStore';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import {
  type CaptureModeDto,
  type CaptureRuleDto,
  type CaptureRuleInputDto,
  deleteCaptureRule,
  saveCaptureRule,
} from '../../ipc/capture';
import { notify } from '../../stores/noticeStore';
import { destLabel, fail, pickDestFolder } from './downloadActions';
import styles from './Downloads.module.css';

const t = ja.downloads.capture;
const s = t.sheet;

const MODES: CaptureModeDto[] = ['element', 'fullPage', 'viewport'];

/** 新しいルールの初期値（Downloads.dc.html の例に合わせる）。 */
export function newRuleInput(): CaptureRuleInputDto {
  return {
    name: s.newName,
    urlPattern: 'https://',
    mode: 'fullPage',
    targetSelector: null,
    hideSelectors: [],
    viewportWidth: 1200,
    scrollTimes: 0,
    waitMs: 1500,
    destFolderId: null,
    enabled: true,
  };
}

/** 「header, .ad」→ ["header", ".ad"]（括弧の中のカンマでは分けない）。 */
export function splitSelectors(text: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let current = '';
  for (const ch of text) {
    if (ch === '(' || ch === '[') {
      depth += 1;
    } else if ((ch === ')' || ch === ']') && depth > 0) {
      depth -= 1;
    }
    if (ch === ',' && depth === 0) {
      out.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  out.push(current);
  return out.map((x) => x.trim()).filter(Boolean);
}

/** キャプチャのルールを編集するシート（新しく作るときは `rule` が null）。 */
export function CaptureRuleSheet({
  rule,
  onClose,
  onSaved,
}: {
  rule: CaptureRuleDto | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const id = useId();
  const [draft, setDraft] = useState<CaptureRuleInputDto>(() =>
    rule
      ? {
          name: rule.name,
          urlPattern: rule.urlPattern,
          mode: rule.mode,
          targetSelector: rule.targetSelector,
          hideSelectors: rule.hideSelectors,
          viewportWidth: rule.viewportWidth,
          scrollTimes: rule.scrollTimes,
          waitMs: rule.waitMs,
          destFolderId: rule.destFolderId,
          enabled: rule.enabled,
        }
      : newRuleInput(),
  );
  const [hideText, setHideText] = useState(() => draft.hideSelectors.join(', '));
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const change = (patch: Partial<CaptureRuleInputDto>) => setDraft((d) => ({ ...d, ...patch }));
  const number = (text: string) => (Number.isFinite(Number(text)) ? Math.round(Number(text)) : 0);

  const save = () => {
    setSaving(true);
    setError(null);
    saveCaptureRule(rule?.id ?? null, { ...draft, hideSelectors: splitSelectors(hideText) })
      .then(() => {
        notify(s.saved);
        onSaved();
        onClose();
      })
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setSaving(false));
  };

  const remove = async () => {
    if (!rule) {
      return;
    }
    const ok = await askConfirm({
      title: s.delete,
      message: s.deleteConfirm(rule.name),
      confirm: s.deleteButton,
    });
    if (ok) {
      deleteCaptureRule(rule.id).then(() => {
        onSaved();
        onClose();
      }, fail);
    }
  };

  return (
    <Dialog labelledBy={id} onClose={onClose}>
      <div className={styles.sheet}>
        <h2 id={id} className={styles.sheetTitle}>
          {rule ? s.editTitle : s.newTitle}
        </h2>
        <div className={styles.ruleForm}>
          <label className={styles.field}>
            {s.name}
            <input
              className={styles.input}
              value={draft.name}
              onChange={(e) => change({ name: e.target.value })}
            />
          </label>
          <label className={styles.field}>
            {s.mode}
            <select
              className={styles.select}
              value={draft.mode}
              onChange={(e) => change({ mode: e.target.value as CaptureModeDto })}
            >
              {MODES.map((m) => (
                <option key={m} value={m}>
                  {t.modes[m]}
                </option>
              ))}
            </select>
          </label>
          <label className={`${styles.field} ${styles.fieldWide}`}>
            {s.pattern}
            <input
              className={`${styles.input} ${styles.mono}`}
              value={draft.urlPattern}
              spellCheck={false}
              onChange={(e) => change({ urlPattern: e.target.value })}
            />
          </label>
          {draft.mode === 'element' && (
            <label className={`${styles.field} ${styles.fieldWide}`}>
              {s.target}
              <input
                className={`${styles.input} ${styles.mono}`}
                value={draft.targetSelector ?? ''}
                spellCheck={false}
                placeholder="article"
                onChange={(e) => change({ targetSelector: e.target.value || null })}
              />
            </label>
          )}
          <label className={`${styles.field} ${styles.fieldWide}`}>
            {s.hide}
            <input
              className={`${styles.input} ${styles.mono}`}
              value={hideText}
              spellCheck={false}
              placeholder="header, .cookie-banner"
              onChange={(e) => setHideText(e.target.value)}
            />
          </label>
          <label className={styles.field}>
            {s.width}
            <input
              className={styles.input}
              type="number"
              min={320}
              max={3840}
              value={draft.viewportWidth}
              onChange={(e) => change({ viewportWidth: number(e.target.value) })}
            />
          </label>
          <label className={styles.field}>
            {s.scrolls}
            <input
              className={styles.input}
              type="number"
              min={0}
              max={30}
              value={draft.scrollTimes}
              onChange={(e) => change({ scrollTimes: number(e.target.value) })}
            />
          </label>
          <label className={styles.field}>
            {s.wait}
            <input
              className={styles.input}
              type="number"
              min={0}
              max={60000}
              step={500}
              value={draft.waitMs}
              onChange={(e) => change({ waitMs: number(e.target.value) })}
            />
          </label>
          <div className={styles.field}>
            {s.dest}
            <span className={styles.destRow}>
              <button
                type="button"
                className={styles.menuButton}
                onClick={() => {
                  pickDestFolder().then((folder) => {
                    if (folder !== undefined) {
                      change({ destFolderId: folder });
                    }
                  }, fail);
                }}
              >
                {destLabel(draft.destFolderId)}
              </button>
              {draft.destFolderId !== null && (
                <button
                  type="button"
                  className={styles.smallButton}
                  onClick={() => change({ destFolderId: null })}
                >
                  {ja.downloads.destClear}
                </button>
              )}
            </span>
          </div>
          <label className={`${styles.option} ${styles.fieldWide}`}>
            {s.enabled}
            <Switch checked={draft.enabled} onChange={(on) => change({ enabled: on })} />
          </label>
        </div>
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
        <div className={styles.sheetActions}>
          {rule && (
            <Button
              className={`${styles.pushLeft} ${styles.dangerText}`}
              onClick={() => void remove()}
            >
              {s.delete}
            </Button>
          )}
          <Button onClick={onClose}>{s.cancel}</Button>
          <Button variant="primary" disabled={saving} onClick={save}>
            {s.save}
          </Button>
        </div>
      </div>
    </Dialog>
  );
}
