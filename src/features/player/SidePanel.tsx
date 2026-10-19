import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useMemo, useRef, useState } from 'react';

import glass from '../../components/glass/glass.module.css';
import { ChevronRightIcon } from '../../components/icons/ChevronRightIcon';
import { KindIcon } from '../../components/icons/KindIcon';
import { NowPlayingIcon } from '../../components/icons/NowPlayingIcon';
import { ReshuffleIcon } from '../../components/icons/ReshuffleIcon';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { hasDuration, kindLabel } from '../../ipc/kinds';
import { thumbUrl } from '../../ipc/media';
import { formatCount, formatDuration, formatLongDuration } from '../../lib/format';
import { getCached, requestItems, useItemCacheVersion } from '../../stores/itemCache';
import { TAG_FOCUS_KEY } from '../library/inspector/ItemInspector';
import { ItemInspector } from '../library/inspector/ItemInspector';
import type { Queue } from './engine/types';
import {
  playerController,
  rebuildQueue,
  updatePlayerPrefs,
  updatePlayingPlaylist,
} from './playerActions';
import { type PlayerPanel, usePlayerStore } from './playerStore';
import styles from './SidePanel.module.css';

const t = ja.player;
/** キューの行の高さ（54px）と間隔（2px）、ユニットの見出しの高さ。 */
const ROW = 56;
const GROUP_ROW = 34;

/** シアターの右パネル：キュー（Player.dc.html「次に再生」）か、今のアイテムの情報（インスペクタ）。 */
export function SidePanel({ panel }: { panel: Exclude<PlayerPanel, null> }) {
  return (
    <aside
      className={`${glass.panel} ${styles.panel}`}
      aria-label={panel === 'queue' ? t.queueTitle : t.info}
    >
      {panel === 'queue' ? <QueuePanel /> : <InfoPanel />}
    </aside>
  );
}

function InfoPanel() {
  const itemId = usePlayerStore((s) => s.snapshot.itemId);
  const focusTags = usePlayerStore((s) => s.focusTags);
  useEffect(() => {
    if (focusTags === 0) {
      return;
    }
    // インスペクタの内容が読めてからタグの入力に移る
    const timer = setTimeout(() => {
      document.querySelector<HTMLInputElement>(`[data-focus-key="${TAG_FOCUS_KEY}"]`)?.focus();
    }, 50);
    return () => clearTimeout(timer);
  }, [focusTags, itemId]);
  if (itemId === null) {
    return null;
  }
  return (
    <div className={styles.info}>
      <ItemInspector key={itemId} id={itemId} />
    </div>
  );
}

function QueuePanel() {
  const queue = usePlayerStore((s) => s.queue);
  const tab = usePlayerStore((s) => s.tab);
  const setTab = usePlayerStore((s) => s.setTab);
  const snapshot = usePlayerStore((s) => s.snapshot);
  const prefs = usePlayerStore((s) => s.prefs);
  const shuffle = usePlayerStore((s) => s.queue?.playlist?.shuffle ?? s.shuffle);
  const playlist = queue?.playlist ?? null;
  const count = queue?.itemIds.length ?? 0;

  // このセッションで表示時間を変えていれば、画像の分を置き換えて数える
  let total = queue?.totalMs ?? 0;
  if (queue && snapshot.imageOverridden) {
    for (const ms of queue.imageDurationMs) {
      if (ms !== null) {
        total += snapshot.imageMs - ms;
      }
    }
  }

  return (
    <>
      <div className={styles.head}>
        <div className={styles.headText}>
          <h2 className={styles.heading}>{t.queueTitle}</h2>
          <span className={styles.summary}>
            {t.queueSummary(formatCount(count), formatLongDuration(total))}
          </span>
        </div>
        <button
          type="button"
          className={styles.reshuffle}
          disabled={!shuffle}
          onClick={() => void rebuildQueue({ reseed: true })}
        >
          <ReshuffleIcon size={13} strokeWidth={2.2} />
          {t.reshuffle}
        </button>
      </div>
      <div className={styles.tabs} role="tablist" aria-label={t.queueTabs}>
        <button
          type="button"
          role="tab"
          className={styles.tab}
          aria-selected={tab === 'queue'}
          onClick={() => setTab('queue')}
        >
          {t.tabQueue}
        </button>
        <button
          type="button"
          role="tab"
          className={styles.tab}
          aria-selected={tab === 'history'}
          onClick={() => setTab('history')}
        >
          {t.tabHistory}
        </button>
      </div>
      {tab === 'queue' ? <QueueList /> : <HistoryList />}
      <div className={styles.footer}>
        <label className={styles.switchRow}>
          {t.bgmThroughImages}
          <Switch
            checked={playlist ? playlist.bgmThroughImages : prefs.bgmThroughImages}
            onChange={(on) =>
              playlist
                ? updatePlayingPlaylist({ bgmThroughImages: on })
                : updatePlayerPrefs({ bgmThroughImages: on })
            }
          />
        </label>
        {playlist && (
          <label className={styles.switchRow}>
            {t.noSameKindRun}
            <Switch
              checked={playlist.maxSameKindRun > 0}
              onChange={(on) => updatePlayingPlaylist({ maxSameKindRun: on ? 2 : 0 })}
            />
          </label>
        )}
        <label className={styles.switchRow}>
          {t.bgmLoop}
          <Switch checked={prefs.bgmLoop} onChange={(on) => updatePlayerPrefs({ bgmLoop: on })} />
        </label>
      </div>
    </>
  );
}

