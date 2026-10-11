/**
 * アプリの中のドラッグ（タイル → フォルダ、フォルダ → フォルダ、ユニットの中の並べ替え）。
 *
 * Tauri のファイルドロップの処理が WebView の HTML5 ドラッグのイベントをすべて受け取ってしまうため
 * （wry が「処理済み」を返し、JS に dragover / drop が届かない）、ポインタイベントで自前に行う。
 * 落とす先は要素の data 属性で見つける：
 * - `data-drop-folder="<id>"`：フォルダの行
 * - `data-drop-folders-root`：フォルダの見出し（いちばん上へ）
 * - `data-drop-entry="<id>"`：ユニットの中のタイル（並べ替え）
 * - `data-drop-playlist="<id>"`：サイドバーの手動プレイリスト（アイテム・ユニット・フォルダを入れる。⌥ で展開を逆に）
 * - `data-drop-playlist-entry="<id>"`：プレイリストの画面の行（並べ替え）
 */

import { create } from 'zustand';

import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { type DropPosition, dropPositionAt, moveTarget } from '../sidebar/tree';
import { moveEntries } from '../../ipc/playlists';
import { addEntriesToPlaylist, addFolderToPlaylist } from '../playlists/playlistActions';
import { addToFolder, dropWithinUnit, moveFolderTo } from './actions';
import { viewedSetUnit } from './itemMenu';

export type DragPayload =
  | { kind: 'entries'; ids: number[] }
  | { kind: 'folder'; id: number }
  | { kind: 'playlistEntries'; playlistId: number; ids: number[] };

export type DropTarget =
  | { kind: 'folder'; id: number; position: DropPosition }
  | { kind: 'root' }
  | { kind: 'entry'; id: number; side: 'before' | 'after' }
  | { kind: 'playlist'; id: number }
  | { kind: 'playlistEntry'; id: number; side: 'before' | 'after' };

interface DragState {
  payload: DragPayload | null;
  /** ポインタの位置（ドラッグ中の印を出す）。 */
  x: number;
  y: number;
  label: string;
  target: DropTarget | null;
}

export const useDragStore = create<DragState>(() => ({
  payload: null,
  x: 0,
  y: 0,
  label: '',
  target: null,
}));

/** ドラッグを始めたとみなす移動量（px）。 */
const THRESHOLD = 5;

/** ポインタの下の落とし先を探す（落とせなければ null）。 */
export function findTarget(payload: DragPayload, x: number, y: number): DropTarget | null {
  const element = document.elementFromPoint(x, y);
  if (!(element instanceof HTMLElement)) {
    return null;
  }
  if (payload.kind === 'playlistEntries') {
    const row = element.closest<HTMLElement>('[data-drop-playlist-entry]');
    if (!row) {
      return null;
    }
    const id = Number(row.dataset.dropPlaylistEntry);
    if (payload.ids.includes(id)) {
      return null;
    }
    const rect = row.getBoundingClientRect();
    return { kind: 'playlistEntry', id, side: y - rect.top < rect.height / 2 ? 'before' : 'after' };
  }
  const playlistRow = element.closest<HTMLElement>('[data-drop-playlist]');
  if (playlistRow) {
    return { kind: 'playlist', id: Number(playlistRow.dataset.dropPlaylist) };
  }
  const folderRow = element.closest<HTMLElement>('[data-drop-folder]');
  if (folderRow) {
    const id = Number(folderRow.dataset.dropFolder);
    if (payload.kind === 'entries') {
      return { kind: 'folder', id, position: 'inside' };
    }
    const rect = folderRow.getBoundingClientRect();
    const position = dropPositionAt((y - rect.top) / rect.height);
    return moveTarget(useSidebarStore.getState().folders, payload.id, id, position)
      ? { kind: 'folder', id, position }
      : null;
  }
  if (payload.kind === 'folder' && element.closest('[data-drop-folders-root]')) {
    return { kind: 'root' };
  }
  const tile = element.closest<HTMLElement>('[data-drop-entry]');
  if (tile && payload.kind === 'entries' && viewedSetUnit() !== null) {
    const id = Number(tile.dataset.dropEntry);
    if (payload.ids.includes(id)) {
      return null;
    }
    const rect = tile.getBoundingClientRect();
    return { kind: 'entry', id, side: x - rect.left < rect.width / 2 ? 'before' : 'after' };
  }
  return null;
}

