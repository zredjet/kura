import type { KeyboardEvent, MouseEvent } from 'react';

import { InlineName } from '../../components/InlineName/InlineName';
import { type MenuEntry, openMenuAt, openMenuBelow } from '../../components/Menu/menuStore';
import { PlaylistIcon } from '../../components/icons/PlaylistIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { ShuffleIcon } from '../../components/icons/ShuffleIcon';
import { ja } from '../../i18n/ja';
import type { PlaylistSummaryDto } from '../../ipc/playlists';
import { formatCount } from '../../lib/format';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { useDragStore } from '../library/dragState';
import {
  createPlaylistInteractive,
  deletePlaylistWithConfirm,
  playPlaylist,
  renamePlaylist,
} from '../playlists/playlistActions';
import { ROW_ATTR } from './Sidebar';
import styles from './Sidebar.module.css';

/**
 * サイドバーのプレイリスト（Sidebar.dc.html）。＋ で新しく作り、右クリックで再生・改名・削除。
 * 手動のプレイリストには、グリッドのタイル・フォルダをドラッグして入れられる。
 */
export function PlaylistList() {
  const playlists = useSidebarStore((s) => s.playlists);
  const editing = useSidebarStore((s) => s.editing);
  const setEditing = useSidebarStore((s) => s.setEditing);
  const current = useViewStore((s) => s.playlist);
  const openPlaylist = useViewStore((s) => s.openPlaylist);
  const dropTarget = useDragStore((s) => s.target);
  const t = ja.playlistMenu;

  const menuFor = (playlist: PlaylistSummaryDto): MenuEntry[] => [
    { label: t.play, onSelect: () => void playPlaylist(playlist.id, playlist.name) },
    'separator',
    {
      label: t.rename,
      shortcut: '↩',
      onSelect: () => setEditing({ kind: 'renamePlaylist', id: playlist.id }),
    },
    'separator',
    {
      label: t.delete,
      shortcut: '⌘⌫',
      danger: true,
      onSelect: () => void deletePlaylistWithConfirm(playlist),
    },
  ];

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, playlist: PlaylistSummaryDto) => {
    const plain = !e.metaKey && !e.altKey && !e.ctrlKey && !e.shiftKey;
    let handled = true;
    if (e.key === 'Enter' && plain) {
      setEditing({ kind: 'renamePlaylist', id: playlist.id });
    } else if (e.key === 'Enter' && e.ctrlKey) {
      openMenuBelow(e.currentTarget, t.label(playlist.name), menuFor(playlist));
    } else if (e.key === 'Backspace' && e.metaKey) {
      void deletePlaylistWithConfirm(playlist);
    } else {
      handled = false;
    }
    if (handled) {
      e.preventDefault();
      e.stopPropagation();
    }
  };

  const onContextMenu = (e: MouseEvent, playlist: PlaylistSummaryDto) => {
    e.preventDefault();
    openMenuAt(e.clientX, e.clientY, t.label(playlist.name), menuFor(playlist));
  };

  const icon = (playlist: PlaylistSummaryDto) =>
    playlist.kind === 'smart' ? (
      <ShuffleIcon size={16} strokeWidth={1.8} />
    ) : (
      <PlaylistIcon size={16} strokeWidth={1.8} />
    );

  return (
    <section className={styles.section} aria-label={ja.sidebar.playlists}>
      <div className={styles.sectionHeader}>
        <h2 className={styles.sectionTitle}>{ja.sidebar.playlists}</h2>
        <button
          type="button"
          className={styles.addButton}
          aria-label={ja.sidebar.addPlaylist}
          onClick={() => void createPlaylistInteractive()}
        >
          <PlusIcon size={14} strokeWidth={2} />
        </button>
      </div>
      {playlists.map((playlist) => {
        const selected = current === playlist.id;
        if (editing?.kind === 'renamePlaylist' && editing.id === playlist.id) {
          return (
            <div key={playlist.id} className={styles.row}>
              <span className={styles.rowIcon}>{icon(playlist)}</span>
              <InlineName
                initial={playlist.name}
                label={ja.sidebar.playlistName}
                onCommit={(name) => {
                  setEditing(null);
                  void renamePlaylist(playlist.id, name);
                }}
                onCancel={() => setEditing(null)}
              />
            </div>
          );
        }
        // 手動のプレイリストだけ、ドラッグで入れられる
        const droppable = playlist.kind === 'manual';
        return (
          <button
            key={playlist.id}
            type="button"
            className={styles.row}
            aria-current={selected ? 'page' : undefined}
            {...{ [ROW_ATTR]: '' }}
            data-drop-playlist={droppable ? playlist.id : undefined}
            data-drop={
              dropTarget?.kind === 'playlist' && dropTarget.id === playlist.id
                ? 'inside'
                : undefined
            }
            onClick={(e) => {
              e.currentTarget.focus();
              if (!selected) {
                openPlaylist(playlist.id);
              }
            }}
            onKeyDown={(e) => onKeyDown(e, playlist)}
            onContextMenu={(e) => onContextMenu(e, playlist)}
          >
            <span className={styles.rowIcon}>{icon(playlist)}</span>
            <span className={styles.rowLabel}>{playlist.name}</span>
            <span className={styles.count}>{formatCount(playlist.itemCount)}</span>
          </button>
        );
      })}
    </section>
  );
}
