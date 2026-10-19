import { useVirtualizer } from '@tanstack/react-virtual';
import { type RefObject, useEffect, useRef } from 'react';

import { ja } from '../../i18n/ja';
import { kindLabel } from '../../ipc/kinds';
import type { PlaylistContentsDto, PlaylistDto } from '../../ipc/playlists';
import { agoLabel } from '../../lib/dates';
import { formatCount, formatDuration } from '../../lib/format';
import {
  getCached,
  getCachedUnit,
  requestItems,
  requestUnits,
  useItemCacheVersion,
} from '../../stores/itemCache';
import { Thumb, useListMargin } from './EntryList';
import { playPlaylist } from './playlistActions';
import styles from './PlaylistView.module.css';

const t = ja.playlist;
/** 行の高さ（Playlist.dc.html：50px）。 */
const ROW = 50;

/**
 * スマートプレイリストの「今回の並び」（Playlist.dc.html の表）：#・名前・種類・長さ・評価・最終再生。
 * ユニット単位ならユニット（と単独のアイテム）の行。ダブルクリックでそこから再生。
 */
export function GeneratedTable({
  playlist,
  contents,
  scrollRef,
}: {
  playlist: PlaylistDto;
  contents: PlaylistContentsDto;
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  useItemCacheVersion((s) => s.version);
  const listRef = useRef<HTMLDivElement>(null);
  const margin = useListMargin(listRef, scrollRef);
  const rows = contents.rows;
  // React Compiler は使っていないので、メモ化できない API の警告は当てはまらない
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW,
    overscan: 8,
    scrollMargin: margin,
  });
  const visible = virtualizer.getVirtualItems();

  useEffect(() => {
    const items: number[] = [];
    const units: number[] = [];
    for (const v of visible) {
      const row = rows[v.index];
      if (row?.target.kind === 'unit') {
        units.push(row.target.id);
      }
      if (row?.coverItemId !== null && row?.coverItemId !== undefined) {
        items.push(row.coverItemId);
      }
    }
    requestItems(items);
    requestUnits(units);
  });

  return (
    <section className={styles.stack} aria-label={t.generated}>
      <div className={styles.listHead}>
        <h2 className={styles.listTitle}>{t.generated}</h2>
        <span className={styles.listNote}>{t.items(formatCount(rows.length))}</span>
      </div>
      {rows.length === 0 ? (
        <p className={styles.emptyHint}>{t.generatedNone}</p>
      ) : (
        <div>
          <div className={styles.tableHead} role="row">
            <span className={styles.num}>{t.columns.index}</span>
            <span>{t.columns.name}</span>
            <span>{t.columns.kind}</span>
            <span className={styles.right}>{t.columns.duration}</span>
            <span>{t.columns.rating}</span>
            <span>{t.columns.lastPlayed}</span>
          </div>
          <div ref={listRef} className={styles.list} style={{ height: virtualizer.getTotalSize() }}>
            {visible.map((v) => {
              const row = rows[v.index];
              if (!row) {
                return null;
              }
              const cover = row.coverItemId !== null ? getCached(row.coverItemId) : undefined;
              const unit = row.target.kind === 'unit' ? getCachedUnit(row.target.id) : undefined;
              const isItem = row.target.kind === 'item';
              const kinds = row.kinds.map(kindLabel);
              const length =
                isItem && cover?.kind === 'image'
                  ? t.seconds(String(Math.round(playlist.imageDurationMs / 1000)))
                  : formatDuration(row.durationMs);
              const rating = isItem ? (cover?.rating ?? 0) : (unit?.rating ?? 0);
              const lastPlayed = isItem ? cover?.lastPlayedAt : unit?.lastPlayedAt;
              return (
                <button
                  key={v.key}
                  type="button"
                  className={styles.tableRow}
                  style={{ transform: `translateY(${v.start - margin}px)` }}
                  onDoubleClick={() =>
                    void playPlaylist(
                      playlist.id,
                      playlist.name,
                      false,
                      row.coverItemId ?? undefined,
                    )
                  }
                >
                  <span className={`${styles.num} ${styles.cell}`}>{v.index + 1}</span>
                  <span className={styles.nameCell}>
                    {isItem ? (
                      <Thumb item={cover} />
                    ) : (
                      <span className={styles.stackCover} aria-hidden="true">
                        <Thumb item={cover} className="" />
                      </span>
                    )}
                    <span>{row.name}</span>
                  </span>
                  <span className={styles.cell}>{kinds.join('・')}</span>
                  <span className={`${styles.right} ${styles.cell}`}>{length}</span>
                  <span className={styles.cell}>
                    {rating > 0 ? (
                      <>
                        <span className={styles.star}>★</span> {rating}
                      </>
                    ) : (
                      '—'
                    )}
                  </span>
                  <span className={styles.cell}>{lastPlayed ? agoLabel(lastPlayed) : t.never}</span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </section>
  );
}
