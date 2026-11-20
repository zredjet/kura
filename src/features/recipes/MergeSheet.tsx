import { useId } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ProgressBar } from '../../components/ProgressBar/ProgressBar';
import { Segmented } from '../../components/Segmented/Segmented';
import { ja } from '../../i18n/ja';
import type { Anchor } from '../../ipc/batch';
import { formatBytes, formatCount } from '../../lib/format';
import { validNameTemplate } from './applyStore';
import { codeLabel, GroupRow } from './batchParts';
import {
  closeMerge,
  MAX_SHEET_NUMBER,
  type MergeOrder,
  setMergeName,
  setMergeOrder,
  setMergeParams,
  startMergeJob,
  useMergeStore,
} from './mergeStore';
import styles from './Recipes.module.css';

const t = ja.merge;

/** 9 点の揃え（左上から行の順）。 */
const ANCHORS: readonly Anchor[] = [
  'topLeft',
  'top',
  'topRight',
  'left',
  'center',
  'right',
  'bottomLeft',
  'bottom',
  'bottomRight',
];

/** 数の欄を読む（空は null、範囲の外・数でなければ undefined）。 */
export function readSheetNumber(text: string, min: number): number | null | undefined {
  const trimmed = text.trim();
  if (trimmed === '') return null;
  const n = Number(trimmed);
  if (!Number.isInteger(n) || n < min || n > MAX_SHEET_NUMBER) return undefined;
  return n;
}

/** #rrggbb を RGBA8 に（不透明）。 */
export function rgbaOf(hex: string): [number, number, number, number] {
  const n = Number.parseInt(hex.replace('#', ''), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 255];
}

/** RGBA8 を #rrggbb に。 */
export function hexOf(rgba: readonly number[]): string {
  return `#${rgba
    .slice(0, 3)
    .map((c) => c.toString(16).padStart(2, '0'))
    .join('')}`;
}

/** 数の欄 1 つ（px）。 */
function NumberField({
  label,
  value,
  min,
  placeholder,
  onChange,
}: {
  label: string;
  value: number | null;
  min: number;
  placeholder?: string;
  onChange: (v: number | null) => void;
}) {
  const id = useId();
  return (
    <div className={styles.numberField}>
      <label htmlFor={id} className={styles.label}>
        {label}
      </label>
      <input
        id={id}
        className={styles.seconds}
        inputMode="numeric"
        defaultValue={value ?? ''}
        placeholder={placeholder}
        onChange={(e) => {
          const n = readSheetNumber(e.target.value, min);
          if (n !== undefined) onChange(n);
        }}
      />
    </div>
  );
}

/**
 * まとめて 1 つにするシート（スプライトシート。M14 段 7）：並べる順・1 行の数（空は自動）・すき間・何もない所（透明｜色）・
 * セルの中の揃え（9 点）・名前、事前チェックのシートの大きさ・コマ・飛ばすもの。
 */
