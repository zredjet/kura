import { type KeyboardEvent, type MouseEvent, useMemo } from 'react';

import { InlineName } from '../../components/InlineName/InlineName';
import { type MenuEntry, openMenuAt, openMenuBelow } from '../../components/Menu/menuStore';
import { ChevronRightIcon } from '../../components/icons/ChevronRightIcon';
import { FolderIcon } from '../../components/icons/FolderIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { UnitIcon } from '../../components/icons/UnitIcon';
import { ja } from '../../i18n/ja';
import type { FolderNodeDto } from '../../ipc/folders';
import type { ListView } from '../../ipc/items';
import { formatCount } from '../../lib/format';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import {
  createFolderNamed,
  deleteFolderWithConfirm,
  makeFolderUnit,
  moveFolderTo,
  pickFolderDestination,
  renameFolder,
  setFolderColor,
  stopUnit,
} from '../library/actions';
import { useDragStore, watchDrag } from '../library/dragState';
import { ROW_ATTR } from './Sidebar';
import styles from './Sidebar.module.css';
import { childrenMap, visibleRows } from './tree';

const COLORS = ['blue', 'violet', 'amber', 'teal'] as const;

/** 子フォルダは、自分に色がなければ親の色を引き継ぐ（Sidebar.dc.html）。 */
function effectiveColors(folders: readonly FolderNodeDto[]): Map<number, string | null> {
  const byId = new Map(folders.map((f) => [f.id, f]));
  const out = new Map<number, string | null>();
  for (const folder of folders) {
    let current: FolderNodeDto | undefined = folder;
    let color: string | null = null;
    for (let depth = 0; current && depth <= folders.length; depth += 1) {
      if (current.color) {
        color = current.color;
        break;
      }
      current = current.parentId === null ? undefined : byId.get(current.parentId);
    }
    out.set(folder.id, color);
  }
  return out;
}

