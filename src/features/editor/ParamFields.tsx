import { type ReactNode, useCallback, useId, useRef, useState } from 'react';

import { LinkIcon } from '../../components/icons/LinkIcon';
import { Segmented } from '../../components/Segmented/Segmented';
import { Select } from '../../components/Select/Select';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import type { Issue, JsonValue, ParamSpec } from '../../ipc/edit';
import { formatCount } from '../../lib/format';
import { isImeKey } from '../../lib/ime';
import { toHex } from './color';
import { ColorPicker, Swatch } from './ColorPicker';
import styles from './Editor.module.css';
import { sendParam } from './editorStore';
import {
  clampNumber,
  controlFor,
  enumLabel,
  GRID_KEYS,
  initialValue,
  lengthDefault,
  type LengthMode,
  makeColor,
  makeLength,
  numberRange,
  numbers,
  paramLabel,
  POINT_KEYS,
  RATIO_KEYS,
  readColor,
  readLength,
  RECT_KEYS,
  SIZE_KEYS,
  unitLabel,
} from './opsCatalog';
import type { ParamGesture } from './paramSend';

const t = ja.editor;

/** 欄が送るもの（ノード・引数は呼ぶ側が決める）。 */
type Send = (gesture: ParamGesture) => void;

/** 指摘の文（種類の言葉と、Rust の言葉）。 */
export function issueText(issue: Issue): string {
  const lead = t.issues[issue.code] ?? '';
  if (!issue.message) return lead;
  return lead && lead !== issue.message ? `${lead}（${issue.message}）` : issue.message;
}

/**
 * 数の入力欄（文字を打っている間は手元の文字。読める値になるたびに `onValue`、Enter・欄を離れたら `onSeal`）。範囲の外の
 * 値は、Enter・欄を離れたときに収めて送る。
 */
export function NumberInput({
  id,
  label,
  value,
  min,
  max,
  step = 1,
  integer = true,
  disabled,
  onValue,
  onSeal,
}: {
  id?: string;
  label?: string;
  value: number | null;
  min: number | null;
  max: number | null;
  step?: number;
  integer?: boolean;
  disabled?: boolean;
  onValue: (v: number) => void;
  onSeal: () => void;
}) {
  const [text, setText] = useState<string | null>(null);
  const clamp = (v: number): number => {
    let out = integer ? Math.round(v) : v;
    if (min !== null) out = Math.max(min, out);
    if (max !== null) out = Math.min(max, out);
    return out;
  };
  const finish = () => {
    if (text !== null && text.trim() !== '') {
      const v = Number(text);
      if (Number.isFinite(v)) {
        const c = clamp(v);
        if (c !== value) onValue(c);
      }
    }
    setText(null);
    onSeal();
  };
  return (
    <input
      id={id}
      aria-label={id ? undefined : label}
      className={styles.paramNumber}
      type="number"
      inputMode={integer ? 'numeric' : 'decimal'}
      min={min ?? undefined}
      max={max ?? undefined}
      step={step}
      disabled={disabled}
      value={text ?? value ?? ''}
      onChange={(e) => {
        const raw = e.target.value;
        setText(raw);
        if (raw.trim() === '') return;
        const v = Number(raw);
        // 範囲の中のときだけ打つたびに送る（外は Enter・離れたときに収める）
        if (Number.isFinite(v) && clamp(v) === v && v !== value) onValue(v);
      }}
      onBlur={finish}
      onKeyDown={(e) => {
        if (isImeKey(e)) return;
        if (e.key === 'Enter') {
          e.preventDefault();
          finish();
        } else if (e.key === 'Escape' && text !== null) {
          e.preventDefault();
          e.stopPropagation();
          setText(null);
        }
      }}
    />
  );
}

/** 送った値を、Rust の結果が同じ値になるまで手元に持つ（スライダーが古い値に戻ってちらつかない）。 */
function usePending<T>(current: T): [T, (v: T | null) => void] {
  const [pending, setPending] = useState<{ v: T } | null>(null);
  // 結果が届いて同じ値になったら手元の値をやめる（描画の途中で直す：effect で直すと 1 回古い値で描く）
  if (pending && Object.is(pending.v, current)) setPending(null);
  const set = useCallback((v: T | null) => setPending(v === null ? null : { v }), []);
  return [pending ? pending.v : current, set];
}