/** キュー（仮想スクロール）。再生済みの行は薄く、今の行に印。ユニット単位ならユニットごとに折りたたむ。 */
function QueueList() {
  const queue = usePlayerStore((s) => s.queue);
  const index = usePlayerStore((s) => s.snapshot.index);
  const bgmItem = usePlayerStore((s) => s.snapshot.bgm?.itemId ?? null);
  const imageMs = usePlayerStore((s) => s.snapshot.imageMs);
  const overridden = usePlayerStore((s) => s.snapshot.imageOverridden);
  const [opened, setOpened] = useState<ReadonlySet<number>>(new Set());
  useItemCacheVersion((s) => s.version);
  const scrollRef = useRef<HTMLDivElement>(null);
  const ids = queue?.itemIds ?? [];
  const currentGroup = queue?.groups.length ? (queue.groupIndex[index] ?? null) : null;
  const rows = useMemo(
    () => (queue ? queueRows(queue, currentGroup, opened) : []),
    [queue, currentGroup, opened],
  );
  // React Compiler は使っていないので、メモ化できない API の警告は当てはまらない
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => (rows[i]?.kind === 'group' ? GROUP_ROW : ROW),
    overscan: 6,
  });
  // 行数が同じでも見出しと曲の並びが変わる（並べ直す・畳む）ので、高さを測り直す
  useEffect(() => {
    virtualizer.measure();
  }, [rows, virtualizer]);
  const visible = virtualizer.getVirtualItems();

  useEffect(() => {
    const wanted = visible
      .map((r) => rows[r.index])
      .map((row) => (row?.kind === 'slot' ? ids[row.index] : undefined))
      .filter((id) => id !== undefined);
    requestItems(wanted);
  });

  // 今のスロットが見えるように（キューを開いたとき・次へ進んだとき）
  const currentRow = rows.findIndex((r) => r.kind === 'slot' && r.index === index);
  useEffect(() => {
    if (currentRow >= 0) {
      virtualizer.scrollToIndex(currentRow, { align: 'auto' });
    }
  }, [currentRow, virtualizer]);

  // 背景で流している曲の行（今より前で、いちばん近いもの）
  let bgmIndex = -1;
  if (bgmItem !== null) {
    for (let i = index - 1; i >= 0; i--) {
      if (ids[i] === bgmItem) {
        bgmIndex = i;
        break;
      }
    }
  }

  return (
    <div ref={scrollRef} className={styles.list}>
      <div className={styles.sizer} style={{ height: virtualizer.getTotalSize() }}>
        {visible.map((virtual) => {
          const row = rows[virtual.index];
          if (!row) {
            return null;
          }
          if (row.kind === 'group') {
            const group = queue?.groups[row.group];
            if (!group) {
              return null;
            }
            const current = row.group === currentGroup;
            return (
              <button
                key={virtual.key}
                type="button"
                className={styles.groupRow}
                style={{ transform: `translateY(${virtual.start}px)` }}
                aria-expanded={row.open}
                data-current={current || undefined}
                data-played={(currentGroup !== null && row.group < currentGroup) || undefined}
                onClick={() =>
                  current
                    ? undefined
                    : setOpened((prev) => {
                        const next = new Set(prev);
                        if (next.has(row.group)) {
                          next.delete(row.group);
                        } else {
                          next.add(row.group);
                        }
                        return next;
                      })
                }
                onDoubleClick={() => playerController().jump(group.start)}
              >
                <span
                  className={styles.chevron}
                  data-open={row.open || undefined}
                  aria-hidden="true"
                >
                  <ChevronRightIcon size={12} strokeWidth={2.2} />
                </span>
                <span className={styles.groupName}>{group.name}</span>
                <span className={styles.groupCount}>{formatCount(group.len)}</span>
              </button>
            );
          }
          const id = ids[row.index];
          if (id === undefined) {
            return null;
          }
          const item = getCached(id);
          const current = row.index === index;
          const background = row.index === bgmIndex;
          const slotImageMs = overridden ? imageMs : queue?.imageDurationMs[row.index];
          return (
            <Row
              key={virtual.key}
              top={virtual.start}
              indent={row.indent}
              item={item}
              current={current}
              background={background}
              played={row.index < index && !background}
              sub={rowSub(item, current, background, slotImageMs ?? null)}
              onPick={() => playerController().jump(row.index)}
            />
          );
        })}
      </div>
    </div>
  );
}

