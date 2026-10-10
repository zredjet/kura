import type { ReactNode } from 'react';

import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { CheckIcon } from '../../components/icons/CheckIcon';
import { CloseIcon } from '../../components/icons/CloseIcon';
import { ChevronDownIcon } from '../../components/icons/ChevronDownIcon';
import { ImageIcon } from '../../components/icons/ImageIcon';
import { NoteIcon } from '../../components/icons/NoteIcon';
import { StarIcon } from '../../components/icons/StarIcon';
import { ThumbLargeIcon } from '../../components/icons/ThumbLargeIcon';
import { ThumbSmallIcon } from '../../components/icons/ThumbSmallIcon';
import { VideoIcon } from '../../components/icons/VideoIcon';
import { ja } from '../../i18n/ja';
import { querySuggest } from '../../ipc/search';
import { formatCount } from '../../lib/format';
import { useSearchStore } from '../../stores/searchStore';
import { ROW_HEIGHTS, useViewStore } from '../../stores/viewStore';
import { clauseParts, fieldTexts, findField, replaceField, singleKind } from '../search/clauses';
import styles from './FilterBar.module.css';

const t = ja.filterBar;

/** フィルタバーが編集するフィールド（これらはチップを別に並べない）。 */
const EDITED_FIELDS = new Set(['type', 'rating', 'dur', 'source']);

const KIND_SEGMENTS: { kind: 'image' | 'video' | 'audio'; label: string; icon: ReactNode }[] = [
  { kind: 'image', label: ja.kinds.image, icon: <ImageIcon size={13} strokeWidth={2} /> },
  { kind: 'video', label: ja.kinds.video, icon: <VideoIcon size={13} strokeWidth={2} /> },
  { kind: 'audio', label: ja.kinds.audio, icon: <NoteIcon size={13} strokeWidth={2} /> },
];

function check(on: boolean): ReactNode {
  return on ? <CheckIcon size={12} strokeWidth={2.4} /> : null;
}

/** 評価の条件の表示（「★4 以上」「未評価」）。 */
function ratingLabel(clauses: readonly string[]): string | null {
  const parts = findField(clauses, 'rating');
  if (!parts) {
    return null;
  }
  const n = Number(parts.value);
  if (parts.op === '>=' && n >= 1 && n <= 5) {
    return t.ratingAtLeast(n);
  }
  if ((parts.op === ':' || parts.op === '=') && n === 0) {
    return t.unrated;
  }
  return `${t.rating} ${parts.op}${parts.value}`;
}

/** 長さの条件が、用意した区切りのどれか（なければ null）。 */
function durationPreset(clauses: readonly string[]): string | null {
  const current = fieldTexts(clauses, 'dur').join(' ');
  if (!current) {
    return null;
  }
  const preset = t.durations.find(([, texts]) => texts.join(' ') === current);
  return preset ? preset[0] : t.duration;
}

/**
 * 絞り込みの帯（Main.dc.html）：種類のセグメント、評価・タグ・長さ・取得元のチップ、そのほかの条件のチップ。
 * どれも検索の条件（viewStore の `filter`）を書き換えるだけで、⌘K パレットと同じ条件を表す。
 * 右端にサムネイルの大きさのスライダー。色のチップは M9 で検討（ADR-56）。
 */
