import { useCallback, useId, useState } from 'react';

import { ChevronLeftIcon } from '../../components/icons/ChevronLeftIcon';
import { ChevronRightIcon } from '../../components/icons/ChevronRightIcon';
import { MinusIcon } from '../../components/icons/MinusIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { Segmented } from '../../components/Segmented/Segmented';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import { formatCount } from '../../lib/format';
import { toHex } from './color';
import { ColorPicker, Swatch } from './ColorPicker';
import styles from './Editor.module.css';
import {
  cancelCrop,
  confirmCrop,
  setCropRect,
  setEditorFrame,
  stepEditorFrame,
  useEditorStore,
} from './editorStore';
import { NumberInput } from './ParamFields';
import { BRUSH_MAX, BRUSH_MIN, useToolStore } from './toolStore';

const t = ja.editor;

/**
 * 大きさ・許容差・切り抜きの枠の数の欄（引数パネルと同じ数の欄：打っている間は手元の文字で、空や範囲の外の値は送らず、
 * Enter・欄を離れたときに収めて確定する）。
 */
function NumberField({
  label,
  value,
  min,
  max,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  onChange: (v: number) => void;
}) {
  const id = useId();
  return (
    <span className={styles.field}>
      <label htmlFor={id} className={styles.fieldLabel}>
        {label}
      </label>
      <NumberInput
        id={id}
        className={styles.number}
        value={value}
        min={min}
        max={max}
        onValue={onChange}
        onSeal={() => undefined}
      />
    </span>
  );
}

