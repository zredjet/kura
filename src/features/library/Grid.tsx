import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';

import { openMenuAt } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { isUnitEntry, type ListView, unitIdOf } from '../../ipc/items';
import { dayLabel, localDay } from '../../lib/dates';
import { formatCount } from '../../lib/format';
import {
  getCached,
  getCachedUnit,
  requestItems,
  requestUnits,
  useItemCacheVersion,
} from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { ROW_HEIGHTS, useViewStore } from '../../stores/viewStore';
import { runScrollBench } from './bench';
import { setOrderSource, useDragStore, watchDrag } from './dragState';
import styles from './Grid.module.css';
import { itemMenu } from './itemMenu';
import { justify, rowOfItem, tileWidths, type TileRow } from './layout/justify';
import { extendTo, selectOne, toggle } from './selection';
import { type TileHandlers, Tile, UnitTile } from './Tile';

/** タイルの間隔・見出しの行の高さ（Main.dc.html）。 */
const GAP = 6;
const HEADING_HEIGHT = 30;

/** アイテムのグリッド（ジャスティファイ配置、行単位の仮想スクロール、日付の見出し）。 */
export function Grid() {
  const list = useLibraryStore((s) => s.list);
  const thumbSize = useViewStore((s) => s.thumbSize);
  const selection = useViewStore((s) => s.selection);
  const view = useViewStore((s) => s.view);
  useItemCacheVersion((s) => s.version);
  const selected = useMemo(() => new Set(selection.ids), [selection.ids]);
  const focusId = selection.focus;
  const dropTarget = useDragStore((s) => s.target);
  const dropHint = dropTarget?.kind === 'entry' ? dropTarget : null;

  const scrollRef = useRef<HTMLElement>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) {
      return;
    }
    const measure = () => {
      const style = getComputedStyle(el);
      const padding = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
      setWidth(Math.max(0, Math.floor(el.clientWidth - padding)));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const layout = useMemo(
    () =>
      list
        ? justify({
            aspects: list.aspects,
            groupStarts: list.groups.map((g) => g.start),
            width,
            targetHeight: ROW_HEIGHTS[thumbSize] ?? 180,
            gap: GAP,
            headingHeight: HEADING_HEIGHT,
          })
        : null,
    [list, width, thumbSize],
  );
  const rows = useMemo(() => layout?.rows ?? [], [layout]);

  // React Compiler は使っていないので、メモ化できない API の警告は当てはまらない
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => (rows[i]?.height ?? 0) + GAP,
    overscan: 4,
  });
  useEffect(() => {
    virtualizer.measure();
  }, [rows, virtualizer]);

  const virtualRows = virtualizer.getVirtualItems();
  const ids = list?.ids;

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
      const row = rows[v.index];
      if (row?.kind === 'tiles') {
        for (let i = row.start; i < row.end; i += 1) {
          const id = ids[i] ?? 0;
          if (isUnitEntry(id)) {
            units.push(unitIdOf(id));
          } else {
            items.push(id);
          }
        }
      }
    }
    requestItems(items);
    requestUnits(units);
  }, [visibleKey, ids, rows, virtualizer]);

  // ユニットの中を開いたら、そのユニットの概要（種類・名前）も取る
  useEffect(() => {
    if (view.kind === 'unit') {
      requestUnits([view.id]);
    }
  }, [view]);

  // 注目しているものが見えるようにスクロールする（←→ で選んだときなど）
  useEffect(() => {
    if (focusId === null || !ids || !layout) {
      return;
    }
    const row = rowOfItem(layout, ids.indexOf(focusId));
    if (row >= 0) {
      virtualizer.scrollToIndex(row, { align: 'auto' });
    }
  }, [focusId, ids, layout, virtualizer]);

  // 開発時の計測（VITE_KURA_BENCH=1）
  useEffect(() => {
    if (import.meta.env.VITE_KURA_BENCH === '1' && rows.length > 0 && scrollRef.current) {
      return runScrollBench(scrollRef.current, rows.length);
    }
  }, [rows.length]);

  // ユニットの中の並べ替えで使う、いまの並び
  useEffect(() => {
    setOrderSource(() => Array.from(useLibraryStore.getState().list?.ids ?? []));
  }, []);

  const handlers = useMemo<TileHandlers>(() => {
    const store = useViewStore.getState;
    const order = () => useLibraryStore.getState().list?.ids ?? new Uint32Array();
    return {
      onClick: (id, e) => {
        // WebKit ではボタンを押してもフォーカスが移らないので、キー操作がこのタイルに届くよう移す
        (e.currentTarget as HTMLElement).focus({ preventScroll: true });
        const { selection: current, setSelection } = store();
        if (e.metaKey) {
          setSelection(toggle(current, id));
        } else if (e.shiftKey) {
          setSelection(extendTo(current, id, order()));
        } else {
          setSelection(selectOne(id));
        }
      },
      onOpen: (id) => {
        const s = store();
        if (isUnitEntry(id)) {
          s.openUnit(unitIdOf(id));
        } else {
          s.setSelection(selectOne(id));
          s.setQuickLook(true);
        }
      },
      onContextMenu: (id, e) => {
        e.preventDefault();
        const s = store();
        const entries = s.selection.ids.includes(id) ? s.selection.ids : [id];
        if (!s.selection.ids.includes(id)) {
          s.setSelection(selectOne(id));
        }
        openMenuAt(e.clientX, e.clientY, ja.itemMenu.label, itemMenu(entries));
      },
      onPointerDown: (id, e) => {
        if (e.metaKey || e.shiftKey) {
          return;
        }
        watchDrag(e, () => {
          const s = store();
          const entries = s.selection.ids.includes(id) ? [...s.selection.ids] : [id];
          if (!s.selection.ids.includes(id)) {
            s.setSelection(selectOne(id));
          }
          return {
            payload: { kind: 'entries', ids: entries },
            label: ja.library.itemCount(formatCount(entries.length)),
          };
        });
      },
    };
  }, []);

  const today = localDay(Date.now());
  const empty = list !== null && list.ids.length === 0;

  return (
    <section ref={scrollRef} className={styles.grid} aria-label={ja.library.gridLabel}>
      {empty ? (
        <EmptyView view={view} />
      ) : (
        <div className={styles.canvas} style={{ height: virtualizer.getTotalSize() }}>
          {virtualRows.map((v) => {
            const row = rows[v.index];
            if (!row || !list) {
              return null;
            }
            if (row.kind === 'heading') {
              const group = list.groups[row.group];
              return (
                <div
                  key={v.key}
                  className={styles.heading}
                  style={{ transform: `translateY(${v.start}px)`, height: row.height }}
                >
                  <h2 className={styles.headingTitle}>{group ? dayLabel(group.day, today) : ''}</h2>
                  <span className={styles.headingCount}>
                    {ja.library.itemCount(formatCount(row.count))}
                  </span>
                </div>
              );
            }
            return (
              <TileRowView
                key={v.key}
                row={row}
                start={v.start}
                width={width}
                list={list}
                selected={selected}
                dropHint={dropHint}
                handlers={handlers}
              />
            );
          })}
        </div>
      )}
    </section>
  );
}

