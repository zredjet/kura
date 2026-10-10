import { type KeyboardEvent, type ReactNode, useEffect } from 'react';

import glass from '../../components/glass/glass.module.css';
import { AllItemsIcon } from '../../components/icons/AllItemsIcon';
import { ClockIcon } from '../../components/icons/ClockIcon';
import { DiceIcon } from '../../components/icons/DiceIcon';
import { TrashIcon } from '../../components/icons/TrashIcon';
import { UnsortedIcon } from '../../components/icons/UnsortedIcon';
import { UntaggedIcon } from '../../components/icons/UntaggedIcon';
import { ja } from '../../i18n/ja';
import type { ListView } from '../../ipc/items';
import { formatCount } from '../../lib/format';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { FolderTree } from './FolderTree';
import { LibrarySwitcher } from './LibrarySwitcher';
import styles from './Sidebar.module.css';

/** 行のボタンに付ける印（↑↓ でたどるため）。 */
export const ROW_ATTR = 'data-sidebar-row';

type LibraryKind = 'all' | 'recent' | 'unsorted' | 'untagged' | 'random' | 'trash';

const LIBRARY_ROWS: { kind: LibraryKind; label: string; icon: ReactNode }[] = [
  { kind: 'all', label: ja.sidebar.all, icon: <AllItemsIcon size={16} strokeWidth={1.8} /> },
  { kind: 'recent', label: ja.sidebar.recent, icon: <ClockIcon size={16} strokeWidth={1.8} /> },
  {
    kind: 'unsorted',
    label: ja.sidebar.unsorted,
    icon: <UnsortedIcon size={16} strokeWidth={1.8} />,
  },
  {
    kind: 'untagged',
    label: ja.sidebar.untagged,
    icon: <UntaggedIcon size={16} strokeWidth={1.8} />,
  },
  { kind: 'random', label: ja.sidebar.random, icon: <DiceIcon size={16} strokeWidth={1.8} /> },
  { kind: 'trash', label: ja.sidebar.trash, icon: <TrashIcon size={16} strokeWidth={1.8} /> },
];

function viewFor(kind: LibraryKind): ListView {
  // ランダムは開くたびに新しい並び（シードは時刻から）
  return kind === 'random' ? { kind, seed: Date.now() } : { kind };
}

/**
 * サイドバー（Sidebar.dc.html）。先頭の行にはネイティブの信号機が重なる。
 * ライブラリの切り替え・ライブラリの項目・フォルダの木。スマートフォルダ（M4）・プレイリスト（M6）・
 * 取得（M7）・容量と設定（M9）は、その機能を入れるときに足す。
 */
export function Sidebar() {
  const counts = useSidebarStore((s) => s.counts);
  const view = useViewStore((s) => s.view);
  const parent = useViewStore((s) => s.parent);
  const setView = useViewStore((s) => s.setView);
  const current = view.kind === 'unit' ? parent : view;

  useEffect(() => {
    void useSidebarStore.getState().load();
  }, []);

  // ↑↓ で行をたどり、たどった先を開く（Finder のサイドバーと同じ）
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.defaultPrevented || e.metaKey || e.altKey || e.ctrlKey) {
      return;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      const rows = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>(`[${ROW_ATTR}]`));
      const index = rows.indexOf(document.activeElement as HTMLButtonElement);
      const next = rows[index + (e.key === 'ArrowDown' ? 1 : -1)];
      if (next) {
        e.preventDefault();
        next.focus();
        next.click();
      }
    }
    // サイドバーにいるあいだは、グリッドの 1 文字のキー操作（T・F・1〜5 など）に渡さない
    e.stopPropagation();
  };

  return (
    // 行のボタンのキー操作（↑↓ で行をたどる）をまとめて受ける。操作の対象は中のボタン
    // eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions
    <nav
      className={`${glass.panel} ${styles.sidebar}`}
      aria-label={ja.sidebar.label}
      onKeyDown={onKeyDown}
    >
      <div className={styles.titlebar} data-tauri-drag-region />
      <LibrarySwitcher />
      <div className={styles.scroll}>
        <section className={styles.section} aria-label={ja.sidebar.librarySection}>
          <h2 className={styles.sectionTitle}>{ja.sidebar.librarySection}</h2>
          {LIBRARY_ROWS.map((row) => {
            const selected = current?.kind === row.kind;
            const count = row.kind === 'random' ? null : (counts?.[row.kind] ?? null);
            return (
              <button
                key={row.kind}
                type="button"
                className={styles.row}
                aria-current={selected ? 'page' : undefined}
                {...{ [ROW_ATTR]: '' }}
                onClick={(e) => {
                  e.currentTarget.focus();
                  if (!selected || row.kind === 'random') {
                    setView(viewFor(row.kind));
                  }
                }}
              >
                <span className={styles.rowIcon}>{row.icon}</span>
                <span className={styles.rowLabel}>{row.label}</span>
                {count !== null && <span className={styles.count}>{formatCount(count)}</span>}
              </button>
            );
          })}
        </section>
        <FolderTree current={current} />
      </div>
    </nav>
  );
}
