import type { KeyboardEvent, MouseEvent } from 'react';

import { InlineName } from '../../components/InlineName/InlineName';
import { type MenuEntry, openMenuAt, openMenuBelow } from '../../components/Menu/menuStore';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { SparkleIcon } from '../../components/icons/SparkleIcon';
import { ja } from '../../i18n/ja';
import type { ListView } from '../../ipc/items';
import type { SmartFolderDto } from '../../ipc/search';
import { formatCount } from '../../lib/format';
import { useSearchStore } from '../../stores/searchStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { deleteSmartWithConfirm, renameSmartFolder } from '../search/searchActions';
import { ROW_ATTR } from './Sidebar';
import styles from './Sidebar.module.css';

/** サイドバーのスマートフォルダ（Sidebar.dc.html）。＋ で新しい検索を作り、右クリックで改名・条件の編集・削除。 */
export function SmartFolderList({ current }: { current: ListView | null }) {
  const folders = useSidebarStore((s) => s.smartFolders);
  const editing = useSidebarStore((s) => s.editing);
  const setEditing = useSidebarStore((s) => s.setEditing);
  const setView = useViewStore((s) => s.setView);
  const openPalette = useSearchStore((s) => s.openPalette);

  const menuFor = (folder: SmartFolderDto): MenuEntry[] => [
    {
      label: ja.smartMenu.rename,
      shortcut: '↩',
      onSelect: () => setEditing({ kind: 'renameSmart', id: folder.id }),
    },
    {
      label: ja.smartMenu.editQuery,
      onSelect: () =>
        openPalette({ initial: [folder.query], editing: { id: folder.id, name: folder.name } }),
    },
    'separator',
    {
      label: ja.smartMenu.delete,
      shortcut: '⌘⌫',
      danger: true,
      onSelect: () => void deleteSmartWithConfirm(folder),
    },
  ];

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, folder: SmartFolderDto) => {
    const plain = !e.metaKey && !e.altKey && !e.ctrlKey && !e.shiftKey;
    let handled = true;
    if (e.key === 'Enter' && plain) {
      setEditing({ kind: 'renameSmart', id: folder.id });
    } else if (e.key === 'Enter' && e.ctrlKey) {
      openMenuBelow(e.currentTarget, ja.smartMenu.label(folder.name), menuFor(folder));
    } else if (e.key === 'Backspace' && e.metaKey) {
      void deleteSmartWithConfirm(folder);
    } else {
      handled = false;
    }
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  const onContextMenu = (e: MouseEvent, folder: SmartFolderDto) => {
    e.preventDefault();
    openMenuAt(e.clientX, e.clientY, ja.smartMenu.label(folder.name), menuFor(folder));
  };

  return (
    <section className={styles.section} aria-label={ja.sidebar.smartFolders}>
      <div className={styles.sectionHeader}>
        <h2 className={styles.sectionTitle}>{ja.sidebar.smartFolders}</h2>
        <button
          type="button"
          className={styles.addButton}
          aria-label={ja.sidebar.addSmartFolder}
          onClick={() => openPalette({ createSmart: true })}
        >
          <PlusIcon size={14} strokeWidth={2} />
        </button>
      </div>
      {folders.map((folder) => {
        const selected = current?.kind === 'smart' && current.id === folder.id;
        if (editing?.kind === 'renameSmart' && editing.id === folder.id) {
          return (
            <div key={folder.id} className={styles.row}>
              <span className={styles.rowIcon}>
                <SparkleIcon size={16} strokeWidth={1.8} />
              </span>
              <InlineName
                initial={folder.name}
                label={ja.sidebar.smartFolderName}
                onCommit={(name) => {
                  setEditing(null);
                  void renameSmartFolder(folder.id, name);
                }}
                onCancel={() => setEditing(null)}
              />
            </div>
          );
        }
        return (
          <button
            key={folder.id}
            type="button"
            className={styles.row}
            aria-current={selected ? 'page' : undefined}
            title={folder.query}
            {...{ [ROW_ATTR]: '' }}
            onClick={(e) => {
              e.currentTarget.focus();
              if (!selected) {
                setView({ kind: 'smart', id: folder.id });
              }
            }}
            onKeyDown={(e) => onKeyDown(e, folder)}
            onContextMenu={(e) => onContextMenu(e, folder)}
          >
            <span className={styles.rowIcon}>
              <SparkleIcon size={16} strokeWidth={1.8} />
            </span>
            <span className={styles.rowLabel}>{folder.name}</span>
            {folder.count !== null && (
              <span className={styles.count}>{formatCount(folder.count)}</span>
            )}
          </button>
        );
      })}
    </section>
  );
}
