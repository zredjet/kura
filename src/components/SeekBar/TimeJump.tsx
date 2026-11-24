import { useEffect, useId, useRef, useState } from 'react';

import { ja } from '../../i18n/ja';
import { formatDuration } from '../../lib/format';
import { isImeKey } from '../../lib/ime';
import { clampSeek } from '../../lib/seek';
import { parseTime } from '../../lib/time';
import styles from './SeekBar.module.css';

const t = ja.seek;

/**
 * 今の時刻のボタン（ADR-230）：押すと欄になり、今の位置が「1:23.4」で全選択されて入る。「1:23.4」「83.4」「1:02:03.5」
 * （全角も）を打って Enter で飛ぶ（終わりの手前で止める）。esc・欄を離れると取り消す（離れたときに飛ばないのは、
 * ほかのボタンを押したときに二重に動かないため）。読めない文字の Enter は欄を開いたまま `aria-invalid`。
 * 日本語入力の確定の Enter は読まない。
 */
export function TimeJump({
  positionMs,
  durationMs,
  onJump,
  tenths = false,
  disabled = false,
}: {
  positionMs: number;
  durationMs: number | null;
  onJump: (ms: number) => void;
  /** ボタンの時刻を 0.1 秒まで出す（ドラッグ中）。 */
  tenths?: boolean;
  disabled?: boolean;
}) {
  const id = useId();
  const [text, setText] = useState<string | null>(null);
  const [bad, setBad] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  // 欄を閉じたあとにボタンへフォーカスを戻すか（Enter・esc で閉じたとき）
  const refocus = useRef(false);
  const editing = text !== null;

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    } else if (refocus.current) {
      refocus.current = false;
      buttonRef.current?.focus();
    }
  }, [editing]);

  const close = (focusButton: boolean) => {
    refocus.current = focusButton;
    setText(null);
    setBad(false);
  };

  const shown = formatDuration(positionMs, tenths);
  // ドラッグ中に 0.1 秒の桁が増えてもバーがずれないよう、長さの 0.1 秒の形の字数だけ幅を取っておく
  const reserve = {
    minWidth: `calc(${formatDuration(durationMs ?? 0, true).length}ch + var(--space-3))`,
  };
  if (!editing) {
    return (
      <button
        ref={buttonRef}
        type="button"
        className={styles.jump}
        style={reserve}
        aria-label={t.jump(formatDuration(positionMs))}
        title={t.jumpHint}
        disabled={disabled}
        onClick={() => setText(formatDuration(positionMs, true))}
      >
        {shown}
      </button>
    );
  }
  return (
    <>
      <label htmlFor={id} className={styles.srOnly}>
        {t.field}
      </label>
      <input
        ref={inputRef}
        id={id}
        className={styles.jumpInput}
        type="text"
        inputMode="decimal"
        spellCheck={false}
        autoComplete="off"
        title={bad ? t.badTime : t.fieldHint}
        aria-invalid={bad || undefined}
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          setBad(false);
        }}
        onBlur={() => close(false)}
        onKeyDown={(e) => {
          if (isImeKey(e)) return;
          if (e.key === 'Enter') {
            e.preventDefault();
            const sec = parseTime(text);
            if (sec === null) {
              setBad(true);
              return;
            }
            onJump(clampSeek(sec * 1000, durationMs));
            close(true);
          } else if (e.key === 'Escape') {
            // クイックルック・シアターを閉じない
            e.preventDefault();
            e.stopPropagation();
            close(true);
          }
        }}
      />
    </>
  );
}
