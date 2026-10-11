import { type ReactNode, useEffect, useRef } from 'react';

import styles from './Dialog.module.css';

interface DialogProps {
  /** 見出しの要素の id（aria-labelledby）。 */
  labelledBy: string;
  /** esc で閉じるときに呼ぶ。 */
  onClose: () => void;
  /** 小さいシート（名前の入力・確認など。幅 420px）か、広いシート（設定画面。幅 760px）。 */
  size?: 'md' | 'sm' | 'lg';
  children: ReactNode;
}

/** 開いているシート（開いた順）。esc・Tab はいちばん上のシートだけが受ける（シートの上にシートを重ねたとき）。 */
const stack: HTMLElement[] = [];

/**
 * いちばん上のシートか：中に開いているシートがなく、入れ子でないシートのうちあとに開いたもの。
 * 一緒に描いた入れ子のシートは、子の effect が先に動いて親より先に積まれるので、入れ子は DOM で見る。
 */
function isTop(dialog: HTMLElement): boolean {
  if (stack.some((other) => other !== dialog && dialog.contains(other))) {
    return false;
  }
  return !stack.slice(stack.indexOf(dialog) + 1).some((other) => !other.contains(dialog));
}

const FOCUSABLE =
  'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

/** 暗幕＋中央上部のシート（design.md §4 のダイアログ）。esc で閉じ、Tab はシートの中を回る。 */
export function Dialog({ labelledBy, onClose, size = 'md', children }: DialogProps) {
  const ref = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) {
      return;
    }
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.focus();
    stack.push(dialog);

    const onKeyDown = (e: KeyboardEvent) => {
      if (!isTop(dialog)) {
        return;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        onCloseRef.current();
        return;
      }
      if (e.key !== 'Tab') {
        return;
      }
      const items = Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE));
      const first = items[0];
      const last = items[items.length - 1];
      if (!first || !last) {
        return;
      }
      if (e.shiftKey && (document.activeElement === first || document.activeElement === dialog)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('keydown', onKeyDown);
      const index = stack.indexOf(dialog);
      if (index >= 0) {
        stack.splice(index, 1);
      }
      previous?.focus();
    };
  }, []);

  return (
    <div className={styles.scrim} data-sheet="">
      <div
        ref={ref}
        role="dialog"
        aria-modal="true"
        aria-labelledby={labelledBy}
        tabIndex={-1}
        className={
          size === 'sm'
            ? `${styles.dialog} ${styles.small}`
            : size === 'lg'
              ? `${styles.dialog} ${styles.large}`
              : styles.dialog
        }
      >
        {children}
      </div>
    </div>
  );
}
