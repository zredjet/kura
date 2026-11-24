import { useMemo } from 'react';

import { MinusIcon } from '../../../components/icons/MinusIcon';
import { PlusIcon } from '../../../components/icons/PlusIcon';
import { ja } from '../../../i18n/ja';
import type { EditWritableFormatDto } from '../../../ipc/edit';
import { formatCount } from '../../../lib/format';
import { channelsText, khz } from '../../asset/describe';
import editorStyles from '../Editor.module.css';
import { originalFormatOf, useEditorStore } from '../editorStore';
import { FormatOutputRow, OpsPanel, type ParamTime } from '../OpsPanel';
import { outputChoice } from '../formats';
import styles from './Audio.module.css';
import { AUDIO_FORMATS, type AudioCaps, capsOf, lossesFor, type AudioLoss } from './audioFormats';
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

/** 元の形式が非可逆か（元のビット数を出さない）。 */
function lossyFormat(format: string | null): boolean {
  return capsOf(format)?.lossless === false;
}

/** 「48 kHz · 24 ビット · ステレオ · 3:12.500 · WAV」（レート・チャンネル・長さは今の出力、ビットは元。「未保存」は名前の横の印）。 */
export function AudioMeta() {
  const view = useAudioStore((s) => s.view);
  const source = useAudioStore((s) => s.source);
  const item = useEditorStore((s) => s.item);
  const originalFormat = useEditorStore(originalFormatOf);
  const rate = view?.rate ?? source?.rate ?? null;
  const channels = view?.channels ?? source?.channels ?? null;
  const frames = view?.frames ?? source?.frames ?? null;
  const bits =
    source?.bits && !lossyFormat(originalFormat) ? ja.asset.bitsValue(String(source.bits)) : null;
  // アイテムの今のファイルの形式（元と違う形式で保存したあとは、元の形式ではない）
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
  const meta = [...parts, format || null].filter(Boolean).join(' · ');
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
  const solving = useAudioStore((s) => s.view?.solving ?? null);
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
        {solving !== null ? (
          <span className={`${styles.cornerPill} ${styles.cornerStatus}`} aria-live="polite">
            {t.solving(`${Math.floor(solving * 100)}%`)}
          </span>
        ) : (
          (pending || building) && (
            <span className={`${styles.cornerPill} ${styles.cornerStatus}`} aria-live="polite">
              {building ? t.peaksBuilding : t.peaksPending}
            </span>
          )
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

/** 書けるもの `caps` の形式に保存したときに失うもの（今の出力のループ・マーカーと元のビット数から）。 */
function useLosses(caps: AudioCaps | null): AudioLoss[] {
  const loop = useAudioStore((s) => s.view?.loop ?? null);
  const markers = useAudioStore((s) => s.view?.markers);
  const bits = useAudioStore((s) => s.source?.bits ?? null);
  return lossesFor(caps, { loop, markers: markers ?? [], bits });
}

/** 失うものの箇条（12px tertiary）。なければ出さない。 */
function LossItems({ losses, format }: { losses: AudioLoss[]; format: string }) {
  if (losses.length === 0) return null;
  const name = ja.editor.formats[format] ?? format.toUpperCase();
  return (
    <ul className={styles.losses} aria-label={t.losses}>
      {losses.map((l) => (
        <li key={l}>{lossText(l, name)}</li>
      ))}
    </ul>
  );
}

/** 出力の形式の下の、保存で失うものの一覧。 */
export function LossList() {
  const outputs = useEditorStore((s) => s.doc.outputs);
  const originalFormat = useEditorStore(originalFormatOf);
  const eff = AUDIO_FORMATS.effectiveFormat(outputChoice(outputs), originalFormat);
  return <LossItems losses={useLosses(capsOf(eff.format))} format={eff.format} />;
}

/** 形式を選ぶシートで、選んだ形式で失うもの（保存できるものは Rust の `edit_formats` の値）。 */
export function FormatLosses({ format }: { format: EditWritableFormatDto }) {
  return <LossItems losses={useLosses(format)} format={format.format} />;
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
