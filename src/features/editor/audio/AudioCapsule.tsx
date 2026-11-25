import type { ReactNode } from 'react';

import { FadeIcon } from '../../../components/icons/FadeIcon';
import { RepeatIcon } from '../../../components/icons/RepeatIcon';
import { ScissorsIcon } from '../../../components/icons/ScissorsIcon';
import { SelectRangeIcon } from '../../../components/icons/SelectRangeIcon';
import { Segmented } from '../../../components/Segmented/Segmented';
import { Switch } from '../../../components/Switch/Switch';
import { ja } from '../../../i18n/ja';
import editorStyles from '../Editor.module.css';
import { sendParam, useEditorStore } from '../editorStore';
import { paramsOf } from '../opsCatalog';
import styles from './Audio.module.css';
import {
  AUDIO_TOOLS,
  type AudioTool,
  cancelTrim,
  setSelection,
  toggleBypass,
  useAudioStore,
} from './audioStore';
import {
  clearLoop,
  confirmTrim,
  fadeLengths,
  sealHandle,
  selectAudioTool,
  selectionToLoop,
  selectionToTrim,
  setFade,
  setLoop,
  setSnapZero,
  setTrimFrame,
} from './audioTools';
import { bandSummary, useEqBands } from './EqBandsField';
import { targetNode } from './handles';
import { TimeField } from './TimeField';
import { Transport } from './Transport';

const t = ja.editor.audio;

const TOOL_ICONS: Record<AudioTool, ReactNode> = {
  select: <SelectRangeIcon size={16} />,
  trim: <ScissorsIcon size={16} />,
  fade: <FadeIcon size={16} />,
  loop: <RepeatIcon size={16} />,
};

/** 道具の切り替え（選択 S・切り詰め T・フェード F・ループ R。下のカプセルの上の段）。 */
export function AudioToolSwitch() {
  const tool = useAudioStore((s) => s.tool);
  const open = useEditorStore((s) => s.phase === 'open');
  return (
    <div
      className={`${editorStyles.tools} ${styles.toolSwitch}`}
      role="group"
      aria-label={ja.editor.tools}
    >
      {AUDIO_TOOLS.map((id) => {
        const label = ja.editor.withKey(t.toolNames[id], t.toolKeys[id]);
        return (
          <button
            key={id}
            type="button"
            className={editorStyles.tool}
            aria-label={label}
            title={label}
            aria-pressed={tool === id}
            disabled={!open}
            onClick={() => selectAudioTool(id)}
          >
            {TOOL_ICONS[id]}
          </button>
        );
      })}
    </div>
  );
}

/** 選択：始め・終わり、［選択の範囲に切り詰める］［選択をループにする］。 */
function SelectFields({ rate, frames }: { rate: number; frames: number }) {
  const sel = useAudioStore((s) => s.selection);
  const has = sel !== null && sel.end > sel.start;
  return (
    <>
      <TimeField
        label={t.start}
        frames={sel?.start ?? null}
        rate={rate}
        max={frames}
        onValue={(f) => setSelection({ start: f, end: Math.max(f + 1, sel?.end ?? frames) })}
      />
      <TimeField
        label={t.end}
        frames={sel?.end ?? null}
        rate={rate}
        max={frames}
        onValue={(f) => setSelection({ start: Math.min(f - 1, sel?.start ?? 0), end: f })}
      />
      <button
        type="button"
        className={editorStyles.pill}
        aria-label={t.selectionToTrim}
        title={t.selectionToTrim}
        disabled={!has}
        onClick={selectionToTrim}
      >
        {t.selectionToTrimShort}
      </button>
      <button
        type="button"
        className={editorStyles.pill}
        aria-label={t.selectionToLoop}
        title={t.selectionToLoop}
        disabled={!has}
        onClick={selectionToLoop}
      >
        {t.selectionToLoopShort}
      </button>
    </>
  );
}

