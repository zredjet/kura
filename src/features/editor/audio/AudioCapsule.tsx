import { Segmented } from '../../../components/Segmented/Segmented';
import { Switch } from '../../../components/Switch/Switch';
import { ja } from '../../../i18n/ja';
import editorStyles from '../Editor.module.css';
import { sendParam, useEditorStore } from '../editorStore';
import { paramsOf } from '../opsCatalog';
import styles from './Audio.module.css';
import { cancelTrim, setSelection, useAudioStore } from './audioStore';
import {
  clearLoop,
  confirmTrim,
  fadeLengths,
  sealHandle,
  selectionToLoop,
  selectionToTrim,
  setFade,
  setLoop,
  setSnapZero,
  setTrimFrame,
} from './audioTools';
import { targetNode } from './handles';
import { TimeField } from './TimeField';
import { Transport } from './Transport';

const t = ja.editor.audio;

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
      <button type="button" className={editorStyles.pill} disabled={!has} onClick={selectionToTrim}>
        {t.selectionToTrim}
      </button>
      <button type="button" className={editorStyles.pill} disabled={!has} onClick={selectionToLoop}>
        {t.selectionToLoop}
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
      <span className={editorStyles.hint}>{b === null ? t.trimWaiting : t.trimHint}</span>
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
        <span className={editorStyles.hint}>{t.fadeHint}</span>
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
      {!loop && <span className={editorStyles.hint}>{t.loopHint}</span>}
    </>
  );
}

/**
 * 下のガラスのカプセル（再生バーの位置）：左に再生の操作、右に道具ごとの欄（時刻の欄は時間で入れ、下に標本の位置）。
 */
export function AudioCapsule() {
  const tool = useAudioStore((s) => s.tool);
  const rate = useAudioStore((s) => s.view?.rate ?? 0);
  const frames = useAudioStore((s) => s.view?.frames ?? 0);
  return (
    <div
      className={`${editorStyles.capsule} ${styles.capsule}`}
      data-editor-capsule=""
      role="toolbar"
      aria-label={ja.editor.toolSettings}
    >
      <Transport />
      <span className={styles.divider} aria-hidden="true" />
      <div className={styles.toolFields}>
        {tool === 'select' && <SelectFields rate={rate} frames={frames} />}
        {tool === 'trim' && <TrimFields rate={rate} />}
        {tool === 'fade' && <FadeFields rate={rate} frames={frames} />}
        {tool === 'loop' && <LoopFields rate={rate} frames={frames} />}
      </div>
    </div>
  );
}