/** 落とした先に合わせて操作する（`alt` は ⌥ を押していたか）。 */
export function performDrop(payload: DragPayload, target: DropTarget, alt = false): void {
  if (target.kind === 'playlist') {
    const playlist = useSidebarStore.getState().playlists.find((p) => p.id === target.id);
    if (playlist && payload.kind === 'entries') {
      void addEntriesToPlaylist(playlist, payload.ids, alt);
    } else if (playlist && payload.kind === 'folder') {
      void addFolderToPlaylist(playlist, payload.id, alt);
    }
    return;
  }
  if (payload.kind === 'playlistEntries') {
    if (target.kind === 'playlistEntry') {
      const order = currentPlaylistOrder();
      const before =
        target.side === 'before' ? target.id : (order[order.indexOf(target.id) + 1] ?? null);
      moveEntries(payload.playlistId, payload.ids, before)
        .then(() => onPlaylistMoved())
        .catch((error: unknown) => console.warn('並べ替えられませんでした', error));
    }
    return;
  }
  if (payload.kind === 'entries') {
    if (target.kind === 'folder') {
      void addToFolder(payload.ids, target.id);
    } else if (target.kind === 'entry') {
      const order = useViewStore.getState().view.kind === 'unit' ? currentOrder() : [];
      const before =
        target.side === 'before' ? target.id : (order[order.indexOf(target.id) + 1] ?? null);
      void dropWithinUnit(payload.ids, before);
    }
    return;
  }
  if (target.kind === 'root') {
    void moveFolderTo(payload.id, null, null);
  } else if (target.kind === 'folder') {
    const move = moveTarget(
      useSidebarStore.getState().folders,
      payload.id,
      target.id,
      target.position,
    );
    if (move) {
      void moveFolderTo(payload.id, move.parentId, move.beforeId);
    }
  }
}

let currentOrder: () => number[] = () => [];
let currentPlaylistOrder: () => number[] = () => [];
let onPlaylistMoved: () => void = () => undefined;

/** プレイリストの画面のエントリの並びと、並べ替えたあとの読み直し（プレイリストの画面が渡す）。 */
export function setPlaylistOrderSource(source: () => number[], moved: () => void): void {
  currentPlaylistOrder = source;
  onPlaylistMoved = moved;
}

/** ユニットの中の並び（グリッドが渡す）。 */
export function setOrderSource(source: () => number[]): void {
  currentOrder = source;
}

/**
 * ポインタを押したところからドラッグを見張る。少し動いたらドラッグを始め、離したところに落とす。
 * ドラッグしたときは、そのあとのクリックを捨てる（選択が変わらないように）。
 */
export function watchDrag(
  event: { button: number; ctrlKey: boolean; clientX: number; clientY: number },
  start: () => { payload: DragPayload; label: string } | null,
): void {
  if (event.button !== 0 || event.ctrlKey) {
    return;
  }
  const originX = event.clientX;
  const originY = event.clientY;
  let started = false;

  const onMove = (e: PointerEvent) => {
    if (!started) {
      if (Math.hypot(e.clientX - originX, e.clientY - originY) < THRESHOLD) {
        return;
      }
      const begun = start();
      if (!begun) {
        cleanup();
        return;
      }
      started = true;
      document.documentElement.dataset.dragging = 'true';
      useDragStore.setState({ payload: begun.payload, label: begun.label });
    }
    const payload = useDragStore.getState().payload;
    if (payload) {
      useDragStore.setState({
        x: e.clientX,
        y: e.clientY,
        target: findTarget(payload, e.clientX, e.clientY),
      });
    }
  };
  const onUp = (e: PointerEvent) => {
    const { payload, target } = useDragStore.getState();
    cleanup();
    if (started) {
      // ドラッグの最後のクリックで選択が変わらないようにする
      window.addEventListener('click', swallow, { capture: true, once: true });
      setTimeout(() => window.removeEventListener('click', swallow, { capture: true }), 0);
      if (payload && target) {
        performDrop(payload, target, e.altKey);
      }
    }
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      cleanup();
    }
  };
  const cleanup = () => {
    window.removeEventListener('pointermove', onMove);
    window.removeEventListener('pointerup', onUp);
    window.removeEventListener('keydown', onKey, true);
    delete document.documentElement.dataset.dragging;
    useDragStore.setState({ payload: null, target: null });
  };
  window.addEventListener('pointermove', onMove);
  window.addEventListener('pointerup', onUp);
  window.addEventListener('keydown', onKey, true);
}

function swallow(e: MouseEvent): void {
  e.stopPropagation();
  e.preventDefault();
}
