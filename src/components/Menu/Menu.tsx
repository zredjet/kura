import { type KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';

import { type MenuAction, useMenuStore } from './menuStore';
import styles from './Menu.module.css';

/** 画面の端から離す距離。 */
const MARGIN = 8;

/**
 * 右クリック・「その他の操作」のメニュー（ガラスの小さな面）。
 * ↑↓ で移動、Return・Space で選ぶ、esc と外側のクリックで閉じる。
 */
export function Menu() {
  const menu = useMenuStore((s) => s.menu);
  const close = useMenuStore((s) => s.close);
  const ref = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);

  // 画面からはみ出さない位置に置く
  useLayoutEffect(() => {
    const el = ref.current;
    if (!menu || !el) {
      setPosition(null);
      return;
    }
    const rect = el.getBoundingClientRect();
    const left = Math.max(MARGIN, Math.min(menu.x, window.innerWidth - rect.width - MARGIN));
    const top = Math.max(MARGIN, Math.min(menu.y, window.innerHeight - rect.height - MARGIN));
    setPosition({ left, top });
  }, [menu]);

  // 位置が決まって見えるようになってから、最初の項目にフォーカスする（見えない要素にはフォーカスできない）
  useEffect(() => {
    if (menu && position) {
      ref.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
    }
  }, [menu, position]);

  useEffect(() => {
    if (!menu) {
      return;
    }
    const onPointerDown = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) {
        close();
      }
    };
    const onBlur = () => close();
    document.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('blur', onBlur);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown, true);
      window.removeEventListener('blur', onBlur);
    };
  }, [menu, close]);

  if (!menu) {
    return null;
  }

  const finish = (action?: MenuAction) => {
    close();
    menu.returnFocus?.focus();
    action?.onSelect();
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const items = Array.from(
      ref.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [],
    );
    const index = items.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const step = e.key === 'ArrowDown' ? 1 : -1;
      const next = items[(index + step + items.length) % items.length];
      next?.focus();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      finish();
    } else if (e.key === 'Tab') {
      e.preventDefault();
    }
    // 開いているあいだはグリッドなどのキー操作に渡さない
    e.stopPropagation();
  };

  return (
    <div
      ref={ref}
      role="menu"
      aria-label={menu.label}
      tabIndex={-1}
      className={styles.menu}
      style={position ?? { left: menu.x, top: menu.y, visibility: 'hidden' }}
      onKeyDown={onKeyDown}
      onContextMenu={(e) => e.preventDefault()}
    >
      {menu.entries.map((entry, i) =>
        entry === 'separator' ? (
          <div key={`sep-${i}`} role="separator" className={styles.separator} />
        ) : (
          <button
            key={`${entry.label}-${i}`}
            type="button"
            role="menuitem"
            className={styles.item}
            data-danger={entry.danger || undefined}
            disabled={entry.disabled}
            onClick={() => finish(entry)}
          >
            <span className={styles.icon} aria-hidden="true">
              {entry.icon}
            </span>
            <span className={styles.label}>{entry.label}</span>
            {entry.shortcut && <kbd className={styles.shortcut}>{entry.shortcut}</kbd>}
          </button>
        ),
      )}
    </div>
  );
}
