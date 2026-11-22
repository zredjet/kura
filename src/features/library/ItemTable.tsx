import { useVirtualizer } from '@tanstack/react-virtual';
import { memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';

import { KindIcon } from '../../components/icons/KindIcon';
import { MediaImage } from '../../components/MediaImage/MediaImage';
import { StarIcon } from '../../components/icons/StarIcon';
import { UnitIcon } from '../../components/icons/UnitIcon';
import { ja } from '../../i18n/ja';
import { isUnitEntry, type ItemSummaryDto, type SortKey, unitIdOf } from '../../ipc/items';
import { hasDuration, kindLabel } from '../../ipc/kinds';
import type { UnitSummaryDto } from '../../ipc/units';
import { relativeDateTime } from '../../lib/dates';
import { formatBytes, formatCount, formatDuration } from '../../lib/format';
import {
  getCached,
  getCachedUnit,
  requestItems,
  requestUnits,
  useItemCacheVersion,
} from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { setOrderSource, useDragStore } from './dragState';
import { spokenLabel } from './describeItem';
import { EmptyView } from './EmptyView';
import styles from './ItemTable.module.css';
import { focusEntry, setVerticalNavigator } from './navigation';
import type { TileHandlers } from './Tile';
import { useEntryHandlers } from './useEntryHandlers';

/** 行の高さ（`--h-sidebar-row` と同じ）。 */
const ROW_HEIGHT = 32;
/** 行のサムネイルの大きさ（`.thumbCell` の `--space-6`。市松模様は敷かない大きさ）。 */
const THUMB_BOX = { w: 24, h: 24 };

/** 列（種類は並び順にない）。 */
const COLUMNS: { key: SortKey | 'kind'; className: string }[] = [
  { key: 'name', className: styles.name ?? '' },
  { key: 'kind', className: styles.kind ?? '' },
  { key: 'duration', className: styles.duration ?? '' },
  { key: 'rating', className: styles.rating ?? '' },
  { key: 'imported', className: styles.date ?? '' },
  { key: 'size', className: styles.size ?? '' },
];

/** リスト表示（⌘3）：1 行 32px の表。見出しを押すと並び順を変える。行単位の仮想スクロール。 */
export function ItemTable() {
  const list = useLibraryStore((s) => s.list);
  const selection = useViewStore((s) => s.selection);
  const view = useViewStore((s) => s.view);
  const sort = useViewStore((s) => s.sort);
  useItemCacheVersion((s) => s.version);
  const selected = useMemo(() => new Set(selection.ids), [selection.ids]);
  const focusId = selection.focus;
  const dropTarget = useDragStore((s) => s.target);
  const dropHint = dropTarget?.kind === 'entry' ? dropTarget : null;
  const handlers = useEntryHandlers();
  const sortable = view.kind !== 'random' && view.kind !== 'trash' && view.kind !== 'unit';

  const scrollRef = useRef<HTMLDivElement>(null);
  // 見出しの右を、行の側のスクロールバーの幅だけあける（列の位置をそろえる）
  const [gutter, setGutter] = useState(0);
  const empty = list !== null && list.ids.length === 0;
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) {
      return;
    }
    const measure = () => setGutter(el.offsetWidth - el.clientWidth);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [empty]);
  const ids = list?.ids;
  const count = ids?.length ?? 0;
  // React Compiler は使っていないので、メモ化できない API の警告は当てはまらない
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });
  const virtualRows = virtualizer.getVirtualItems();

  // 見えている行の詳細を取る
  const visibleKey =
    virtualRows.length > 0
      ? `${virtualRows[0]?.index}-${virtualRows[virtualRows.length - 1]?.index}`
      : '';
  useEffect(() => {
    if (!ids) {
      return;
    }
    const items: number[] = [];
    const units: number[] = [];
    for (const v of virtualizer.getVirtualItems()) {
      const id = ids[v.index] ?? 0;
      if (isUnitEntry(id)) {
        units.push(unitIdOf(id));
      } else {
        items.push(id);
      }
    }
    requestItems(items);
    requestUnits(units);
  }, [visibleKey, ids, virtualizer]);

  // 注目しているものが見えるようにスクロールする
  useEffect(() => {
    if (focusId === null || !ids) {
      return;
    }
    const index = ids.indexOf(focusId);
    if (index >= 0) {
      virtualizer.scrollToIndex(index, { align: 'auto' });
    }
    // 描き直したあとで、フォーカスを新しい行・タイルへ
    const frame = requestAnimationFrame(() => focusEntry(focusId));
    return () => cancelAnimationFrame(frame);
  }, [focusId, ids, virtualizer]);

  // ↑↓ は前・後ろの行へ
  useEffect(() => {
    setVerticalNavigator((index, dir) => {
      const next = index + dir;
      return next >= 0 && next < count ? next : null;
    });
    return () => setVerticalNavigator(null);
  }, [count]);

  useEffect(() => {
    setOrderSource(() => Array.from(useLibraryStore.getState().list?.ids ?? []));
  }, []);

  const changeSort = (key: SortKey) => {
    // 名前は小さい順（A→Z）、それ以外は大きい・新しい順から。同じ列なら向きを変える
    useViewStore.getState().setSort({ key, desc: key === sort.key ? !sort.desc : key !== 'name' });
  };

  if (empty) {
    return (
      <section className={styles.table} aria-label={ja.library.gridLabel}>
        <EmptyView view={view} />
      </section>
    );
  }

  return (
    <section className={styles.table} aria-label={ja.library.gridLabel}>
      <div className={styles.header} style={{ marginRight: `calc(var(--space-2) + ${gutter}px)` }}>
        <span className={styles.thumbCell} />
        {COLUMNS.map(({ key, className }) => {
          const label = ja.table.columns[key];
          if (key === 'kind' || !sortable) {
            return (
              <span key={key} className={`${styles.cell} ${className}`}>
                {label}
              </span>
            );
          }
          const active = sort.key === key;
          return (
            <button
              key={key}
              type="button"
              className={`${styles.cell} ${styles.headerButton} ${className}`}
              aria-pressed={active}
              aria-label={ja.table.sortBy(label)}
              onClick={() => changeSort(key)}
            >
              {label}
              {active && (
                <span className={styles.sortMark} aria-hidden="true">
                  {sort.desc ? '↓' : '↑'}
                </span>
              )}
            </button>
          );
        })}
      </div>
      <div ref={scrollRef} className={styles.body}>
        <div className={styles.canvas} style={{ height: virtualizer.getTotalSize() }}>
          {ids &&
            virtualRows.map((v) => {
              const id = ids[v.index] ?? 0;
              return (
                <TableRow
                  key={id}
                  id={id}
                  start={v.start}
                  item={isUnitEntry(id) ? undefined : getCached(id)}
                  unit={isUnitEntry(id) ? getCachedUnit(unitIdOf(id)) : undefined}
                  selected={selected.has(id)}
                  dropSide={dropHint?.id === id ? dropHint.side : undefined}
                  handlers={handlers}
                />
              );
            })}
        </div>
      </div>
    </section>
  );
}