/** キューの行：まとまりの見出しとスロット。 */
export type QueueRow =
  | { kind: 'group'; group: number; open: boolean }
  | { kind: 'slot'; index: number; indent: boolean };

/** キューの行を作る。ユニット単位なら見出しを付け、今のまとまりと開いたまとまりだけ中を出す。 */
export function queueRows(
  queue: Pick<Queue, 'itemIds' | 'groups'>,
  currentGroup: number | null,
  opened: ReadonlySet<number>,
): QueueRow[] {
  if (queue.groups.length === 0) {
    return queue.itemIds.map((_, index) => ({ kind: 'slot', index, indent: false }));
  }
  const rows: QueueRow[] = [];
  queue.groups.forEach((group, g) => {
    // 単体のアイテムは見出しを付けずにそのまま
    if (group.target.kind === 'item' && group.len === 1) {
      rows.push({ kind: 'slot', index: group.start, indent: false });
      return;
    }
    const open = g === currentGroup || opened.has(g);
    rows.push({ kind: 'group', group: g, open });
    if (open) {
      for (let i = group.start; i < group.start + group.len; i++) {
        rows.push({ kind: 'slot', index: i, indent: true });
      }
    }
  });
  return rows;
}

/** このセッションで再生済みになったもの（新しい順）。 */
function HistoryList() {
  const history = usePlayerStore((s) => s.history);
  useItemCacheVersion((s) => s.version);
  useEffect(() => {
    requestItems(history);
  }, [history]);
  if (history.length === 0) {
    return <p className={styles.empty}>{t.historyEmpty}</p>;
  }
  return (
    <div className={styles.list}>
      <div className={styles.sizer} style={{ height: history.length * ROW }}>
        {history.map((id, i) => {
          const item = getCached(id);
          return (
            <Row
              key={id}
              top={i * ROW}
              item={item}
              current={false}
              background={false}
              played={false}
              sub={rowSub(item, false, false, null)}
            />
          );
        })}
      </div>
    </div>
  );
}

function rowSub(
  item: ItemSummaryDto | undefined,
  current: boolean,
  background: boolean,
  imageMs: number | null,
): string {
  if (!item) {
    return '';
  }
  const kind = kindLabel(item.kind);
  if (current) {
    return t.nowPlaying(kind);
  }
  if (background) {
    return t.inBackground;
  }
  const duration =
    item.kind === 'image'
      ? imageMs !== null
        ? t.seconds(String(Math.round(imageMs / 1000)))
        : ''
      : hasDuration(item.kind) && item.durationMs !== null
        ? formatDuration(item.durationMs)
        : '';
  return duration ? t.rowSub(kind, duration) : kind;
}

function Row({
  top,
  indent = false,
  item,
  current,
  background,
  played,
  sub,
  onPick,
}: {
  top: number;
  indent?: boolean;
  item: ItemSummaryDto | undefined;
  current: boolean;
  background: boolean;
  played: boolean;
  sub: string;
  onPick?: () => void;
}) {
  const content = (
    <>
      <span className={styles.thumb} aria-hidden="true">
        {item?.thumbState === 'ready' ? (
          <img src={thumbUrl(item.hash, 's')} alt="" loading="lazy" />
        ) : (
          <KindIcon kind={item?.kind} size={18} />
        )}
      </span>
      <span className={styles.text}>
        <span className={styles.rowTitle}>{item?.title ?? ''}</span>
        <span className={styles.rowSub} data-accent={current || background || undefined}>
          {sub}
        </span>
      </span>
      {current && (
        <span className={styles.nowPlaying}>
          <NowPlayingIcon size={16} />
        </span>
      )}
    </>
  );
  const className = indent ? `${styles.row} ${styles.indented}` : styles.row;
  const style = { transform: `translateY(${top}px)` };
  if (!onPick) {
    return (
      <div className={className} style={style}>
        {content}
      </div>
    );
  }
  return (
    <button
      type="button"
      className={className}
      style={style}
      data-current={current || undefined}
      data-played={played || undefined}
      aria-current={current ? 'true' : undefined}
      onClick={onPick}
    >
      {content}
    </button>
  );
}