function TileRowView({
  row,
  start,
  width,
  list,
  selected,
  dropHint,
  handlers,
}: {
  row: TileRow;
  start: number;
  width: number;
  list: { ids: Uint32Array; aspects: Uint16Array };
  selected: ReadonlySet<number>;
  dropHint: { id: number; side: 'before' | 'after' } | null;
  handlers: TileHandlers;
}) {
  const widths = tileWidths(row, list.aspects, width, GAP);
  return (
    <div className={styles.row} style={{ transform: `translateY(${start}px)`, height: row.height }}>
      {widths.map((w, k) => {
        const id = list.ids[row.start + k] ?? 0;
        if (isUnitEntry(id)) {
          return (
            <UnitTile
              key={id}
              id={id}
              unit={getCachedUnit(unitIdOf(id))}
              width={w}
              height={row.height}
              selected={selected.has(id)}
              handlers={handlers}
            />
          );
        }
        return (
          <Tile
            key={id}
            id={id}
            item={getCached(id)}
            width={w}
            height={row.height}
            selected={selected.has(id)}
            dropSide={dropHint?.id === id ? dropHint.side : undefined}
            handlers={handlers}
          />
        );
      })}
    </div>
  );
}

/** 一覧が空のときの案内（一覧の種類ごと）。 */
function EmptyView({ view }: { view: ListView }) {
  const t = ja.library.emptyView;
  let title: string = t.other;
  let hint: string | null = null;
  if (view.kind === 'all') {
    title = ja.library.emptyTitle;
    hint = ja.library.emptyHint;
  } else if (view.kind === 'folder') {
    title = t.folder;
    hint = t.folderHint;
  } else if (view.kind === 'trash') {
    title = t.trash;
  } else if (view.kind === 'unit') {
    title = t.unit;
  }
  return (
    <div className={styles.empty}>
      <p className={styles.emptyTitle}>{title}</p>
      {hint && <p className={styles.emptyHint}>{hint}</p>}
    </div>
  );
}