interface TableRowProps {
  id: number;
  start: number;
  item: ItemSummaryDto | undefined;
  unit: UnitSummaryDto | undefined;
  selected: boolean;
  dropSide: 'before' | 'after' | undefined;
  handlers: TileHandlers;
}

/** 表の 1 行（アイテムかユニット）。 */
const TableRow = memo(function TableRow({
  id,
  start,
  item,
  unit,
  selected,
  dropSide,
  handlers,
}: TableRowProps) {
  const t = ja.table;
  const kind = item?.kind;
  const name = unit ? unit.name : (item?.title ?? '');
  const kindText = unit ? t.unitKind(formatCount(unit.count)) : kindLabel(kind);
  const durationMs = unit ? unit.durationMs : (item?.durationMs ?? null);
  const duration =
    durationMs !== null && durationMs > 0 && (unit || hasDuration(kind))
      ? formatDuration(durationMs)
      : '';
  const rating = unit ? unit.rating : (item?.rating ?? 0);
  const date = unit ? unit.createdAt : (item?.importedAt ?? null);
  const thumb = unit ? unit.cover : item;
  const label = item
    ? spokenLabel(item)
    : [name, kindText, duration, rating > 0 ? ja.inspector.rating(Math.round(rating)) : '']
        .filter(Boolean)
        .join('、');
  const kindIcon = (
    <span className={styles.kindIcon}>
      {unit ? (
        <UnitIcon size={14} strokeWidth={1.9} />
      ) : (
        <KindIcon kind={kind} size={14} strokeWidth={1.9} />
      )}
    </span>
  );
  return (
    <button
      type="button"
      className={styles.row}
      // transform で置くと、アクセシビリティの枠が一覧の先頭から伸びてしまう（VoiceOver の枠がずれる）
      style={{ top: start }}
      aria-pressed={selected}
      aria-label={label}
      data-entry={id}
      data-drop-entry={unit ? undefined : id}
      data-drop-axis="y"
      data-drop={dropSide}
      onClick={(e) => handlers.onClick(id, e)}
      onDoubleClick={() => handlers.onOpen(id)}
      onContextMenu={(e) => handlers.onContextMenu(id, e)}
      onPointerDown={(e) => handlers.onPointerDown(id, e)}
    >
      <span className={styles.thumbCell}>
        {thumb ? (
          <MediaImage
            source={thumb}
            size="s"
            fit="cover"
            box={THUMB_BOX}
            className={styles.thumb}
            fallback={kindIcon}
          />
        ) : (
          kindIcon
        )}
      </span>
      <span className={`${styles.cell} ${styles.name}`}>
        {name}
        {unit && <UnitIcon size={12} strokeWidth={2} />}
      </span>
      <span className={`${styles.cell} ${styles.kind}`}>{kindText}</span>
      <span className={`${styles.cell} ${styles.duration}`}>{duration}</span>
      <span className={`${styles.cell} ${styles.rating}`}>
        {[1, 2, 3, 4, 5].map((n) => (
          <StarIcon key={n} size={11} strokeWidth={2} filled={n <= Math.round(rating)} />
        ))}
      </span>
      <span className={`${styles.cell} ${styles.date}`}>
        {date !== null ? relativeDateTime(date) : ''}
      </span>
      <span className={`${styles.cell} ${styles.size}`}>
        {item ? formatBytes(item.sizeBytes) : ''}
      </span>
    </button>
  );
});
