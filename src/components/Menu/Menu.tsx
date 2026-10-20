import { type KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';

import { CheckIcon } from '../icons/CheckIcon';
import { ChevronRightIcon } from '../icons/ChevronRightIcon';
import { isSubmenu, type MenuAction, type MenuSubmenu, useMenuStore } from './menuStore';
import styles from './Menu.module.css';

/** 画面の端から離す距離。 */
const MARGIN = 8;
/** 横に開く面を元の面に重ねる幅（行の端と揃える。`.menu` の内側の余白と同じ）。 */
const SUB_OVERLAP = 4;

/** 開いている横の面（どのメニューの何番目の行か、キーで開いたか）。別のメニューを開いたら無効になる。 */
interface OpenSub {
  owner: unknown;
  index: number;
  focus: boolean;
}

/** 面の直下の選べる行。 */
function rowsOf(list: HTMLElement | null): HTMLButtonElement[] {
  return Array.from(
    list?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [],
  ).filter((button) => button.parentElement === list);
}

/** 選択肢の役割（チェックがあれば `menuitemradio`）。 */
function roleOf(action: MenuAction): 'menuitem' | 'menuitemradio' {
  return action.checked === undefined ? 'menuitem' : 'menuitemradio';
}

/**
 * 右クリック・「その他の操作」のメニュー（ガラスの小さな面）。
 * ↑↓ で移動、Return・Space で選ぶ、esc と外側のクリックで閉じる。▸ の項目は → で横に開き、← と esc で戻る。
 */
export function Menu() {
  const menu = useMenuStore((s) => s.menu);
  const close = useMenuStore((s) => s.close);
  const ref = useRef<HTMLDivElement>(null);
  const subRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const [opened, setOpened] = useState<OpenSub | null>(null);
  const [subPosition, setSubPosition] = useState<{ left: number; top: number } | null>(null);
  const sub = opened && menu && opened.owner === menu ? opened : null;

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

  // 横の面は行の右に置く（はみ出すなら左）。元の面を基準に置く（ガラスの面は fixed の子の基準になる）
  useLayoutEffect(() => {
    const el = ref.current;
    const panel = subRef.current;
    const row = sub ? el?.querySelector<HTMLElement>(`[data-sub-index="${sub.index}"]`) : null;
    if (!sub || !el || !panel || !row) {
      setSubPosition(null);
      return;
    }
    const outer = el.getBoundingClientRect();
    const size = panel.getBoundingClientRect();
    const right = outer.right - SUB_OVERLAP + size.width <= window.innerWidth - MARGIN;
    const left = right ? outer.width - SUB_OVERLAP : SUB_OVERLAP - size.width;
    const wanted = row.offsetTop - SUB_OVERLAP;
    const top = Math.max(
      MARGIN - outer.top,
      Math.min(wanted, window.innerHeight - MARGIN - size.height - outer.top),
    );
    setSubPosition({ left, top });
  }, [sub, position]);

  // 位置が決まって見えるようになってから、最初の項目にフォーカスする（見えない要素にはフォーカスできない）
  useEffect(() => {
    if (menu && position) {
      ref.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus();
    }
  }, [menu, position]);

  // キーで横の面を開いたら、その最初の選択肢へ
  useEffect(() => {
    if (sub?.focus && subPosition) {
      rowsOf(subRef.current)[0]?.focus();
    }
  }, [sub, subPosition]);

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

  /** 横の面を開く（キーで開いたら、その最初の選択肢へ）。 */
  const openSub = (index: number, focus: boolean) => setOpened({ owner: menu, index, focus });

  /** 横の面を閉じて、開いた行に戻る。 */
  const closeSub = () => {
    if (sub) {
      ref.current?.querySelector<HTMLElement>(`[data-sub-index="${sub.index}"]`)?.focus();
    }
    setOpened(null);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const active = document.activeElement as HTMLElement | null;
    const inSub = !!active && !!subRef.current?.contains(active);
    const items = rowsOf(inSub ? subRef.current : ref.current);
    const index = items.indexOf(active as HTMLButtonElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const step = e.key === 'ArrowDown' ? 1 : -1;
      const next = items[(index + step + items.length) % items.length];
      next?.focus();
    } else if (e.key === 'ArrowRight' && !inSub) {
      const at = active?.dataset.subIndex;
      if (at !== undefined) {
        e.preventDefault();
        openSub(Number(at), true);
      }
    } else if ((e.key === 'ArrowLeft' || e.key === 'Escape') && inSub) {
      e.preventDefault();
      closeSub();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      finish();
    } else if (e.key === 'Tab') {
      e.preventDefault();
    }
    // 開いているあいだはグリッドなどのキー操作に渡さない
    e.stopPropagation();
  };

  const openedEntry = sub ? menu.entries[sub.index] : undefined;
  const subEntry: MenuSubmenu | null = openedEntry && isSubmenu(openedEntry) ? openedEntry : null;

  const actionRow = (entry: MenuAction, key: string, onPointerEnter?: () => void) => (
    <button
      key={key}
      type="button"
      role={roleOf(entry)}
      aria-checked={entry.checked}
      className={styles.item}
      data-danger={entry.danger || undefined}
      disabled={entry.disabled}
      onClick={() => finish(entry)}
      onPointerEnter={onPointerEnter}
    >
      <span className={styles.icon} aria-hidden="true">
        {entry.checked ? <CheckIcon size={14} strokeWidth={2} /> : entry.icon}
      </span>
      <span className={styles.label}>{entry.label}</span>
      {entry.shortcut && <kbd className={styles.shortcut}>{entry.shortcut}</kbd>}
    </button>
  );

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
      {menu.entries.map((entry, i) => {
        if (entry === 'separator') {
          return <div key={`sep-${i}`} role="separator" className={styles.separator} />;
        }
        if (isSubmenu(entry)) {
          return (
            <button
              key={`${entry.label}-${i}`}
              type="button"
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={sub?.index === i}
              className={styles.item}
              data-sub-index={i}
              data-open={sub?.index === i || undefined}
              disabled={entry.disabled}
              onClick={() => openSub(i, true)}
              onPointerEnter={() => {
                if (sub?.index !== i) {
                  openSub(i, false);
                }
              }}
            >
              <span className={styles.icon} aria-hidden="true">
                {entry.icon}
              </span>
              <span className={styles.label}>{entry.label}</span>
              <span className={styles.chevron} aria-hidden="true">
                <ChevronRightIcon size={12} strokeWidth={2} />
              </span>
            </button>
          );
        }
        // ほかの行にポインタが移ったら横の面を閉じる
        return actionRow(entry, `${entry.label}-${i}`, () => setOpened(null));
      })}
      {subEntry && (
        <div
          ref={subRef}
          role="menu"
          aria-label={subEntry.label}
          className={`${styles.menu} ${styles.submenu}`}
          style={subPosition ?? { visibility: 'hidden' }}
        >
          {subEntry.submenu.map((action, j) => actionRow(action, `${action.label}-${j}`))}
        </div>
      )}
    </div>
  );
}