/** 切り詰め：始め・終わり、使い方の一言、「やめる（esc）」「切り詰める（↵）」。 */
function TrimFields({ rate }: { rate: number }) {
  const trim = useAudioStore((s) => s.trim);
  if (!trim) return null;
  const b = trim.bounds;
  return (
    <>
      {b !== null && (
        <>
          <TimeField
            label={t.start}
            frames={trim.start}
            rate={rate}
            max={b - 1}
            onValue={(f) => setTrimFrame(f, trim.end)}
          />
          <TimeField
            label={t.end}
            frames={trim.end}
            rate={rate}
            min={1}
            max={b}
            onValue={(f) => setTrimFrame(trim.start, f)}
          />
        </>
      )}
      <button type="button" className={editorStyles.pill} onClick={cancelTrim}>
        {t.trimCancel}
      </button>
      <button
        type="button"
        className={`${editorStyles.pill} ${editorStyles.pillPrimary}`}
        disabled={b === null}
        onClick={confirmTrim}
      >
        {t.trimConfirm}
      </button>
      <span className={styles.toolHint}>{b === null ? t.trimWaiting : t.trimHint}</span>
    </>
  );
}

/** フェード：フェードイン・フェードアウトの長さ、曲線のセグメント。 */
function FadeFields({ rate, frames }: { rate: number; frames: number }) {
  // 手元の値・表示の結果が変わるたびに長さを読み直す
  useAudioStore((s) => s.local);
  useAudioStore((s) => s.view?.nodes);
  const lengths = fadeLengths();
  const node = useEditorStore((s) => targetNode(s.doc, s.selected, 'audio.fade'));
  const curve = useEditorStore((s) => {
    const n = node === null ? undefined : s.doc.nodes[node];
    const c = n ? paramsOf(n).curve : null;
    return typeof c === 'string' ? c : 'linear';
  });
  const fadeOps = ja.editor.ops['audio.fade'];
  return (
    <>
      <TimeField
        label={t.fadeIn}
        frames={lengths.fadeIn}
        rate={rate}
        max={frames}
        onValue={(f) => setFade('in', f)}
        onSeal={sealHandle}
      />
      <TimeField
        label={t.fadeOut}
        frames={lengths.fadeOut}
        rate={rate}
        max={frames}
        onValue={(f) => setFade('out', frames - f)}
        onSeal={sealHandle}
      />
      <Segmented
        label={t.curve}
        value={curve}
        disabled={node === null}
        options={['linear', 'equalPower'].map((v) => ({
          value: v,
          label: fadeOps?.values?.curve?.[v] ?? v,
        }))}
        onChange={(v) => {
          if (node !== null) sendParam(node, 'curve', { kind: 'set', value: v });
        }}
      />
      {lengths.fadeIn === 0 && lengths.fadeOut === 0 && (
        <span className={styles.toolHint}>{t.fadeHint}</span>
      )}
    </>
  );
}

/** ループ：始め・終わり、［ループ点を消す］、「ゼロ交差に吸着」。 */
function LoopFields({ rate, frames }: { rate: number; frames: number }) {
  const loop = useAudioStore((s) =>
    s.local && 'loop' in s.local ? (s.local.loop ?? null) : (s.view?.loop ?? null),
  );
  const snap = useAudioStore((s) => s.snapZero);
  return (
    <>
      <TimeField
        label={t.start}
        frames={loop?.start ?? null}
        rate={rate}
        max={frames - 1}
        onValue={(f) => setLoop({ start: f, end: Math.max(f + 1, loop?.end ?? frames) })}
        onSeal={sealHandle}
      />
      <TimeField
        label={t.end}
        frames={loop?.end ?? null}
        rate={rate}
        min={1}
        max={frames}
        onValue={(f) => setLoop({ start: Math.min(f - 1, loop?.start ?? 0), end: f })}
        onSeal={sealHandle}
      />
      <button type="button" className={editorStyles.pill} disabled={!loop} onClick={clearLoop}>
        {t.loopClear}
      </button>
      <label className={editorStyles.switchLabel}>
        <Switch checked={snap} onChange={setSnapZero} />
        {t.snapZero}
      </label>
      {!loop && <span className={styles.toolHint}>{t.loopHint}</span>}
    </>
  );
}

