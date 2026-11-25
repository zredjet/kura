import { useState } from 'react';

import { PlusIcon } from '../../../components/icons/PlusIcon';
import { Select } from '../../../components/Select/Select';
import { Switch } from '../../../components/Switch/Switch';
import { ja } from '../../../i18n/ja';
import { EQ_LIMITS } from '../../../ipc/edit';
import editorStyles from '../Editor.module.css';
import { useEditorStore } from '../editorStore';
import styles from './Audio.module.css';
import { sendEqBands, setEqBand, useAudioStore, useEqPort } from './audioStore';
import {
  BAND_CHANNELS,
  BAND_TYPES,
  type BandType,
  type EqBand,
  clampFreq,
  clampGain,
  clampQ,
  dbText,
  freqText,
  hasGain,
  hasQ,
  hasSlope,
  nearestSlope,
  newBell,
  readEq,
} from './eqModel';

const ts = ja.editor.audio.spectrum;

/** ノードの今のバンドの列（頭の文書 ＋ 手元の値）。 */
export function useEqBands(node: number): EqBand[] {
  const params = useEditorStore((s) => s.doc.nodes[node]?.params);
  const local = useAudioStore((s) => s.eqLocal);
  return local && local.node === node ? local.bands : readEq(params).bands;
}

/** バンドの中身（「ベル · 2.80 kHz · −5.0 dB · Q 6.00」）。 */
export function bandDetail(b: EqBand): string {
  const parts = [ts.bandTypes[b.type], freqText(b.freq)];
  if (hasGain(b.type)) parts.push(dbText(b.gainDb));
  if (hasSlope(b.type)) parts.push(ts.slopeValue(b.slope));
  else if (hasQ(b.type)) parts.push(`Q ${b.q.toFixed(2)}`);
  if (b.channel !== 'all') parts.push(ts.bandChannels[b.channel]);
  if (!b.enabled) parts.push(ts.off);
  return parts.join(' · ');
}

/** バンドの読み（「バンド 3」と全角の空白のあとに「ベル · 2.80 kHz · −5.0 dB · Q 6.00」。カプセルの下の段と面の読み上げ）。 */
export function bandSummary(b: EqBand, index: number): string {
  return ts.bandReading(index + 1, bandDetail(b));
}

/** 数の欄（Enter か離れたときに確定する）。 */
function NumberField({
  label,
  value,
  step,
  format,
  onCommit,
}: {
  label: string;
  value: number;
  step: number;
  format: (v: number) => string;
  onCommit: (v: number) => void;
}) {
  const [text, setText] = useState<string | null>(null);
  const commit = () => {
    if (text === null) return;
    const v = Number(text);
    setText(null);
    if (Number.isFinite(v)) onCommit(v);
  };
  return (
    <label className={editorStyles.paramRow}>
      <span className={editorStyles.paramLabel}>{label}</span>
      <input
        className={styles.eqNumber}
        type="number"
        step={step}
        value={text ?? format(value)}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit();
        }}
      />
    </label>
  );
}

/**
 * イコライザのバンドの列の欄（右パネル。M17 段 5）：「n バンドのうち m が有効」・バンドの表（行は `<button>`。キーと
 * 読み上げの道。8 行を超えたら表の中でスクロール）・［＋ バンドを足す］・選んだバンドの欄・［バンドを消す］。値の欄は
 * 右パネルだけ（面の点は飾り）。行のキー（↑↓ でバンドを移る・⌫ でバンドを消す）は `audioKeys`。
 */
