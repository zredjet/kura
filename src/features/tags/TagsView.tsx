import { useVirtualizer } from '@tanstack/react-virtual';
import { type KeyboardEvent, type MouseEvent, useEffect, useMemo, useRef, useState } from 'react';

import { type MenuEntry, openMenuAt, openMenuBelow } from '../../components/Menu/menuStore';
import { SearchIcon } from '../../components/icons/SearchIcon';
import { ja } from '../../i18n/ja';
import { listTags, type TagListEntryDto } from '../../ipc/tags';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import { useRevisionStore } from '../../stores/revisionStore';
import { useViewStore } from '../../stores/viewStore';
import { openTagDialog } from './tagDialogStore';
import { tagRows } from './tagRows';
import styles from './TagsView.module.css';

const t = ja.tagsView;

/** 行の高さ（見出し・タグ）。 */
const HEADING_HEIGHT = 36;
const ROW_HEIGHT = 32;

/** このタグですべての一覧を絞り込む。 */
function filterBy(entry: TagListEntryDto): void {
  const view = useViewStore.getState();
  view.setView({ kind: 'all' });
  view.setFilter([entry.clause]);
}

function menuFor(entry: TagListEntryDto): MenuEntry[] {
  return [
    { label: t.filter, shortcut: '↩', onSelect: () => filterBy(entry) },
    'separator',
    { label: t.edit, onSelect: () => openTagDialog(entry.tag.id) },
  ];
}

/** 付いている数の言い方（「128 項目 · 自動 120・手動 8 · ユニット 2」）。 */
export function usageText(entry: TagListEntryDto): string {
  if (entry.items === 0 && entry.units === 0) {
    return t.unused;
  }
  const parts: string[] = [];
  if (entry.items > 0) {
    parts.push(ja.library.itemCount(formatCount(entry.items)));
    parts.push(t.sources(formatCount(entry.auto), formatCount(entry.items - entry.auto)));
  }
  if (entry.units > 0) {
    parts.push(t.units(formatCount(entry.units)));
  }
  return parts.join(' · ');
}

/**
 * タグの一覧（サイドバーの「タグ」）：名前空間ごとの見出しの下に、名前と付いている数（自動・手動）。
 * 押すとそのタグで絞り込み、右クリック（⌃↩）で編集のシート（名前・統合・削除）を開く。
 */
export function TagsView() {
  const revision = useRevisionStore((s) => s.revision);
  const [entries, setEntries] = useState<TagListEntryDto[] | null>(null);
  const [query, setQuery] = useState('');
  useEffect(() => {
    listTags().then(setEntries, (error: unknown) =>
      notify(error instanceof Error ? error.message : String(error)),
    );
  }, [revision]);

  const rows = useMemo(() => tagRows(entries ?? [], query), [entries, query]);
  const scrollRef = useRef<HTMLDivElement>(null);
  // React Compiler は使っていないので、メモ化できない API の警告は当てはまらない
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => (rows[i]?.kind === 'heading' ? HEADING_HEIGHT : ROW_HEIGHT),
    overscan: 12,
  });
  useEffect(() => {
    virtualizer.measure();
  }, [rows, virtualizer]);

  /** `index` 番目の行から `dir` の向きにある、いちばん近いタグの行へフォーカスを移す。 */
  const focusRow = (index: number, dir: 1 | -1) => {
    for (let i = index; i >= 0 && i < rows.length; i += dir) {
      if (rows[i]?.kind === 'tag') {
        virtualizer.scrollToIndex(i, { align: 'auto' });
        requestAnimationFrame(() =>
          scrollRef.current?.querySelector<HTMLElement>(`[data-tag-row="${i}"]`)?.focus(),
        );
        return;
      }
    }
  };

  const onListKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const index = Number((document.activeElement as HTMLElement | null)?.dataset.tagRow ?? -1);
    if (index < 0) {
      return;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      focusRow(index + (e.key === 'ArrowDown' ? 1 : -1), e.key === 'ArrowDown' ? 1 : -1);
    } else if (e.key === 'Enter' && e.ctrlKey) {
      const row = rows[index];
      if (row?.kind === 'tag') {
        e.preventDefault();
        openMenuBelow(e.target as HTMLElement, t.menu(row.entry.tag.name), menuFor(row.entry));
      }
    }
    // 一覧にいるあいだは、ライブラリの 1 文字のキー操作に渡さない
    e.stopPropagation();
  };

  const onContextMenu = (e: MouseEvent, entry: TagListEntryDto) => {
    e.preventDefault();
    openMenuAt(e.clientX, e.clientY, t.menu(entry.tag.name), menuFor(entry));
  };

  const total = entries?.length ?? 0;
  return (
    <div className={styles.view}>
      <header className={styles.head}>
        <div className={styles.heading}>
          <h1 className={styles.title}>{t.title}</h1>
          <span className={styles.summary}>
            {query.trim() && entries
              ? t.matched(
                  formatCount(rows.filter((r) => r.kind === 'tag').length),
                  formatCount(total),
                )
              : t.count(formatCount(total))}
          </span>
        </div>
        <label className={styles.filter}>
          <SearchIcon size={15} strokeWidth={1.9} />
          <input
            className={styles.filterInput}
            type="search"
            value={query}
            placeholder={t.filterPlaceholder}
            aria-label={t.filterLabel}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              e.stopPropagation();
              if (e.key === 'ArrowDown') {
                e.preventDefault();
                focusRow(0, 1);
              }
            }}
          />
        </label>
      </header>
      {entries !== null && rows.length === 0 ? (
        <div className={styles.empty}>
          <p className={styles.emptyTitle}>{query.trim() ? t.noMatch(query.trim()) : t.none}</p>
          {!query.trim() && <p className={styles.emptyHint}>{t.noneHint}</p>}
        </div>
      ) : (
        // 行のボタンのキー操作（↑↓・⌃↩）をまとめて受ける。操作の対象は中のボタン
        // eslint-disable-next-line jsx-a11y/no-static-element-interactions
        <div ref={scrollRef} className={styles.scroll} onKeyDown={onListKeyDown}>
          <div className={styles.canvas} style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((v) => {
              const row = rows[v.index];
              if (!row) {
                return null;
              }
              if (row.kind === 'heading') {
                return (
                  <h2
                    key={`h-${row.namespace}`}
                    className={styles.groupHeading}
                    style={{ top: v.start, height: HEADING_HEIGHT }}
                  >
                    {row.namespace || t.noNamespace}
                    <span className={styles.groupCount}>{formatCount(row.count)}</span>
                  </h2>
                );
              }
              const { entry } = row;
              return (
                <div
                  key={entry.tag.id}
                  className={styles.item}
                  style={{ top: v.start, height: ROW_HEIGHT }}
                >
                  <button
                    type="button"
                    className={styles.row}
                    data-tag-row={v.index}
                    data-color={entry.tag.color ?? undefined}
                    aria-label={`${entry.tag.name}、${usageText(entry)}`}
                    onClick={() => filterBy(entry)}
                    onContextMenu={(e) => onContextMenu(e, entry)}
                  >
                    <span className={styles.dot} aria-hidden="true" />
                    <span className={styles.name}>{entry.tag.name}</span>
                    <span className={styles.usage}>{usageText(entry)}</span>
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