export function MergeSheet() {
  const phase = useMergeStore((s) => s.phase);
  const order = useMergeStore((s) => s.order);
  const columns = useMergeStore((s) => s.columns);
  const gapX = useMergeStore((s) => s.gapX);
  const gapY = useMergeStore((s) => s.gapY);
  const fill = useMergeStore((s) => s.fill);
  const anchor = useMergeStore((s) => s.anchor);
  const name = useMergeStore((s) => s.name);
  const done = useMergeStore((s) => s.done);
  const total = useMergeStore((s) => s.total);
  const precheck = useMergeStore((s) => s.precheck);
  const error = useMergeStore((s) => s.error);
  const starting = useMergeStore((s) => s.starting);
  const titleId = useId();
  const nameId = useId();
  const colorId = useId();
  if (phase === 'closed' || phase === 'preparing') return null;
  const sheet = precheck?.sheet ?? null;
  const n = precheck?.apply.length ?? 0;
  const validName = validNameTemplate(name);
  return (
    <Dialog labelledBy={titleId} onClose={closeMerge} size="md">
      <div className={styles.sheet}>
        <h2 id={titleId} className={styles.title}>
          {t.title}
        </h2>
        <div className={styles.body}>
          <div className={styles.field}>
            <span className={styles.label}>{t.order}</span>
            <Segmented<MergeOrder>
              label={t.order}
              value={order}
              options={[
                { value: 'selected', label: t.orderSelected },
                { value: 'name', label: t.orderName },
                { value: 'list', label: t.orderList },
              ]}
              onChange={(o) => void setMergeOrder(o)}
            />
          </div>
          <div className={styles.numberRow}>
            <NumberField
              label={t.columns}
              value={columns}
              min={1}
              placeholder={t.columnsAuto}
              onChange={(v) => setMergeParams({ columns: v })}
            />
            <NumberField
              label={`${t.gapX}（${t.px}）`}
              value={gapX}
              min={0}
              placeholder="0"
              onChange={(v) => setMergeParams({ gapX: v ?? 0 })}
            />
            <NumberField
              label={`${t.gapY}（${t.px}）`}
              value={gapY}
              min={0}
              placeholder="0"
              onChange={(v) => setMergeParams({ gapY: v ?? 0 })}
            />
          </div>
          <div className={styles.field}>
            <span className={styles.label}>{t.fill}</span>
            <div className={styles.fillRow}>
              <Segmented<'transparent' | 'color'>
                label={t.fill}
                value={fill ? 'color' : 'transparent'}
                options={[
                  { value: 'transparent', label: t.fillTransparent },
                  { value: 'color', label: t.fillColor },
                ]}
                onChange={(v) =>
                  setMergeParams({ fill: v === 'color' ? (fill ?? [255, 255, 255, 255]) : null })
                }
              />
              {fill && (
                <input
                  id={colorId}
                  type="color"
                  className={styles.color}
                  aria-label={t.fillColor}
                  value={hexOf(fill)}
                  onChange={(e) => setMergeParams({ fill: rgbaOf(e.target.value) })}
                />
              )}
            </div>
          </div>
          <div className={styles.field}>
            <span className={styles.label}>{t.anchor}</span>
            <div className={styles.anchors} role="group" aria-label={t.anchor}>
              {ANCHORS.map((a) => (
                <button
                  key={a}
                  type="button"
                  className={styles.anchor}
                  aria-label={t.anchors[a]}
                  title={t.anchors[a]}
                  aria-pressed={anchor === a}
                  onClick={() => setMergeParams({ anchor: a })}
                >
                  <span className={styles.anchorDot} aria-hidden="true" />
                </button>
              ))}
            </div>
          </div>
          <div className={styles.field}>
            <label htmlFor={nameId} className={styles.label}>
              {t.name}
            </label>
            <input
              id={nameId}
              className={styles.input}
              value={name}
              spellCheck={false}
              autoComplete="off"
              onChange={(e) => setMergeName(e.target.value)}
            />
            {validName ? (
              <p className={styles.note}>{t.nameHint}</p>
            ) : (
              <p className={styles.error} role="alert">
                {ja.editor.saveAsBadName}
              </p>
            )}
          </div>
          {phase === 'checking' && (
            <div className={styles.checking} role="status">
              <span>
                {ja.batch.checking} {ja.batch.checkingCount(formatCount(done), formatCount(total))}
              </span>
              <ProgressBar value={total > 0 ? done / total : null} label={ja.batch.checking} />
            </div>
          )}
          {phase === 'error' && (
            <p className={styles.error} role="alert">
              {ja.batch.checkFailed}：{error}
            </p>
          )}
          {phase === 'ready' && precheck && (
            <>
              {sheet && (
                <div className={styles.field}>
                  <p className={styles.counts}>
                    {t.sheet(formatCount(sheet.width), formatCount(sheet.height))}
                  </p>
                  <p className={styles.note}>
                    {t.cells(
                      formatCount(sheet.frames),
                      formatCount(sheet.columns),
                      formatCount(sheet.cellW),
                      formatCount(sheet.cellH),
                    )}
                  </p>
                  {sheet.approximate && <p className={styles.note}>{t.approximate}</p>}
                  {sheet.frames > n && <p className={styles.note}>{t.animated}</p>}
                </div>
              )}
              {precheck.skips.length > 0 && (
                <ul className={styles.groups}>
                  {precheck.skips.map((g) => (
                    <GroupRow
                      key={`${g.state}:${g.code}:${g.message}`}
                      title={codeLabel(g.code, g.message)}
                      ids={g.items}
                      note={g.message}
                      onSelect={closeMerge}
                    />
                  ))}
                </ul>
              )}
              <p className={styles.note}>{ja.batch.disk(formatBytes(precheck.disk.newBytes))}</p>
              {error && (
                <p className={styles.error} role="alert">
                  {error}
                </p>
              )}
            </>
          )}
        </div>
        <footer className={styles.actions}>
          <Button onClick={closeMerge}>{ja.batch.cancel}</Button>
          <Button
            variant="primary"
            disabled={phase !== 'ready' || n === 0 || starting || !validName || !sheet}
            onClick={() => void startMergeJob()}
          >
            {t.start(formatCount(n))}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
