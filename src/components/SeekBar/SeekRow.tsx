import { useState } from 'react';

import { ja } from '../../i18n/ja';
import { formatDuration } from '../../lib/format';
import { SeekBar } from './SeekBar';
import styles from './SeekBar.module.css';
import { TimeJump } from './TimeJump';

/**
 * シークの行（design.md §4「シークバー」・ADR-230）：左に今の時刻のボタン（押すと時刻を打って飛ぶ。ドラッグ中は
 * 0.1 秒まで）、まん中にシークバー、右に長さ。シアターの再生バーとクイックルックのカプセルで使う。
 */
export function SeekRow({
  positionMs,
  durationMs,
  onSeek,
  onScrubStart,
  onScrubEnd,
  resetKey = null,
  disabled = false,
  className,
}: {
  positionMs: number;
  durationMs: number | null;
  onSeek: (ms: number) => void;
  onScrubStart?: () => void;
  onScrubEnd?: () => void;
  resetKey?: unknown;
  disabled?: boolean;
  className?: string;
}) {
  const [scrubbing, setScrubbing] = useState(false);
  // ドラッグを始めたら・アイテムが替わったら時刻の欄を閉じる（開いたままだと、あとの Enter でドラッグの位置から
  // 打った時刻へ戻る・前のアイテムのつもりの時刻で次のアイテムを飛ばすため）
  const [drags, setDrags] = useState(0);
  const length = durationMs !== null && durationMs > 0 ? durationMs : null;
  const off = disabled || length === null;
  return (
    <div className={[styles.row, className].filter(Boolean).join(' ')}>
      <TimeJump
        key={`${String(resetKey)}:${drags}`}
        positionMs={positionMs}
        durationMs={length}
        onJump={onSeek}
        tenths={scrubbing}
        disabled={off}
      />
      <SeekBar
        label={ja.seek.label}
        positionMs={positionMs}
        durationMs={length}
        onSeek={onSeek}
        onScrubStart={() => {
          setScrubbing(true);
          setDrags((n) => n + 1);
          onScrubStart?.();
        }}
        onScrubEnd={() => {
          setScrubbing(false);
          onScrubEnd?.();
        }}
        resetKey={resetKey}
        disabled={disabled}
      />
      <span className={styles.duration}>{length !== null ? formatDuration(length) : '--:--'}</span>
    </div>
  );
}