/** 色：今の色の見本（押すと色を選ぶ部品）と最近の 8 色。 */
function ColorGroup() {
  const color = useToolStore((s) => s.color);
  const recent = useToolStore((s) => s.recent);
  const setColor = useToolStore((s) => s.setColor);
  const [open, setOpen] = useState(false);
  const close = useCallback(() => setOpen(false), []);
  return (
    <div className={styles.colorGroup}>
      <div className={styles.pickerAnchor}>
        <Swatch
          color={color}
          large
          label={t.pickColor(toHex(color.rgba))}
          pressed={open}
          onClick={() => setOpen((v) => !v)}
        />
        {open && <ColorPicker color={color} recent={recent} onChange={setColor} onClose={close} />}
      </div>
      {recent.length > 0 && (
        <div className={styles.recent} role="group" aria-label={t.recent}>
          {recent.map((c) => (
            <Swatch
              key={`${c.rgba.join(',')}:${c.index ?? ''}`}
              color={c}
              label={t.useRecent(toHex(c.rgba))}
              onClick={() => setColor(c)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/** 筆の形・大きさ・ピクセルパーフェクト（ペン・消しゴム）。 */
function BrushGroup() {
  const shape = useToolStore((s) => s.shape);
  const size = useToolStore((s) => s.size);
  const pixelPerfect = useToolStore((s) => s.pixelPerfect);
  const tools = useToolStore.getState();
  return (
    <>
      <Segmented
        label={t.shape}
        value={shape}
        options={[
          { value: 'square', label: t.square },
          { value: 'circle', label: t.circle },
        ]}
        onChange={tools.setShape}
      />
      <span className={styles.sizeGroup}>
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.withKey(t.smaller, '[')}
          title={t.withKey(t.smaller, '[')}
          disabled={size <= BRUSH_MIN}
          onClick={() => tools.stepSize(-1)}
        >
          <MinusIcon size={14} strokeWidth={2} />
        </button>
        <NumberField
          label={t.brushSize}
          value={size}
          min={BRUSH_MIN}
          max={BRUSH_MAX}
          onChange={tools.setSize}
        />
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.withKey(t.larger, ']')}
          title={t.withKey(t.larger, ']')}
          disabled={size >= BRUSH_MAX}
          onClick={() => tools.stepSize(1)}
        >
          <PlusIcon size={14} strokeWidth={2} />
        </button>
      </span>
      <label className={styles.switchLabel}>
        <Switch checked={pixelPerfect} onChange={tools.setPixelPerfect} />
        {t.pixelPerfect}
      </label>
    </>
  );
}

/** アニメの画像のコマ（「3 / 8」と ← →）。 */
function FrameGroup() {
  const frame = useEditorStore((s) => s.frame);
  const count = useEditorStore((s) => s.view?.frames.length ?? 1);
  if (count <= 1) return null;
  return (
    <div className={styles.frames} role="group" aria-label={t.frames}>
      <button
        type="button"
        className={styles.iconButton}
        aria-label={t.prevFrame}
        title={t.prevFrame}
        onClick={() => stepEditorFrame(-1)}
      >
        <ChevronLeftIcon size={14} strokeWidth={2} />
      </button>
      <span className={styles.framePosition}>
        {t.framePosition(formatCount(frame + 1), formatCount(count))}
      </span>
      <button
        type="button"
        className={styles.iconButton}
        aria-label={t.nextFrame}
        title={t.nextFrame}
        onClick={() => setEditorFrame((frame + 1) % count)}
      >
        <ChevronRightIcon size={14} strokeWidth={2} />
      </button>
    </div>
  );
}

/** 切り抜き：枠の X・Y・幅・高さと「やめる」「切り抜く」。 */
function CropGroup() {
  const crop = useEditorStore((s) => s.crop);
  if (!crop) return null;
  const b = crop.bounds;
  const r = crop.rect;
  const field = (key: 'x' | 'y' | 'w' | 'h', max: number) => (
    <NumberField
      key={key}
      label={t.rect[key]}
      value={r[key]}
      min={key === 'w' || key === 'h' ? 1 : 0}
      max={max}
      onChange={(v) => setCropRect({ ...r, [key]: v })}
    />
  );
  return (
    <>
      {b && (
        <>
          {field('x', b.w - 1)}
          {field('y', b.h - 1)}
          {field('w', b.w)}
          {field('h', b.h)}
        </>
      )}
      <span className={styles.hint}>{t.cropHint}</span>
      <button type="button" className={styles.pill} onClick={cancelCrop}>
        {t.cropCancel}
      </button>
      <button
        type="button"
        className={`${styles.pill} ${styles.pillPrimary}`}
        disabled={!b}
        onClick={confirmCrop}
      >
        {t.cropApply}
      </button>
    </>
  );
}

/**
 * 下のガラスのカプセル（再生バーの位置）：道具の設定。ペンは筆・重ね方・色、消しゴムは筆、塗りつぶしは許容差・
 * つながり方・色、スポイトと手のひらは使い方の一言。アニメの画像はコマを選べる。
 */
export function ToolSettings() {
  const tool = useToolStore((s) => s.tool);
  const blend = useToolStore((s) => s.blend);
  const tolerance = useToolStore((s) => s.tolerance);
  const connectivity = useToolStore((s) => s.connectivity);
  const contiguous = useToolStore((s) => s.contiguous);
  const indexed = useEditorStore((s) => s.view?.format === 'indexed8');
  const tools = useToolStore.getState();

  return (
    <div
      className={styles.capsule}
      data-editor-capsule=""
      role="toolbar"
      aria-label={t.toolSettings}
    >
      {tool !== 'crop' && <FrameGroup />}
      {tool === 'crop' && <CropGroup />}
      {(tool === 'pen' || tool === 'eraser') && <BrushGroup />}
      {tool === 'pen' && !indexed && (
        <Segmented
          label={t.blend}
          value={blend}
          options={[
            { value: 'over', label: t.blendOver },
            { value: 'replace', label: t.blendReplace },
          ]}
          onChange={tools.setBlend}
        />
      )}
      {tool === 'fill' && (
        <>
          {!indexed && (
            <NumberField
              label={t.tolerance}
              value={tolerance}
              min={0}
              max={255}
              onChange={tools.setTolerance}
            />
          )}
          <Segmented
            label={t.connectivity}
            value={connectivity}
            options={[
              { value: 'four', label: t.four },
              { value: 'eight', label: t.eight },
            ]}
            onChange={tools.setConnectivity}
          />
          <label className={styles.switchLabel}>
            <Switch checked={contiguous} onChange={tools.setContiguous} />
            {t.contiguous}
          </label>
        </>
      )}
      {(tool === 'pen' || tool === 'fill' || tool === 'eyedropper') && <ColorGroup />}
      {tool === 'eyedropper' && <span className={styles.hint}>{t.eyedropperHint}</span>}
      {tool === 'hand' && <span className={styles.hint}>{t.handHint}</span>}
      {indexed && (tool === 'pen' || tool === 'fill') && (
        <span className={styles.hint}>{t.indexedHint}</span>
      )}
    </div>
  );
}
