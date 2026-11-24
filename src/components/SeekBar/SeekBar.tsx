import type { CSSProperties, KeyboardEvent } from 'react';

import { ja } from '../../i18n/ja';
import { formatDuration } from '../../lib/format';
import { clampSeek, msAtX, tooltipLeft } from '../../lib/seek';
import styles from './SeekBar.module.css';
import { useScrub } from './useScrub';

/** 吹き出しの幅（px。CSS の `.tip` の幅と同じ。左右の端で寄せるのに使う）。 */
export const TIP_WIDTH = 72;
/** フォーカス中の矢印で動かす幅（ms）。 */
export const KEY_STEP_MS = 5000;
/** フォーカス中の PageUp・PageDown で動かす幅（ms）。 */
export const PAGE_STEP_MS = 30_000;
/** フォーカス中にこの部品が受けるキー。 */
const RANGE_KEYS: ReadonlySet<string> = new Set([
  'ArrowLeft',
  'ArrowRight',
  'ArrowUp',
  'ArrowDown',
  'Home',
  'End',
  'PageUp',
  'PageDown',
]);

export interface SeekBarProps {
  /** 読み上げの名前（「再生位置」）。 */
  label: string;
  positionMs: number;
  /** 長さ（null・0 なら押せない）。 */
  durationMs: number | null;
  onSeek: (ms: number) => void;
  onScrubStart?: () => void;
  onScrubEnd?: () => void;
  /** アイテム（替わったらドラッグを捨てる）。 */
  resetKey?: unknown;
  disabled?: boolean;
  className?: string;
}

/**
 * シークバー（design.md §4「シークバー」・ADR-230）：押せる範囲 28px、溝 6px（ホバー・ドラッグ中 10px）、つまみ 16px。
 * ホバーでポインタの下の時刻、ドラッグ中は 0.1 秒までを吹き出しで出す。⌥ ドラッグは 1/10 の速さ。
 * 見た目は自前で描き、本物の `<input type="range">` を透明に重ねて読み上げ・Tab・キーを受け持たせる（押しても
 * フォーカスは移らない。Tab で乗ったときだけ ← → ↑ ↓ で ±5 秒、PageUp・PageDown で ±30 秒、Home・End。読み上げの
 * 増減は 1 秒）。
 */
export function SeekBar({
  label,
  positionMs,
  durationMs,
  onSeek,
  onScrubStart,
  onScrubEnd,
  resetKey = null,
  disabled = false,
  className,
}: SeekBarProps) {
  const length = durationMs !== null && durationMs > 0 ? durationMs : null;
  const { scrubMs, hover, handlers } = useScrub({
    durationMs: length,
    positionMs,
    onSeek,
    onScrubStart,
    onScrubEnd,
    resetKey,
    disabled,
    trackHover: true,
  });
  const enabled = !disabled && length !== null;
  const shownMs = scrubMs ?? Math.min(Math.max(0, positionMs), length ?? 0);
  const ratio = length !== null ? Math.min(1, Math.max(0, shownMs / length)) : 0;
  const pct = `${(ratio * 100).toFixed(3)}%`;

  // 吹き出し：ドラッグ中は今の値（0.1 秒）の位置、そうでなければポインタの下（秒）
  let tip: { left: number; text: string } | null = null;
  if (enabled && length !== null && hover !== null) {
    if (scrubMs !== null) {
      tip = {
        left: tooltipLeft(ratio * hover.width, hover.width, TIP_WIDTH),
        text: formatDuration(scrubMs, true),
      };
    } else {
      tip = {
        left: tooltipLeft(hover.x, hover.width, TIP_WIDTH),
        text: formatDuration(msAtX(hover.x, hover.width, length)),
      };
    }
  }

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (!enabled) {
      return;
    }
    // 修飾キーつきの矢印などは動かさない（range の既定の 1% 送りもしない）
    if ((e.metaKey || e.ctrlKey || e.altKey) && RANGE_KEYS.has(e.key)) {
      e.preventDefault();
      return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey) {
      return;
    }
    let next: number;
    switch (e.key) {
      case 'ArrowLeft':
      case 'ArrowDown':
        next = positionMs - KEY_STEP_MS;
        break;
      case 'ArrowRight':
      case 'ArrowUp':
        next = positionMs + KEY_STEP_MS;
        break;
      case 'PageDown':
        next = positionMs - PAGE_STEP_MS;
        break;
      case 'PageUp':
        next = positionMs + PAGE_STEP_MS;
        break;
      case 'Home':
        next = 0;
        break;
      case 'End':
        next = length ?? 0;
        break;
      default:
        return;
    }
    e.preventDefault();
    onSeek(clampSeek(next, length));
  };

  return (
    <div
      className={[styles.bar, className].filter(Boolean).join(' ')}
      data-scrubbing={scrubMs !== null || undefined}
      data-disabled={!enabled || undefined}
      style={{ '--pct': pct } as CSSProperties}
      {...handlers}
    >
      <span className={styles.track} aria-hidden="true">
        <span className={styles.fill} />
      </span>
      <span className={styles.thumb} aria-hidden="true" />
      {tip && (
        <span className={styles.tip} style={{ left: `${tip.left}px` }} aria-hidden="true">
          {tip.text}
        </span>
      )}
      <input
        type="range"
        className={styles.input}
        aria-label={label}
        aria-valuetext={
          length !== null
            ? ja.seek.valueText(formatDuration(shownMs), formatDuration(length))
            : undefined
        }
        min={0}
        max={length ?? 0}
        // 読み上げの増減（VoiceOver）は step で動くので 1 秒にする（"any" だと動かない）
        step={1000}
        value={shownMs}
        disabled={!enabled}
        onKeyDown={onKeyDown}
        // 読み上げの増減（キーは onKeyDown で受けて既定の動きを止めている）
        onChange={(e) => onSeek(clampSeek(Number(e.target.value), length))}
      />
    </div>
  );
}