export function FilterBar() {
  const thumbSize = useViewStore((s) => s.thumbSize);
  const setThumbSize = useViewStore((s) => s.setThumbSize);
  const filter = useViewStore((s) => s.filter);
  const setFilter = useViewStore((s) => s.setFilter);
  const openPalette = useSearchStore((s) => s.openPalette);

  const kind = singleKind(filter);
  const hasKind = findField(filter, 'type') !== null;
  const rating = ratingLabel(filter);
  const duration = durationPreset(filter);
  const source = findField(filter, 'source')?.value ?? null;
  const others = filter.filter((c) => {
    const parts = clauseParts(c);
    return parts.negated || !parts.field || !EDITED_FIELDS.has(parts.field);
  });

  const ratingMenu = (anchor: HTMLElement) => {
    const current = findField(filter, 'rating');
    const entries: MenuEntry[] = [5, 4, 3, 2, 1].map((n) => ({
      label: t.ratingAtLeast(n),
      icon: check(current?.op === '>=' && Number(current.value) === n),
      onSelect: () => setFilter(replaceField(filter, 'rating', `評価>=${n}`)),
    }));
    entries.push({
      label: t.unrated,
      icon: check(rating === t.unrated),
      onSelect: () => setFilter(replaceField(filter, 'rating', '評価:0')),
    });
    entries.push('separator', {
      label: t.clearField,
      disabled: !current,
      onSelect: () => setFilter(replaceField(filter, 'rating', null)),
    });
    openMenuBelow(anchor, t.rating, entries);
  };

  const durationMenu = (anchor: HTMLElement) => {
    const entries: MenuEntry[] = t.durations.map(([label, texts]) => ({
      label,
      icon: check(duration === label),
      onSelect: () => setFilter(replaceField(filter, 'dur', texts)),
    }));
    entries.push('separator', {
      label: t.clearField,
      disabled: duration === null,
      onSelect: () => setFilter(replaceField(filter, 'dur', null)),
    });
    openMenuBelow(anchor, t.duration, entries);
  };

  const sourceMenu = (anchor: HTMLElement) => {
    querySuggest('取得元:', 20)
      .then((found) => {
        const entries: MenuEntry[] = found.map((s) => ({
          label: s.count !== null ? `${s.label}（${formatCount(s.count)}）` : s.label,
          icon: check(source === s.label),
          onSelect: () => setFilter(replaceField(filter, 'source', s.insert)),
        }));
        if (entries.length === 0) {
          entries.push({ label: t.noSources, disabled: true, onSelect: () => {} });
        }
        entries.push('separator', {
          label: t.clearField,
          disabled: source === null,
          onSelect: () => setFilter(replaceField(filter, 'source', null)),
        });
        openMenuBelow(anchor, t.source, entries);
      })
      .catch((error: unknown) => console.warn('取得元を読めませんでした', error));
  };

  return (
    <div className={styles.filterBar}>
      <div role="group" aria-label={t.kinds} className={styles.segment}>
        <button
          type="button"
          className={styles.segmentButton}
          aria-pressed={!hasKind}
          onClick={() => setFilter(replaceField(filter, 'type', null))}
        >
          {t.all}
        </button>
        {KIND_SEGMENTS.map((s) => (
          <button
            key={s.kind}
            type="button"
            className={styles.segmentButton}
            aria-pressed={kind === s.kind}
            onClick={() =>
              setFilter(replaceField(filter, 'type', kind === s.kind ? null : `種類:${s.label}`))
            }
          >
            {s.icon}
            {s.label}
          </button>
        ))}
      </div>
      <button
        type="button"
        className={styles.chip}
        data-active={rating !== null || undefined}
        aria-haspopup="menu"
        onClick={(e) => ratingMenu(e.currentTarget)}
      >
        <StarIcon size={12} strokeWidth={2} />
        {rating ?? t.rating}
      </button>
      <button
        type="button"
        className={styles.chip}
        aria-haspopup="dialog"
        onClick={() => openPalette({ initial: filter })}
      >
        {t.tag}
        <ChevronDownIcon size={11} strokeWidth={2.4} />
      </button>
      <button
        type="button"
        className={styles.chip}
        data-active={duration !== null || undefined}
        aria-haspopup="menu"
        onClick={(e) => durationMenu(e.currentTarget)}
      >
        {duration ?? t.duration}
        <ChevronDownIcon size={11} strokeWidth={2.4} />
      </button>
      <button
        type="button"
        className={styles.chip}
        data-active={source !== null || undefined}
        aria-haspopup="menu"
        onClick={(e) => sourceMenu(e.currentTarget)}
      >
        {source ?? t.source}
        <ChevronDownIcon size={11} strokeWidth={2.4} />
      </button>
      {others.map((clause, i) => {
        const parts = clauseParts(clause);
        return (
          <span
            key={`${i}-${clause}`}
            className={styles.condition}
            data-excluded={parts.negated || undefined}
          >
            {parts.negated && <span className={styles.prefix}>{ja.search.excluded}</span>}
            {parts.negated ? clause.trim().slice(1) : clause}
            <button
              type="button"
              className={styles.remove}
              aria-label={ja.search.removeClause(clause)}
              onClick={() => setFilter(filter.filter((c) => c !== clause))}
            >
              <CloseIcon size={9} strokeWidth={3} />
            </button>
          </span>
        );
      })}
      {filter.length > 0 && (
        <button type="button" className={styles.clear} onClick={() => setFilter([])}>
          {ja.search.clear}
        </button>
      )}

      <label className={styles.size}>
        <ThumbSmallIcon size={12} strokeWidth={2.2} />
        <input
          type="range"
          className={styles.slider}
          min={1}
          max={ROW_HEIGHTS.length}
          value={thumbSize + 1}
          aria-label={ja.library.thumbSize}
          onChange={(e) => setThumbSize(Number(e.target.value) - 1)}
        />
        <ThumbLargeIcon size={16} strokeWidth={1.8} />
      </label>
    </div>
  );
}
