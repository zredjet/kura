import { useVirtualizer } from '@tanstack/react-virtual';
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';

import { ja } from '../../i18n/ja';
import { dayLabel, localDay } from '../../lib/dates';
import { formatCount } from '../../lib/format';
import { getCached, requestItems, useItemCacheVersion } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { ROW_HEIGHTS, useViewStore } from '../../stores/viewStore';
import { runScrollBench } from './bench';
import styles from './Grid.module.css';
import { justify, rowOfItem, tileWidths, type TileRow } from './layout/justify';
import { Tile } from './Tile';

/** タイルの間隔・見出しの行の高さ（Main.dc.html）。 */
const GAP = 6;
const HEADING_HEIGHT = 30;

/** アイテムのグリッド（ジャスティファイ配置、行単位の仮想スクロール、日付の見出し）。 */
export function Grid() {
  const list = useLibraryStore((s) => s.list);
  const thumbSize = useViewStore((s) => s.thumbSize);
  const selectedId = useViewStore((s) => s.selectedId);
  const select = useViewStore((s) => s.select);
  const setQuickLook = useViewStore((s) => s.setQuickLook);
  useItemCacheVersion((s) => s.version);

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
    const wanted: number[] = [];
    for (const v of virtualizer.getVirtualItems()) {
      const row = rows[v.index];
      if (row?.kind === 'tiles') {
        for (let i = row.start; i < row.end; i += 1) {
          wanted.push(ids[i] ?? 0);
        }
      }
    }
    requestItems(wanted);
  }, [visibleKey, ids, rows, virtualizer]);

  // 選んだアイテムが見えるようにスクロールする（←→ で選んだときなど）
  useEffect(() => {
    if (selectedId === null || !ids || !layout) {
      return;
    }
    const row = rowOfItem(layout, ids.indexOf(selectedId));
    if (row >= 0) {
      virtualizer.scrollToIndex(row, { align: 'auto' });
    }
  }, [selectedId, ids, layout, virtualizer]);

  // 開発時の計測（VITE_KURA_BENCH=1）
  useEffect(() => {
    if (import.meta.env.VITE_KURA_BENCH === '1' && rows.length > 0 && scrollRef.current) {
      return runScrollBench(scrollRef.current, rows.length);
    }
  }, [rows.length]);

  const open = useCallback(
    (id: number) => {
      select(id);
      setQuickLook(true);
    },
    [select, setQuickLook],
  );

  const today = localDay(Date.now());
  const empty = list !== null && list.ids.length === 0;

  return (
    <section ref={scrollRef} className={styles.grid} aria-label={ja.library.gridLabel}>
      {empty ? (
        <div className={styles.empty}>
          <p className={styles.emptyTitle}>{ja.library.emptyTitle}</p>
          <p className={styles.emptyHint}>{ja.library.emptyHint}</p>
        </div>
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
                selectedId={selectedId}
                onSelect={select}
                onOpen={open}
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
  selectedId,
  onSelect,
  onOpen,
}: {
  row: TileRow;
  start: number;
  width: number;
  list: { ids: Uint32Array; aspects: Uint16Array };
  selectedId: number | null;
  onSelect: (id: number) => void;
  onOpen: (id: number) => void;
}) {
  const widths = tileWidths(row, list.aspects, width, GAP);
  return (
    <div className={styles.row} style={{ transform: `translateY(${start}px)`, height: row.height }}>
      {widths.map((w, k) => {
        const id = list.ids[row.start + k] ?? 0;
        return (
          <Tile
            key={id}
            id={id}
            item={getCached(id)}
            width={w}
            height={row.height}
            selected={id === selectedId}
            onSelect={onSelect}
            onOpen={onOpen}
          />
        );
      })}
    </div>
  );
}