/** 整数・小数：数の欄とスライダー（ドラッグ中は preview、離したら確定、矢印と数の入力は merge、Enter・離れたら seal）。 */
function NumberParam({
  spec,
  label,
  value,
  send,
}: {
  spec: ParamSpec;
  label: string;
  value: JsonValue;
  send: Send;
}) {
  const id = useId();
  const r = numberRange(spec);
  const current = typeof value === 'number' ? value : null;
  const [shown, setPending] = usePending<number | null>(current);
  const dragging = useRef(false);
  const unit = unitLabel(spec.unit);
  const endDrag = (cancel: boolean) => {
    if (!dragging.current) return;
    dragging.current = false;
    send(cancel ? { kind: 'cancel' } : { kind: 'release' });
    if (cancel) setPending(null);
  };
  return (
    <div className={styles.paramRow}>
      <label htmlFor={id} className={styles.paramLabel}>
        {label}
      </label>
      <div className={styles.paramControls}>
        {r.slider && r.min !== null && r.max !== null && (
          <input
            type="range"
            className={styles.paramSlider}
            aria-label={label}
            min={r.min}
            max={r.max}
            step={r.step}
            value={shown ?? r.min}
            onPointerDown={() => {
              dragging.current = true;
            }}
            onPointerUp={() => endDrag(false)}
            onPointerCancel={() => endDrag(true)}
            onChange={(e) => {
              const v = clampNumber(spec, Number(e.target.value));
              if (v === null) return;
              setPending(v);
              send(dragging.current ? { kind: 'drag', value: v } : { kind: 'nudge', value: v });
            }}
            onBlur={() => {
              endDrag(false);
              send({ kind: 'seal' });
            }}
          />
        )}
        <NumberInput
          id={id}
          value={shown}
          min={r.min}
          max={r.max}
          step={r.step}
          integer={r.integer}
          onValue={(v) => {
            setPending(v);
            send({ kind: 'nudge', value: v });
          }}
          onSeal={() => send({ kind: 'seal' })}
        />
        {unit && <span className={styles.paramUnit}>{unit}</span>}
      </div>
    </div>
  );
}

