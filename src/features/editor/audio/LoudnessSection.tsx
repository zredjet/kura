import { useId } from 'react';

import { Select } from '../../../components/Select/Select';
import { ja } from '../../../i18n/ja';
import type { EditLoudnessStateDto } from '../../../ipc/edit';
import { loudnessText, peakText, signed } from '../../asset/describe';
import editorStyles from '../Editor.module.css';
import { pushEditorSteps, useEditorStore } from '../editorStore';
import { opSpec, paramsOf } from '../opsCatalog';
import { addSteps } from '../paramSend';
import styles from './Audio.module.css';
import { useAudioStore } from './audioStore';

const t = ja.editor.audio;

/** 正規化の操作の目標（最後の有効な `audio.normalize`。なければ null）。 */
function useNormalizeTarget(): number | null {
  return useEditorStore((s) => {
    for (let i = s.doc.order.length - 1; i >= 0; i--) {
      const id = s.doc.order[i];
      const n = id === undefined ? undefined : s.doc.nodes[id];
      if (n && n.op === 'audio.normalize' && n.enabled) {
        const v = paramsOf(n).target;
        return typeof v === 'number' ? v : -23;
      }
    }
    return null;
  });
}

/** 値の一行「−14.2 LUFS（LRA 6.1 LU） · −1.0 dBTP」（インスペクタと同じ書き方。計れない値は理由）。 */
export function loudnessLine(state: EditLoudnessStateDto): string | null {
  if (state.type !== 'ready') return null;
  const l = state.loudness;
  const parts = [loudnessText(l)];
  if (l.truePeak !== null) parts.push(peakText(l));
  return parts.join(' · ');
}

/** 目標との差（LU。+0.2 の形。計れなければ null）。 */
export function targetDiff(state: EditLoudnessStateDto, target: number): string | null {
  if (state.type !== 'ready' || state.loudness.loudness === null) return null;
  const d = state.loudness.loudness - target;
  const text = signed(d, 1);
  return d > 0 && Number(text) !== 0 ? `+${text}` : text;
}

/**
 * 右パネルのラウドネス（出力の形式の上に固定）：値・目標の選択（−23 LUFS が既定）・目標との差。正規化の操作があれば
 * その目標を出して選択を止める。計っている間・古い間は前の値を tertiary にして「計っています…」を添える。
 * ［この目標にそろえる］（積分ラウドネスを計れたときだけ押せる）で末尾に正規化を足す。
 */
export function LoudnessSection() {
  const headingId = useId();
  const targetId = useId();
  const open = useEditorStore((s) => s.phase === 'open');
  const loudness = useAudioStore((s) => s.loudness);
  const fresh = useAudioStore((s) => s.loudnessFresh);
  const chosen = useAudioStore((s) => s.loudnessTarget);
  const normalize = useNormalizeTarget();
  const target = normalize ?? chosen;
  const line = loudnessLine(loudness);
  const diff = targetDiff(loudness, target);
  const measuring = !fresh && loudness.type !== 'failed';
  // 積分ラウドネスを計れたときだけそろえられる（無音・短すぎるなどは計り終えても null）
  const measured = loudness.type === 'ready' && loudness.loudness.loudness !== null;
  const align = () => {
    const desc = opSpec('audio.normalize');
    if (!desc) return;
    const order = useEditorStore.getState().doc.order;
    pushEditorSteps(addSteps(order.length, desc.id, desc.version, { target, peakLimit: null }), {
      selectCreated: true,
    });
  };
  const options: { value: string; label: string }[] = t.targets.map((o) => ({
    value: String(o.value),
    label: o.label,
  }));
  if (!options.some((o) => Number(o.value) === target)) {
    options.push({ value: String(target), label: `${signed(target, 1)} LUFS` });
  }
  return (
    <section className={styles.loudness} aria-labelledby={headingId}>
      <h3 id={headingId} className={editorStyles.paramsTitle}>
        {t.loudnessTitle}
      </h3>
      <p className={styles.loudnessValue} data-stale={measuring || undefined}>
        {loudness.type === 'failed'
          ? t.measureFailed(loudness.message)
          : (line ?? (measuring ? t.measuring : t.notMeasured))}
      </p>
      {measuring && line && (
        <p className={styles.loudnessNote} role="status">
          {t.measuring}
        </p>
      )}
      <div className={editorStyles.paramRow}>
        <span id={targetId} className={editorStyles.paramLabel}>
          {normalize !== null ? t.normalizeTarget : t.target}
        </span>
        <Select
          aria-labelledby={targetId}
          value={String(target)}
          options={options}
          disabled={!open || normalize !== null}
          onChange={(v) => useAudioStore.setState({ loudnessTarget: Number(v) })}
        />
      </div>
      {diff !== null && <p className={styles.loudnessNote}>{t.diff(diff)}</p>}
      {normalize === null && (
        <button
          type="button"
          className={editorStyles.pill}
          disabled={!open || !measured}
          onClick={align}
        >
          {t.alignToTarget}
        </button>
      )}
    </section>
  );
}