/** フォルダの木（作成・改名・色・移動・削除・ドラッグで所属、キー操作）。 */
export function FolderTree({ current }: { current: ListView | null }) {
  const folders = useSidebarStore((s) => s.folders);
  const expanded = useSidebarStore((s) => s.expanded);
  const editing = useSidebarStore((s) => s.editing);
  const setEditing = useSidebarStore((s) => s.setEditing);
  const setExpanded = useSidebarStore((s) => s.setExpanded);
  const setView = useViewStore((s) => s.setView);
  const openUnit = useViewStore((s) => s.openUnit);
  const dropTarget = useDragStore((s) => s.target);

  const rows = useMemo(
    () => visibleRows(folders, (id) => expanded[id] === true || editingParent(editing) === id),
    [folders, expanded, editing],
  );
  const colors = useMemo(() => effectiveColors(folders), [folders]);
  const children = useMemo(() => childrenMap(folders), [folders]);

  const menuFor = (folder: FolderNodeDto): MenuEntry[] => [
    {
      label: ja.folderMenu.newSubfolder,
      shortcut: '⌘⇧N',
      onSelect: () => setEditing({ kind: 'create', parentId: folder.id }),
    },
    {
      label: ja.folderMenu.rename,
      shortcut: '↩',
      onSelect: () => setEditing({ kind: 'rename', id: folder.id }),
    },
    'separator',
    {
      label: ja.colors.none,
      icon: <span className={styles.swatch} />,
      disabled: folder.color === null,
      onSelect: () => void setFolderColor(folder.id, null),
    },
    ...COLORS.map((color) => ({
      label: ja.colors[color],
      icon: <span className={styles.swatch} data-color={color} />,
      disabled: folder.color === color,
      onSelect: () => void setFolderColor(folder.id, color),
    })),
    'separator',
    { label: ja.folderMenu.move, onSelect: () => void pickFolderDestination(folder) },
    ...(folder.unitId === null
      ? [
          {
            label: ja.folderMenu.useAsUnit,
            shortcut: '⌘G',
            onSelect: () => void makeFolderUnit(folder),
          },
        ]
      : [
          {
            label: ja.folderMenu.openUnit,
            onSelect: () => openUnit(folder.unitId as number),
          },
          {
            label: ja.folderMenu.stopUnit,
            onSelect: () => void stopUnit(folder.unitId as number),
          },
        ]),
    'separator',
    {
      label: ja.folderMenu.delete,
      shortcut: '⌘⌫',
      danger: true,
      onSelect: () => void deleteFolderWithConfirm(folder),
    },
  ];

  const siblingsOf = (folder: FolderNodeDto) =>
    (children.get(folder.parentId) ?? []).map((f) => f.id);

  const onRowKeyDown = (
    e: KeyboardEvent<HTMLButtonElement>,
    folder: FolderNodeDto,
    hasChildren: boolean,
  ) => {
    const plain = !e.metaKey && !e.altKey && !e.ctrlKey && !e.shiftKey;
    let handled = true;
    if (e.key === 'Enter' && plain) {
      setEditing({ kind: 'rename', id: folder.id });
    } else if (e.key === 'Enter' && e.ctrlKey) {
      openMenuBelow(e.currentTarget, ja.folderMenu.label(folder.name), menuFor(folder));
    } else if (e.key === 'ArrowRight' && plain && hasChildren) {
      setExpanded(folder.id, true);
    } else if (e.key === 'ArrowLeft' && plain && hasChildren) {
      setExpanded(folder.id, false);
    } else if (e.key === 'Backspace' && e.metaKey) {
      void deleteFolderWithConfirm(folder);
    } else if ((e.key === 'g' || e.key === 'G') && e.metaKey && !e.shiftKey) {
      if (folder.unitId === null) {
        void makeFolderUnit(folder);
      }
    } else if ((e.key === 'n' || e.key === 'N') && e.metaKey && e.shiftKey) {
      setEditing({ kind: 'create', parentId: folder.id });
    } else if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && e.metaKey && e.altKey) {
      // 同じ親の中で 1 つ上・下へ（上へは前のフォルダの前、下へは次のフォルダの後ろ。端では動かない）
      const ids = siblingsOf(folder);
      const index = ids.indexOf(folder.id);
      const up = e.key === 'ArrowUp';
      if (up ? index > 0 : index < ids.length - 1) {
        const before = up ? ids[index - 1] : ids[index + 2];
        void moveFolderTo(folder.id, folder.parentId, before ?? null);
      }
    } else {
      handled = false;
    }
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  const onContextMenu = (e: MouseEvent, folder: FolderNodeDto) => {
    e.preventDefault();
    openMenuAt(e.clientX, e.clientY, ja.folderMenu.label(folder.name), menuFor(folder));
  };

  return (
    <section className={styles.section} aria-label={ja.sidebar.folders}>
      {/* いちばん上へ移すときは見出しに落とす */}
      <div
        className={styles.sectionHeader}
        data-drop-folders-root=""
        data-drop={dropTarget?.kind === 'root' ? 'inside' : undefined}
      >
        <h2 className={styles.sectionTitle}>{ja.sidebar.folders}</h2>
        <button
          type="button"
          className={styles.addButton}
          aria-label={ja.sidebar.addFolder}
          onClick={() => setEditing({ kind: 'create', parentId: null })}
        >
          <PlusIcon size={14} strokeWidth={2} />
        </button>
      </div>
      {editing?.kind === 'create' && editing.parentId === null && (
        <NewFolderRow depth={0} parentId={null} />
      )}
      {rows.map((row) => {
        const folder = row.node;
        const selected = current?.kind === 'folder' && current.id === folder.id;
        const renaming = editing?.kind === 'rename' && editing.id === folder.id;
        const color = colors.get(folder.id) ?? null;
        const rowHint =
          dropTarget?.kind === 'folder' && dropTarget.id === folder.id
            ? dropTarget.position
            : undefined;
        return (
          <div key={folder.id} className={styles.folderItem}>
            <div
              className={styles.folderRow}
              data-color={color ?? undefined}
              data-drop={rowHint}
              data-drop-folder={folder.id}
              style={{ paddingLeft: `calc(var(--space-1) + ${row.depth} * 18px)` }}
            >
              <button
                type="button"
                className={styles.disclosure}
                tabIndex={-1}
                aria-label={
                  row.expanded ? ja.sidebar.collapse(folder.name) : ja.sidebar.expand(folder.name)
                }
                aria-hidden={!row.hasChildren}
                disabled={!row.hasChildren}
                data-open={row.expanded}
                onClick={() => setExpanded(folder.id, !row.expanded)}
              >
                <ChevronRightIcon size={12} strokeWidth={2.4} />
              </button>
              {renaming ? (
                <span className={styles.editing}>
                  <span className={styles.folderIcon} data-top={row.depth === 0}>
                    <FolderIcon size={16} strokeWidth={1.8} />
                  </span>
                  <InlineName
                    initial={folder.name}
                    label={ja.sidebar.folderName}
                    onCommit={(name) => {
                      setEditing(null);
                      void renameFolder(folder.id, name);
                    }}
                    onCancel={() => setEditing(null)}
                  />
                </span>
              ) : (
                <button
                  type="button"
                  className={styles.folderButton}
                  aria-current={selected ? 'page' : undefined}
                  aria-expanded={row.hasChildren ? row.expanded : undefined}
                  {...{ [ROW_ATTR]: '' }}
                  onClick={(e) => {
                    // WebKit ではボタンを押してもフォーカスが移らないので移す（続けて ⌘G などを押せるように）
                    e.currentTarget.focus();
                    if (!selected) {
                      setView({ kind: 'folder', id: folder.id });
                    }
                  }}
                  onKeyDown={(e) => onRowKeyDown(e, folder, row.hasChildren)}
                  onContextMenu={(e) => onContextMenu(e, folder)}
                  onPointerDown={(e) =>
                    watchDrag(e, () => ({
                      payload: { kind: 'folder', id: folder.id },
                      label: folder.name,
                    }))
                  }
                >
                  <span className={styles.folderIcon} data-top={row.depth === 0}>
                    <FolderIcon size={16} strokeWidth={1.8} />
                  </span>
                  <span className={styles.rowLabel}>{folder.name}</span>
                  {folder.unitId !== null && (
                    <span className={styles.unitMark} title={ja.sidebar.usedAsUnit}>
                      <UnitIcon size={12} strokeWidth={2} />
                    </span>
                  )}
                  {folder.count !== null && (
                    <span className={styles.count}>{formatCount(folder.count)}</span>
                  )}
                </button>
              )}
            </div>
            {editing?.kind === 'create' && editing.parentId === folder.id && (
              <NewFolderRow depth={row.depth + 1} parentId={folder.id} />
            )}
          </div>
        );
      })}
    </section>
  );
}

function editingParent(
  editing: ReturnType<typeof useSidebarStore.getState>['editing'],
): number | null {
  return editing?.kind === 'create' ? editing.parentId : null;
}

/** 新しいフォルダの名前を入れる行。 */
function NewFolderRow({ depth, parentId }: { depth: number; parentId: number | null }) {
  const setEditing = useSidebarStore((s) => s.setEditing);
  return (
    <div
      className={styles.folderRow}
      style={{ paddingLeft: `calc(var(--space-1) + ${depth} * 18px)` }}
    >
      <span className={styles.disclosure} />
      <span className={styles.editing}>
        <span className={styles.folderIcon} data-top={depth === 0}>
          <FolderIcon size={16} strokeWidth={1.8} />
        </span>
        <InlineName
          initial=""
          label={ja.sidebar.folderName}
          onCommit={(name) => {
            setEditing(null);
            void createFolderNamed(parentId, name);
          }}
          onCancel={() => setEditing(null)}
        />
      </span>
    </div>
  );
}
