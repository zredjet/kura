import { useMemo } from 'react';

import { MinusIcon } from '../../../components/icons/MinusIcon';
import { PlusIcon } from '../../../components/icons/PlusIcon';
import { ja } from '../../../i18n/ja';
import { formatCount } from '../../../lib/format';
import { channelsText, khz } from '../../asset/describe';
import editorStyles from '../Editor.module.css';
import { useEditorStore } from '../editorStore';
import { FormatOutputRow, OpsPanel, type ParamTime } from '../OpsPanel';
import { outputChoice } from '../formats';
import styles from './Audio.module.css';
import { AUDIO_FORMATS, capsOf, lossesFor, type AudioLoss } from './audioFormats';
import { audioZoom, useAudioStore } from './audioStore';
import { LoudnessSection } from './LoudnessSection';
import { timeText } from './timeText';

export { AudioCapsule } from './AudioCapsule';

/*
 * 音声エディタの殻に置く部品（design.md §3.1 の「音声エディタ」の案文。M13 の段 6）：名前の下の一行、左下のカーソル・
 * 選択の時刻と標本の位置（波形の段を作り直している間の一言はその上）、右下の表示の幅、右パネルの「操作」（ラウドネス・
 * 出力の形式・失うもの）。波形の面は `WaveStage`、下のカプセル（再生の操作・道具の切り替え・道具の欄）は `AudioCapsule`。
 */

const t = ja.editor.audio;

/**
 * 上部の道具の置き場（音声では空。道具の切り替えは、欄と並べて下のカプセルの上の段に置く：`AudioToolSwitch`）。
 */
export function AudioTopTools() {
  return null;
}

/** 元の形式が非可逆か（ビットを出さない）。 */
function lossyExt(ext: string | undefined): boolean {
  return capsOf(AUDIO_FORMATS.formatOfExt(ext))?.lossless === false;
}

/** 「48 kHz · 24 ビット · ステレオ · 3:12.500 · WAV · 未保存」（レート・チャンネル・長さは今の出力、ビットは元）。 */
export function AudioMeta() {
  const view = useAudioStore((s) => s.view);
  const source = useAudioStore((s) => s.source);
  const item = useEditorStore((s) => s.item);
  const dirty = useEditorStore((s) => s.status?.dirty ?? false);
  const rate = view?.rate ?? source?.rate ?? null;
  const channels = view?.channels ?? source?.channels ?? null;
  const frames = view?.frames ?? source?.frames ?? null;
  const bits =
    source?.bits && !lossyExt(item?.ext) ? ja.asset.bitsValue(String(source.bits)) : null;
  const format = (AUDIO_FORMATS.formatOfExt(item?.ext) ?? item?.ext ?? '').toUpperCase();
  const parts =
    rate !== null && channels !== null && frames !== null
      ? [
          t.meta(
            ja.asset.rateValue(khz(rate)),
            bits,
            channelsText(channels),
            timeText(frames, rate),
          ),
        ]
      : [];
  const meta = [...parts, format || null, dirty ? ja.editor.unsaved : null]
    .filter(Boolean)
    .join(' · ');
  return <span className={editorStyles.meta}>{meta}</span>;
}

/** 左下のカーソル・選択の時刻と標本の位置、右下の表示の幅と − ＋。 */
export function AudioStatus() {
  const open = useEditorStore((s) => s.phase === 'open');
  const rate = useAudioStore((s) => s.view?.rate ?? 0);
  const cursor = useAudioStore((s) => s.cursor);
  const selection = useAudioStore((s) => s.selection);
  const span = useAudioStore((s) => s.window.span);
  const pending = useAudioStore((s) => s.peaksPending);
  const building = useAudioStore((s) => s.peaksBuilding);
  if (!open) return null;
  const where =
    selection && selection.end > selection.start
      ? t.selectionAt(
          timeText(selection.start, rate),
          timeText(selection.end, rate),
          timeText(selection.end - selection.start, rate),
        )
      : t.cursorAt(timeText(cursor, rate), formatCount(cursor));
  return (
    <>
      {/* 左下：カーソル・選択の時刻。波形の段を作り直している間の一言はその上の行（カプセルと重ならない幅に収める） */}
      <div className={styles.corner}>
        {(pending || building) && (
          <span className={`${styles.cornerPill} ${styles.cornerStatus}`} aria-live="polite">
            {building ? t.peaksBuilding : t.peaksPending}
          </span>
        )}
        <span className={styles.cornerPill} aria-live="off">
          {where}
        </span>
      </div>
      <div className={`${editorStyles.zoom} ${styles.zoom}`} role="group" aria-label={t.viewWidth}>
        <button
          type="button"
          className={editorStyles.iconButton}
          aria-label={t.zoomOut}
          title={t.zoomOut}
          onClick={() => audioZoom(-1)}
        >
          <MinusIcon size={14} strokeWidth={2} />
        </button>
        <span className={styles.widthValue}>{t.width(timeText(span, rate))}</span>
        <button
          type="button"
          className={editorStyles.iconButton}
          aria-label={t.zoomIn}
          title={t.zoomIn}
          onClick={() => audioZoom(1)}
        >
          <PlusIcon size={14} strokeWidth={2} />
        </button>
      </div>
    </>
  );
}

/** 失うものの文。 */
function lossText(loss: AudioLoss, format: string): string {
  switch (loss) {
    case 'loop':
      return t.lossLoop(format);
    case 'markers':
      return t.lossMarkers(format);
    case 'lossy':
      return t.lossLossy(format);
    case 'bits':
      return t.lossBits(format);
  }
}

/** 出力の形式の下の、保存で失うものの一覧（12px tertiary の箇条）。 */
export function LossList() {
  const outputs = useEditorStore((s) => s.doc.outputs);
  const item = useEditorStore((s) => s.item);
  const loop = useAudioStore((s) => s.view?.loop ?? null);
  const markers = useAudioStore((s) => s.view?.markers);
  const bits = useAudioStore((s) => s.source?.bits ?? null);
  const eff = AUDIO_FORMATS.effectiveFormat(
    outputChoice(outputs),
    AUDIO_FORMATS.formatOfExt(item?.ext),
  );
  const losses = lossesFor(capsOf(eff.format), { loop, markers: markers ?? [], bits });
  if (losses.length === 0) return null;
  const name = ja.editor.formats[eff.format] ?? eff.format.toUpperCase();
  return (
    <ul className={styles.losses} aria-label={t.losses}>
      {losses.map((l) => (
        <li key={l}>{lossText(l, name)}</li>
      ))}
    </ul>
  );
}

/** 右パネルの「操作」（標本の引数は時間の欄。ラウドネス・出力の形式・失うもの）。 */
export function AudioOps() {
  const nodes = useAudioStore((s) => s.view?.nodes);
  const time = useMemo<ParamTime>(
    () => ({
      rate: (node) => nodes?.find((n) => n.id === node)?.inRate ?? null,
      frames: (node) => nodes?.find((n) => n.id === node)?.inFrames ?? null,
    }),
    [nodes],
  );
  return (
    <OpsPanel
      time={time}
      output={
        <>
          <LoudnessSection />
          <FormatOutputRow />
          <LossList />
        </>
      }
    />
  );
}