/** 数の組（矩形・点・大きさ・比・格子）。どの欄を変えても同じ引数の merge、Enter・離れたら seal。 */
function NumbersParam({
  label,
  value,
  keys,
  names,
  min,
  send,
  columns = keys.length > 2 ? 2 : keys.length,
  chain = false,
}: {
  label: string;
  value: JsonValue;
  keys: readonly string[];
  names: Record<string, string>;
  min: Record<string, number>;
  send: Send;
  columns?: number;
  chain?: boolean;
}) {
  const now = numbers(value, keys) ?? Object.fromEntries(keys.map((k) => [k, 0]));
  const [linked, setLinked] = useState(false);
  const [shown, setPending] = usePending<string>(JSON.stringify(now));
  const vals = JSON.parse(shown) as Record<string, number>;
  const setOne = (key: string, v: number) => {
    const next = { ...vals, [key]: v };
    // 鎖：幅と高さの比を保つ
    if (chain && linked && keys.length === 2) {
      const [a, b] = keys as [string, string];
      const other = key === a ? b : a;
      const base = vals[key] ?? 0;
      if (base > 0) next[other] = Math.max(1, Math.round(((vals[other] ?? 0) * v) / base));
    }
    setPending(JSON.stringify(next));
    send({ kind: 'nudge', value: next });
  };
  return (
    <fieldset className={styles.paramGroup}>
      <legend className={styles.paramLabel}>{label}</legend>
      <div
        className={styles.paramGrid}
        style={{ gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))` }}
      >
        {keys.map((key) => (
          <NumberLabeled
            key={key}
            name={names[key] ?? key}
            value={vals[key] ?? 0}
            min={min[key] ?? null}
            onValue={(v) => setOne(key, v)}
            onSeal={() => send({ kind: 'seal' })}
          />
        ))}
      </div>
      {chain && (
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.keepRatio}
          title={t.keepRatio}
          aria-pressed={linked}
          onClick={() => setLinked((v) => !v)}
        >
          <LinkIcon size={14} />
        </button>
      )}
    </fieldset>
  );
}

function NumberLabeled({
  name,
  value,
  min,
  onValue,
  onSeal,
}: {
  name: string;
  value: number;
  min: number | null;
  onValue: (v: number) => void;
  onSeal: () => void;
}) {
  const id = useId();
  return (
    <span className={styles.paramCell}>
      <label htmlFor={id} className={styles.paramCellLabel}>
        {name}
      </label>
      <NumberInput id={id} value={value} min={min} max={null} onValue={onValue} onSeal={onSeal} />
    </span>
  );
}

/** 長さ：「px｜%｜自動」と数。 */
function LengthParam({
  spec,
  label,
  value,
  size,
  send,
}: {
  spec: ParamSpec;
  label: string;
  value: JsonValue;
  size: { w: number; h: number } | null;
  send: Send;
}) {
  const id = useId();
  const len = readLength(value);
  const axis = /height|^h$/i.test(spec.key) ? (size?.h ?? null) : (size?.w ?? null);
  return (
    <div className={styles.paramRow}>
      <label htmlFor={id} className={styles.paramLabel}>
        {label}
      </label>
      <div className={styles.paramControls}>
        <Segmented<LengthMode>
          label={t.lengthMode(label)}
          value={len.mode}
          options={(['px', 'percent', 'auto'] as const).map((m) => ({
            value: m,
            label: t.lengthModes[m],
          }))}
          onChange={(mode) =>
            send({ kind: 'set', value: makeLength(mode, lengthDefault(mode, axis)) })
          }
        />
        {len.mode !== 'auto' && (
          <NumberInput
            id={id}
            value={len.n}
            min={len.mode === 'px' ? 1 : 0.01}
            max={null}
            integer={len.mode === 'px'}
            step={len.mode === 'px' ? 1 : 1}
            onValue={(v) => send({ kind: 'nudge', value: makeLength(len.mode, v) })}
            onSeal={() => send({ kind: 'seal' })}
          />
        )}
      </div>
    </div>
  );
}

/** 色：見本（押すと欄の下に色を選ぶ部品）。アルファを持たない引数は不透明度を出さない。 */
function ColorParam({
  spec,
  label,
  value,
  send,
}: {
  spec: ParamSpec;
  label: string;
  value: JsonValue;
  send: Send;
}) {
  const alpha = spec.kind.type === 'color' && spec.kind.alpha;
  const rgba = readColor(value) ?? [0, 0, 0, alpha ? 0 : 255];
  const [open, setOpen] = useState(false);
  const close = useCallback(() => {
    setOpen(false);
    send({ kind: 'seal' });
  }, [send, setOpen]);
  const hex = toHex(rgba);
  return (
    <div className={styles.paramRow}>
      <span className={styles.paramLabel}>{label}</span>
      <div className={styles.paramColor}>
        <div className={styles.paramControls}>
          <Swatch
            color={{ rgba, index: null }}
            label={t.paramColor(label, hex)}
            pressed={open}
            onClick={() => setOpen((v) => !v)}
          />
          <span className={styles.paramHex}>{hex}</span>
        </div>
        {open && (
          <ColorPicker
            color={{ rgba, index: null }}
            recent={[]}
            inline
            alpha={alpha}
            label={label}
            onChange={(c) => send({ kind: 'nudge', value: makeColor(c.rgba, alpha) })}
            onClose={close}
          />
        )}
      </div>
    </div>
  );
}

/** 引数 1 つの欄（`ParamSchema` の種類から組む。既定値が null の引数は「指定する」のスイッチを添える）。 */
export function ParamField({
  node,
  op,
  spec,
  value,
  issues,
  size,
  count = null,
}: {
  node: number;
  op: string;
  spec: ParamSpec;
  value: JsonValue;
  issues: readonly Issue[];
  size: { w: number; h: number } | null;
  /** 追記だけの配列の要素の数（ノードの `log`。引数の値は `[]` で届く）。 */
  count?: number | null;
}) {
  const label = paramLabel(op, spec.key);
  const send = useCallback<Send>((g) => sendParam(node, spec.key, g), [node, spec.key]);
  const control = controlFor(spec);
  const optional = spec.default === null && control !== 'count' && control !== 'readonly';
  const mine = issues.filter((i) => i.node === node && i.param === spec.key);
  let body: ReactNode = null;
  if (!(optional && value === null)) {
    switch (control) {
      case 'number':
        body = <NumberParam spec={spec} label={label} value={value} send={send} />;
        break;
      case 'switch':
        body = (
          <label className={styles.paramSwitch}>
            <Switch checked={value === true} onChange={(on) => send({ kind: 'set', value: on })} />
            {label}
          </label>
        );
        break;
      case 'segmented':
      case 'select': {
        const values = spec.kind.type === 'enum' ? spec.kind.values : [];
        const options = values.map((v) => ({ value: v, label: enumLabel(op, spec.key, v) }));
        const current = typeof value === 'string' ? value : (values[0] ?? '');
        body = (
          <div className={styles.paramRow}>
            <span className={styles.paramLabel}>{label}</span>
            {control === 'segmented' ? (
              <Segmented
                label={label}
                value={current}
                options={options}
                onChange={(v) => send({ kind: 'set', value: v })}
              />
            ) : (
              <Select
                aria-label={label}
                value={current}
                options={options}
                onChange={(v) => send({ kind: 'set', value: v })}
              />
            )}
          </div>
        );
        break;
      }
      case 'rect':
        body = (
          <NumbersParam
            label={label}
            value={value}
            keys={RECT_KEYS}
            names={t.rect}
            min={{ w: 1, h: 1 }}
            send={send}
          />
        );
        break;
      case 'point':
        body = (
          <NumbersParam
            label={label}
            value={value}
            keys={POINT_KEYS}
            names={t.point}
            min={{}}
            send={send}
          />
        );
        break;
      case 'size':
        body = (
          <NumbersParam
            label={label}
            value={value}
            keys={SIZE_KEYS}
            names={t.sizeFields}
            min={{ w: 1, h: 1 }}
            send={send}
            chain
          />
        );
        break;
      case 'ratio':
        body = (
          <NumbersParam
            label={label}
            value={value}
            keys={RATIO_KEYS}
            names={t.ratio}
            min={{ num: 0, den: 1 }}
            send={send}
          />
        );
        break;
      case 'grid':
        body = (
          <NumbersParam
            label={label}
            value={value}
            keys={GRID_KEYS}
            names={t.grid}
            min={{ cellW: 1, cellH: 1, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0 }}
            send={send}
          />
        );
        break;
      case 'length':
        body = <LengthParam spec={spec} label={label} value={value} size={size} send={send} />;
        break;
      case 'color':
        body = <ColorParam spec={spec} label={label} value={value} send={send} />;
        break;
      case 'count':
        body = (
          <div className={styles.paramRow}>
            <span className={styles.paramLabel}>{label}</span>
            <span className={styles.paramValue}>
              {count !== null
                ? formatCount(count)
                : Array.isArray(value)
                  ? formatCount(value.length)
                  : '—'}
            </span>
          </div>
        );
        break;
      default:
        body = (
          <div className={styles.paramRow}>
            <span className={styles.paramLabel}>{label}</span>
            <span className={styles.paramValue}>
              {value === null ? t.none : JSON.stringify(value)}
            </span>
          </div>
        );
    }
  }
  return (
    <div className={styles.param} data-param={spec.key}>
      {optional && (
        <label className={styles.paramSwitch}>
          <Switch
            checked={value !== null}
            onChange={(on) => send({ kind: 'set', value: on ? initialValue(spec, size) : null })}
          />
          {`${label}を${t.specify}`}
        </label>
      )}
      {body}
      {mine.map((issue, i) => (
        <p key={i} className={styles.paramIssue} role="alert">
          {issueText(issue)}
        </p>
      ))}
    </div>
  );
}