/**
 * 下のガラスのカプセル（再生バーの位置）を 2 段にする：上の段に再生の操作と道具の切り替え、下の段に今の道具の欄（時刻
 * の欄は時間で入れ、下に標本の位置。使い方の一言は欄の下の行）。左下・右下の表示と重ならない帯の中に置く。
 */
/**
 * スペクトルのときの下の段（M17 段 5）：選んだバンドの読み（文字だけ）と使い方の一言（カプセルの幅は増やさず、一言は
 * その幅で省略する）。面の読み上げ（`aria-describedby`）も同じ読みを使う。
 */
function SpectrumFields() {
  const node = useEditorStore((s) => targetNode(s.doc, s.selected, 'audio.eq'));
  const bypass = useAudioStore((s) => s.bypass);
  const audition = useAudioStore((s) => s.view?.audition ?? null);
  // 外した音を用意している間（長い音声の流し解き。それまでは外していない音を鳴らす）
  const preparing =
    bypass !== null && audition?.node === bypass && audition.phase.type === 'solving'
      ? audition.phase.progress
      : null;
  const ts = ja.editor.audio.spectrum;
  return (
    <div className={styles.specFields}>
      <span id={SPECTRUM_READING_ID} className={styles.specReading}>
        {node === null ? ts.usage : <BandReading node={node} />}
      </span>
      {node !== null && (
        <button
          type="button"
          className={editorStyles.pill}
          aria-pressed={bypass !== null}
          onClick={toggleBypass}
        >
          {ts.bypass}
        </button>
      )}
      {node !== null && (
        <span className={styles.specUsage}>
          {preparing !== null ? ts.bypassPreparing(`${Math.floor(preparing * 100)}%`) : ts.usage}
        </span>
      )}
      {/* 読み上げは用意している間かどうかだけ（進みの % が変わるたびに読まない） */}
      <span className={styles.srOnly} aria-live="polite">
        {preparing !== null ? ts.bypassPreparingSpoken : ''}
      </span>
    </div>
  );
}

/** 選んだバンドの読み。 */
function BandReading({ node }: { node: number }) {
  const bands = useEqBands(node);
  const band = useAudioStore((s) => s.eqBand);
  const b = band === null ? undefined : bands[band];
  return <>{b && band !== null ? bandSummary(b, band) : ja.editor.audio.spectrum.noBand}</>;
}

/** 選んだバンドの読みの要素の ID（面の `aria-describedby`）。 */
export const SPECTRUM_READING_ID = 'kura-spectrum-reading';

export function AudioCapsule() {
  const tool = useAudioStore((s) => s.tool);
  const face = useAudioStore((s) => s.face);
  const rate = useAudioStore((s) => s.view?.rate ?? 0);
  const frames = useAudioStore((s) => s.view?.frames ?? 0);
  return (
    <div
      className={`${editorStyles.capsule} ${styles.capsule}`}
      data-editor-capsule=""
      role="toolbar"
      aria-label={ja.editor.toolSettings}
    >
      <div className={styles.capsuleRow}>
        <Transport />
        <AudioToolSwitch />
      </div>
      <div className={styles.toolFields}>
        {face === 'spectrum' && <SpectrumFields />}
        {face === 'wave' && tool === 'select' && <SelectFields rate={rate} frames={frames} />}
        {face === 'wave' && tool === 'trim' && <TrimFields rate={rate} />}
        {face === 'wave' && tool === 'fade' && <FadeFields rate={rate} frames={frames} />}
        {face === 'wave' && tool === 'loop' && <LoopFields rate={rate} frames={frames} />}
      </div>
    </div>
  );
}
