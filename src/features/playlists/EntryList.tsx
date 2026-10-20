import { useVirtualizer } from '@tanstack/react-virtual';
import {
  type KeyboardEvent,
  type MouseEvent,
  type PointerEvent,
  type RefObject,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';

import { type MenuEntry, openMenuAt } from '../../components/Menu/menuStore';
import { ChevronRightIcon } from '../../components/icons/ChevronRightIcon';
import { GripIcon } from '../../components/icons/GripIcon';
import { KindIcon } from '../../components/icons/KindIcon';
import { MediaImage } from '../../components/MediaImage/MediaImage';
import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { hasDuration, kindLabel } from '../../ipc/kinds';
import type { EntryRowDto, PlaylistContentsDto, PlaylistDto } from '../../ipc/playlists';
import { formatCount, formatDuration } from '../../lib/format';
import { getCached, requestItems, useItemCacheVersion } from '../../stores/itemCache';
import { bumpRevision } from '../../stores/revisionStore';
import { setPlaylistOrderSource, useDragStore, watchDrag } from '../library/dragState';
import { playPlaylist, removeFromPlaylist, snapshotPlaylist } from './playlistActions';
import { type ListRow, listRows, rowHeight } from './rows';
import styles from './PlaylistView.module.css';

const t = ja.playlist;

/** 行のサムネイルの大きさ（PlaylistView.module.css の `.thumb`。市松模様は敷かない大きさ）。 */
const ROW_THUMB = { w: 40, h: 40 };

/** サムネイル（なければ種類の絵）。`box` は表示の大きさ（表紙のモザイクは大きい）。 */
export function Thumb({
  item,
  className,
  box = ROW_THUMB,
}: {
  item: ItemSummaryDto | undefined;
  className?: string;
  box?: { w: number; h: number };
}) {
  const icon = <KindIcon kind={item?.kind} size={18} />;
  return (
    <span className={className ?? styles.thumb} aria-hidden="true">
      {item ? (
        <MediaImage source={item} size="s" fit="cover" box={box} loading="lazy" fallback={icon} />
      ) : (
        icon
      )}
    </span>
  );
}

/** 「動画 · 0:45」「画像 · 8秒」「フォント」（フォント・3D は再生しないので長さを出さない）。 */
export function itemSub(item: ItemSummaryDto | undefined, imageMs: number): string {
  if (!item) {
    return '';
  }
  const kind = kindLabel(item.kind);
  const length =
    item.kind === 'image'
      ? ja.player.seconds(String(Math.round(imageMs / 1000)))
      : hasDuration(item.kind) && item.durationMs !== null
        ? formatDuration(item.durationMs)
        : '';
  return length ? `${kind} · ${length}` : kind;
}

/** 一覧をページのスクロールに乗せて仮想スクロールする（一覧より上の高さを scrollMargin にする）。 */
export function useListMargin(
  listRef: RefObject<HTMLElement | null>,
  scrollRef: RefObject<HTMLElement | null>,
): number {
  const [margin, setMargin] = useState(0);
  useLayoutEffect(() => {
    const list = listRef.current;
    const scroller = scrollRef.current;
    if (!list || !scroller) {
      return;
    }
    const measure = () => {
      setMargin(
        list.getBoundingClientRect().top -
          scroller.getBoundingClientRect().top +
          scroller.scrollTop,
      );
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(scroller.firstElementChild ?? scroller);
    return () => observer.disconnect();
  }, [listRef, scrollRef]);
  return margin;
}

/**
 * 手動プレイリストの中身（design.md §3.1）。ファイル単位は平らな行と「〇〇から」の見出し、
 * ユニット単位はエントリの行（▸ で中身）。選んでドラッグで並べ替え、⌫ で外す、ダブルクリックでそこから再生。
 */
export function EntryList({
  playlist,
  contents,
  scrollRef,
  onChanged,
}: {
  playlist: PlaylistDto;
  contents: PlaylistContentsDto;
  scrollRef: RefObject<HTMLDivElement | null>;
  onChanged: () => void;
}) {
  const granularity = playlist.granularity;
  const [opened, setOpened] = useState<ReadonlySet<number>>(new Set());
  const [selected, setSelected] = useState<ReadonlySet<number>>(new Set());
  const [anchor, setAnchor] = useState<number | null>(null);
  const dropTarget = useDragStore((s) => s.target);
  useItemCacheVersion((s) => s.version);
  const listRef = useRef<HTMLDivElement>(null);
  const margin = useListMargin(listRef, scrollRef);
  const rows = useMemo(
    () => listRows(contents, granularity, opened),
    [contents, granularity, opened],
  );
  const entryOrder = useMemo(() => contents.rows.map((r) => r.entryId), [contents]);

  useEffect(() => {
    setPlaylistOrderSource(
      () => entryOrder,
      () => {
        bumpRevision();
        onChanged();
      },
    );
  }, [entryOrder, onChanged]);

  // React Compiler は使っていないので、メモ化できない API の警告は当てはまらない
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => rowHeight(rows[i], granularity) + 2,
    overscan: 8,
    scrollMargin: margin,
  });
  useEffect(() => {
    virtualizer.measure();
  }, [rows, virtualizer]);
  const visible = virtualizer.getVirtualItems();

  useEffect(() => {
    const wanted: number[] = [];
    for (const v of visible) {
      const row = rows[v.index];
      if (row?.kind === 'item') {
        wanted.push(row.itemId);
      } else if (row) {
        const cover = contents.rows[row.entry]?.coverItemId;
        if (cover !== null && cover !== undefined) {
          wanted.push(cover);
        }
      }
    }
    requestItems(wanted);
  });

  const entryOf = (row: ListRow): EntryRowDto | undefined => contents.rows[row.entry];

  const select = (entryId: number, e: MouseEvent | PointerEvent) => {
    if (e.shiftKey && anchor !== null) {
      const a = entryOrder.indexOf(anchor);
      const b = entryOrder.indexOf(entryId);
      const [from, to] = a < b ? [a, b] : [b, a];
      setSelected(new Set(entryOrder.slice(from, to + 1)));
      return;
    }
    if (e.metaKey) {
      const next = new Set(selected);
      if (next.has(entryId)) {
        next.delete(entryId);
      } else {
        next.add(entryId);
      }
      setSelected(next);
      setAnchor(entryId);
      return;
    }
    setSelected(new Set([entryId]));
    setAnchor(entryId);
  };

  const playFrom = (row: ListRow) => {
    const entry = entryOf(row);
    const itemId = row.kind === 'item' ? row.itemId : (entry?.coverItemId ?? undefined);
    void playPlaylist(playlist.id, playlist.name, false, itemId);
  };

  const menuFor = (row: ListRow): MenuEntry[] => {
    const entry = entryOf(row);
    const ids = entry && selected.has(entry.entryId) ? [...selected] : entry ? [entry.entryId] : [];
    const expandable = ids.some((id) => {
      const target = contents.rows.find((r) => r.entryId === id)?.target;
      return target && target.kind !== 'item';
    });
    const out: MenuEntry[] = [
      { label: t.entryMenu.playFrom, shortcut: '↩', onSelect: () => playFrom(row) },
      'separator',
    ];
    if (expandable) {
      out.push({
        label: t.entryMenu.snapshot,
        onSelect: () => void snapshotPlaylist(playlist.id, ids).then(onChanged),
      });
    }
    out.push({
      label: t.entryMenu.remove,
      shortcut: '⌫',
      danger: true,
      onSelect: () => void removeFromPlaylist(playlist.id, ids).then(onChanged),
    });
    return out;
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (selected.size === 0) {
      return;
    }
    if (e.key === 'Backspace' || e.key === 'Delete') {
      e.preventDefault();
      void removeFromPlaylist(playlist.id, [...selected]).then(onChanged);
      setSelected(new Set());
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const first = rows.find((r) => selected.has(entryOf(r)?.entryId ?? -1));
      if (first) {
        playFrom(first);
      }
    }
  };

  const startDrag = (e: PointerEvent, entryId: number) => {
    const ids = selected.has(entryId) ? entryOrder.filter((id) => selected.has(id)) : [entryId];
    watchDrag(e, () => ({
      payload: { kind: 'playlistEntries', playlistId: playlist.id, ids },
      label: ja.library.itemCount(formatCount(ids.length)),
    }));
  };

  // 落とす位置の線：エントリの最初の行（前）か最後の行（後ろ）
  const dropSide = (index: number): 'before' | 'after' | undefined => {
    if (dropTarget?.kind !== 'playlistEntry') {
      return undefined;
    }
    const row = rows[index];
    const entryId = row ? entryOf(row)?.entryId : undefined;
    if (entryId !== dropTarget.id) {
      return undefined;
    }
    const prev = rows[index - 1];
    const next = rows[index + 1];
    const first = !prev || entryOf(prev)?.entryId !== entryId;
    const last = !next || entryOf(next)?.entryId !== entryId;
    if (dropTarget.side === 'before' && first) {
      return 'before';
    }
    if (dropTarget.side === 'after' && last) {
      return 'after';
    }
    return undefined;
  };

  if (rows.length === 0) {
    return (
      <div className={styles.empty}>
        <p className={styles.emptyTitle}>{t.empty}</p>
        <p className={styles.emptyHint}>{t.emptyHint}</p>
      </div>
    );
  }

  return (
    // 行のキー操作（⌫・↩）をまとめて受ける。操作の対象は中の行のボタン
    // eslint-disable-next-line jsx-a11y/no-static-element-interactions
    <div
      ref={listRef}
      className={styles.list}
      style={{ height: virtualizer.getTotalSize() }}
      aria-label={t.listLabel}
      onKeyDown={onKeyDown}
    >
      {visible.map((v) => {
        const row = rows[v.index];
        const entry = row ? entryOf(row) : undefined;
        if (!row || !entry) {
          return null;
        }
        const top = v.start - margin;
        const style = { transform: `translateY(${top}px)`, height: rowHeight(row, granularity) };
        if (row.kind === 'header') {
          const kind = entry.target.kind === 'unit' ? t.kindUnit : t.kindFolder;
          return (
            <div key={v.key} className={styles.fromHeader} style={style}>
              {t.fromHeader(entry.name, kind, formatCount(entry.itemCount))}
            </div>
          );
        }
        const isSelected = selected.has(entry.entryId);
        const common = {
          'aria-selected': isSelected,
          'data-drop-playlist-entry': entry.entryId,
          'data-drop-side': dropSide(v.index),
          onClick: (e: MouseEvent<HTMLButtonElement>) => {
            e.currentTarget.focus();
            select(entry.entryId, e);
          },
          onDoubleClick: () => playFrom(row),
          onPointerDown: (e: PointerEvent<HTMLButtonElement>) => startDrag(e, entry.entryId),
          onContextMenu: (e: MouseEvent) => {
            e.preventDefault();
            if (!selected.has(entry.entryId)) {
              setSelected(new Set([entry.entryId]));
              setAnchor(entry.entryId);
            }
            openMenuAt(e.clientX, e.clientY, t.entryMenu.label, menuFor(row));
          },
        };
        if (row.kind === 'item') {
          const item = getCached(row.itemId);
          const imageMs = entry.imageDurationMs ?? playlist.imageDurationMs;
          return (
            <button
              key={v.key}
              type="button"
              className={styles.listRow}
              style={style}
              data-indent={row.indent || undefined}
              {...common}
            >
              <Thumb item={item} />
              <span className={styles.rowText}>
                <span className={styles.rowTitle}>
                  <span>{item?.title ?? ''}</span>
                  {granularity === 'unit' && !row.indent && (
                    <span className={styles.single}>{t.single}</span>
                  )}
                </span>
                <span className={styles.rowSub}>{itemSub(item, imageMs)}</span>
              </span>
              {!row.indent && (
                <span className={styles.grip} aria-hidden="true">
                  <GripIcon size={14} />
                </span>
              )}
            </button>
          );
        }
        // ユニット単位のエントリの行
        if (entry.target.kind === 'item') {
          const item = getCached(entry.target.id);
          return (
            <button key={v.key} type="button" className={styles.listRow} style={style} {...common}>
              <Thumb item={item} />
              <span className={styles.rowText}>
                <span className={styles.rowTitle}>
                  <span>{entry.name}</span>
                  <span className={styles.single}>{t.single}</span>
                </span>
                <span className={styles.rowSub}>
                  {itemSub(item, entry.imageDurationMs ?? playlist.imageDurationMs)}
                </span>
              </span>
              <span className={styles.grip} aria-hidden="true">
                <GripIcon size={14} />
              </span>
            </button>
          );
        }
        const open = opened.has(entry.entryId);
        const kinds = entry.kinds.map(kindLabel).join('・');
        const cover = entry.coverItemId !== null ? getCached(entry.coverItemId) : undefined;
        return (
          <div
            key={v.key}
            className={styles.listRow}
            style={style}
            data-selected={isSelected || undefined}
            data-drop-playlist-entry={entry.entryId}
            data-drop-side={dropSide(v.index)}
          >
            <button
              type="button"
              className={styles.expand}
              aria-expanded={open}
              aria-label={open ? t.collapse(entry.name) : t.expand(entry.name)}
              onClick={() =>
                setOpened((prev) => {
                  const next = new Set(prev);
                  if (next.has(entry.entryId)) {
                    next.delete(entry.entryId);
                  } else {
                    next.add(entry.entryId);
                  }
                  return next;
                })
              }
            >
              <ChevronRightIcon size={12} strokeWidth={2.2} />
            </button>
            <button type="button" className={styles.unitButton} {...common}>
              <span className={styles.stackCover} aria-hidden="true">
                <Thumb item={cover} className="" />
              </span>
              <span className={styles.rowText}>
                <span className={`${styles.rowTitle} ${styles.unitTitle}`}>
                  <span>{entry.name}</span>
                </span>
                <span className={styles.rowSub}>
                  {t.unitSub(formatCount(entry.itemCount), kinds, formatDuration(entry.durationMs))}
                </span>
              </span>
              <span className={styles.grip} aria-hidden="true">
                <GripIcon size={14} />
              </span>
            </button>
          </div>
        );
      })}
    </div>
  );
}
