import { type MouseEvent, useEffect, useId, useRef, useState } from 'react';

import glass from '../../components/glass/glass.module.css';
import { ja } from '../../i18n/ja';
import { isImeKey } from '../../lib/ime';
import { cssColor, type Hsv, hsvToRgb, hueStops, parseHex, rgbToHsv, toHex } from './color';
import styles from './Editor.module.css';
import type { PaintColor } from './stroke';

const t = ja.editor;

/** 色の見本（市松模様の上に色。押すと選ぶ）。 */
export function Swatch({
  color,
  label,
  onClick,
  pressed,
  large,
}: {
  color: PaintColor;
  label: string;
  onClick: (e: MouseEvent<HTMLButtonElement>) => void;
  pressed?: boolean;
  large?: boolean;
}) {
  return (
    <button
      type="button"
      className={large ? `${styles.swatch} ${styles.swatchLarge}` : styles.swatch}
      aria-label={label}
      aria-pressed={pressed}
      title={label}
      onClick={onClick}
    >
      <span className={styles.swatchColor} style={{ background: cssColor(color.rgba) }} />
    </button>
  );
}

/** 帯つきのつまみ（色相・彩度・明るさ・不透明度）。 */
function Slider({
  label,
  value,
  max,
  track,
  checker,
  onChange,
}: {
  label: string;
  value: number;
  max: number;
  track: string;
  checker?: boolean;
  onChange: (v: number) => void;
}) {
  const id = useId();
  return (
    <div className={styles.slider}>
      <label htmlFor={id} className={styles.sliderLabel}>
        {label}
      </label>
      <span
        className={checker ? `${styles.sliderTrack} ${styles.checkerFill}` : styles.sliderTrack}
      >
        <span className={styles.sliderGradient} style={{ background: track }} />
        <input
          id={id}
          type="range"
          className={styles.sliderInput}
          min={0}
          max={max}
          step={1}
          value={Math.round(value)}
          onChange={(e) => onChange(Number(e.target.value))}
        />
      </span>
      <span className={styles.sliderValue}>{Math.round(value)}</span>
    </div>
  );
}

/**
 * 色を選ぶ部品（自前。macOS の色パネルはアルファを選べない）：色相・彩度・明るさ・不透明度のつまみ、16 進の欄、
 * 最近の 8 色。下のカプセルの見本の上に開くガラスの面で、外を押す・esc で閉じる（開いている間はエディタのキーを
 * 受けない：`data-sheet`）。右パネルの色の引数（段 5b）では、欄の下に開く（`inline`。パネルの中でスクロールして切れない
 * よう浮かせない）。`alpha` が偽なら不透明度のつまみを出さない（アルファを持たない引数）。
 */
export function ColorPicker({
  color,
  recent,
  onChange,
  onClose,
  inline = false,
  alpha = true,
  label = t.color,
}: {
  color: PaintColor;
  recent: readonly PaintColor[];
  onChange: (color: PaintColor) => void;
  onClose: () => void;
  inline?: boolean;
  alpha?: boolean;
  label?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const hexId = useId();
  const [r, g, b, a] = color.rgba;
  // 無彩色でも色相を失わないよう、つまみの値は手元に持ち、外から色が変わったときだけ合わせ直す
  const [hsv, setHsv] = useState<Hsv>(() => rgbToHsv(r, g, b));
  const [seen, setSeen] = useState(color.rgba.join(','));
  const [hexText, setHexText] = useState(toHex(color.rgba));
  const key = color.rgba.join(',');
  if (key !== seen) {
    setSeen(key);
    setHexText(toHex(color.rgba));
    const [rr, gg, bb] = hsvToRgb(hsv.h, hsv.s, hsv.v);
    if (rr !== r || gg !== g || bb !== b) setHsv(rgbToHsv(r, g, b));
  }

  useEffect(() => {
    const onDown = (e: PointerEvent) => {
      const el = ref.current;
      if (
        el &&
        e.target instanceof Node &&
        !el.contains(e.target) &&
        !el.parentElement?.contains(e.target)
      ) {
        onClose();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !isImeKey(e)) {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener('pointerdown', onDown, true);
    window.addEventListener('keydown', onKey, true);
    return () => {
      window.removeEventListener('pointerdown', onDown, true);
      window.removeEventListener('keydown', onKey, true);
    };
  }, [onClose]);

  const setFromHsv = (next: Hsv, alpha = a) => {
    setHsv(next);
    const [nr, ng, nb] = hsvToRgb(next.h, next.s, next.v);
    onChange({ rgba: [nr, ng, nb, alpha], index: null });
  };

  const commitHex = () => {
    const parsed = parseHex(hexText);
    if (parsed) {
      setHsv(rgbToHsv(parsed[0], parsed[1], parsed[2]));
      onChange({ rgba: alpha ? parsed : [parsed[0], parsed[1], parsed[2], 255], index: null });
    } else {
      setHexText(toHex(color.rgba));
    }
  };

  const opaque = (s: number, v: number) => cssColor([...hsvToRgb(hsv.h, s, v), 255]);
  const solid = cssColor([r, g, b, 255]);
  const clear = cssColor([r, g, b, 0]);

  return (
    <div
      ref={ref}
      className={
        inline ? `${styles.picker} ${styles.pickerInline}` : `${glass.panel} ${styles.picker}`
      }
      role="dialog"
      aria-label={label}
      data-sheet=""
    >
      <div className={`${styles.pickerPreview} ${styles.checkerFill}`}>
        <span className={styles.pickerPreviewColor} style={{ background: cssColor(color.rgba) }} />
      </div>
      <Slider
        label={t.hue}
        value={hsv.h}
        max={360}
        track={`linear-gradient(to right, ${hueStops().join(', ')})`}
        onChange={(h) => setFromHsv({ ...hsv, h })}
      />
      <Slider
        label={t.saturation}
        value={hsv.s * 100}
        max={100}
        track={`linear-gradient(to right, ${opaque(0, hsv.v)}, ${opaque(1, hsv.v)})`}
        onChange={(s) => setFromHsv({ ...hsv, s: s / 100 })}
      />
      <Slider
        label={t.brightness}
        value={hsv.v * 100}
        max={100}
        track={`linear-gradient(to right, ${opaque(hsv.s, 0)}, ${opaque(hsv.s, 1)})`}
        onChange={(v) => setFromHsv({ ...hsv, v: v / 100 })}
      />
      {alpha && (
        <Slider
          label={t.opacity}
          value={a}
          max={255}
          track={`linear-gradient(to right, ${clear}, ${solid})`}
          checker
          onChange={(next) => onChange({ rgba: [r, g, b, next], index: null })}
        />
      )}
      <div className={styles.hexRow}>
        <label htmlFor={hexId} className={styles.sliderLabel}>
          {t.hex}
        </label>
        <input
          id={hexId}
          className={styles.hexInput}
          value={hexText}
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => setHexText(e.target.value)}
          onBlur={commitHex}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !isImeKey(e)) {
              e.preventDefault();
              commitHex();
            }
          }}
        />
      </div>
      {recent.length > 0 && (
        <div className={styles.pickerRecent} role="group" aria-label={t.recent}>
          {recent.map((c) => (
            <Swatch
              key={`${c.rgba.join(',')}:${c.index ?? ''}`}
              color={c}
              label={t.useRecent(toHex(c.rgba))}
              onClick={() => {
                setHsv(rgbToHsv(c.rgba[0], c.rgba[1], c.rgba[2]));
                onChange(c);
              }}
            />
          ))}
        </div>
      )}
    </div>
  );
}