export function EqBandsField({ node }: { node: number }) {
  const bands = useEqBands(node);
  const selected = useAudioStore((s) => s.eqBand);
  const { rate } = useEqPort();
  const enabledCount = bands.filter((b) => b.enabled).length;
  const full = bands.length >= EQ_LIMITS.maxBands;
  const band = selected === null ? undefined : bands[selected];
  const set = (index: number, patch: Partial<EqBand>) =>
    sendEqBands(
      node,
      bands.map((b, i) => (i === index ? { ...b, ...patch } : b)),
      'set',
    );
  return (
    <div className={styles.eqBands}>
      <p className={styles.eqBandsHead}>{ts.bandCount(bands.length, enabledCount)}</p>
      <div className={styles.eqTable} role="list" aria-label={ts.bands}>
        {bands.map((b, i) => (
          <div key={i} className={styles.eqRow} role="listitem">
            <button
              type="button"
              className={styles.eqRowButton}
              aria-pressed={i === selected}
              data-eq-band-row={i}
              data-eq-node={node}
              onClick={(e) => {
                // WebKit は押したボタンにフォーカスを移さない：移さないと次の ⌫ が行に届かず、核の「操作を消す」になる
                e.currentTarget.focus();
                setEqBand(i);
              }}
            >
              <span className={styles.eqIndex}>{i + 1}</span>
              <span>{bandDetail(b)}</span>
            </button>
            <Switch
              checked={b.enabled}
              aria-label={ts.bandEnabled(i + 1)}
              onChange={(on) => set(i, { enabled: on })}
            />
          </div>
        ))}
      </div>
      <button
        type="button"
        className={styles.eqAdd}
        data-eq-band-add=""
        disabled={full}
        onClick={() => {
          sendEqBands(node, [...bands, newBell(1_000, 0)], 'set');
          setEqBand(bands.length);
        }}
      >
        <PlusIcon size={14} strokeWidth={2} />
        {ts.addBand}
      </button>
      {full && <p className={editorStyles.note}>{ts.bandsFull(EQ_LIMITS.maxBands)}</p>}
      {band && selected !== null && (
        <div className={styles.eqSelected} role="group" aria-label={bandSummary(band, selected)}>
          <label className={editorStyles.paramRow}>
            <span className={editorStyles.paramLabel}>{ts.bandType}</span>
            <Select
              value={band.type}
              options={BAND_TYPES.map((v) => ({ value: v, label: ts.bandTypes[v] }))}
              onChange={(v) => {
                const type = v as BandType;
                set(selected, { type, q: clampQ(band.q, type) });
              }}
            />
          </label>
          <NumberField
            label={ts.freq}
            value={band.freq}
            step={1}
            format={(v) => (v >= 100 ? v.toFixed(0) : v.toFixed(1))}
            onCommit={(v) => set(selected, { freq: clampFreq(v, rate) })}
          />
          {hasGain(band.type) && (
            <NumberField
              label={ts.gain}
              value={band.gainDb}
              step={0.1}
              format={(v) => v.toFixed(1)}
              onCommit={(v) => set(selected, { gainDb: clampGain(v) })}
            />
          )}
          {hasQ(band.type) && (
            <NumberField
              label="Q"
              value={band.q}
              step={0.01}
              format={(v) => v.toFixed(2)}
              onCommit={(v) => set(selected, { q: clampQ(v, band.type) })}
            />
          )}
          {hasSlope(band.type) && (
            <label className={editorStyles.paramRow}>
              <span className={editorStyles.paramLabel}>{ts.slope}</span>
              <Select
                value={String(band.slope)}
                options={EQ_LIMITS.slopes.map((v) => ({
                  value: String(v),
                  label: ts.slopeValue(v),
                }))}
                onChange={(v) => set(selected, { slope: nearestSlope(Number(v)) })}
              />
            </label>
          )}
          <label className={editorStyles.paramRow}>
            <span className={editorStyles.paramLabel}>{ts.bandChannel}</span>
            <Select
              value={band.channel}
              options={BAND_CHANNELS.map((v) => ({ value: v, label: ts.bandChannels[v] }))}
              onChange={(v) => set(selected, { channel: v as EqBand['channel'] })}
            />
          </label>
          <button
            type="button"
            className={styles.eqRemove}
            onClick={() => {
              setEqBand(null);
              sendEqBands(
                node,
                bands.filter((_, i) => i !== selected),
                'set',
              );
            }}
          >
            {ts.removeBand}
          </button>
        </div>
      )}
    </div>
  );
}
