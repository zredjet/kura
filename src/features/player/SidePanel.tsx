import { useVirtualizer } from '@tanstack/react-virtual';
import { useEffect, useRef } from 'react';

import glass from '../../components/glass/glass.module.css';
import { ImageIcon } from '../../components/icons/ImageIcon';
import { MusicIcon } from '../../components/icons/MusicIcon';
import { NowPlayingIcon } from '../../components/icons/NowPlayingIcon';
import { VideoIcon } from '../../components/icons/VideoIcon';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { thumbUrl } from '../../ipc/media';
import { formatCount, formatDuration, formatLongDuration } from '../../lib/format';
import { getCached, requestItems, useItemCacheVersion } from '../../stores/itemCache';
import { TAG_FOCUS_KEY } from '../library/inspector/ItemInspector';
import { ItemInspector } from '../library/inspector/ItemInspector';
import { playerController, updatePlayerPrefs } from './playerActions';
import { type PlayerPanel, usePlayerStore } from './playerStore';
import styles from './SidePanel.module.css';

const t = ja.player;
/** キューの行の高さ（54px）と間隔（2px）。 */
const ROW = 56;

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
        <h2 className={styles.heading}>{t.queueTitle}</h2>
        <span className={styles.summary}>
          {t.queueSummary(formatCount(count), formatLongDuration(total))}
        </span>
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
            checked={prefs.bgmThroughImages}
            onChange={(on) => updatePlayerPrefs({ bgmThroughImages: on })}
          />
        </label>
        <label className={styles.switchRow}>
          {t.bgmLoop}
          <Switch checked={prefs.bgmLoop} onChange={(on) => updatePlayerPrefs({ bgmLoop: on })} />
        </label>
      </div>
    </>
  );
}

/** キュー（仮想スクロール）。再生済みの行は薄く、今の行に印。 */
function QueueList() {
  const queue = usePlayerStore((s) => s.queue);
  const index = usePlayerStore((s) => s.snapshot.index);
  const bgmItem = usePlayerStore((s) => s.snapshot.bgm?.itemId ?? null);
  const imageMs = usePlayerStore((s) => s.snapshot.imageMs);
  const overridden = usePlayerStore((s) => s.snapshot.imageOverridden);
  useItemCacheVersion((s) => s.version);
  const scrollRef = useRef<HTMLDivElement>(null);
  const ids = queue?.itemIds ?? [];
  // React Compiler は使っていないので、メモ化できない API の警告は当てはまらない
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtualizer = useVirtualizer({
    count: ids.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW,
    overscan: 6,
  });
  const rows = virtualizer.getVirtualItems();

  useEffect(() => {
    const wanted = rows.map((r) => ids[r.index]).filter((id) => id !== undefined);
    requestItems(wanted);
  });

  // 今のスロットが見えるように（キューを開いたとき・次へ進んだとき）
  useEffect(() => {
    virtualizer.scrollToIndex(index, { align: 'auto' });
  }, [index, virtualizer]);

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
        {rows.map((row) => {
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
              key={row.key}
              top={row.start}
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
  const kind = ja.kinds[item.kind as 'image' | 'video' | 'audio'] ?? '';
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
      : item.durationMs !== null
        ? formatDuration(item.durationMs)
        : '';
  return duration ? t.rowSub(kind, duration) : kind;
}

function Row({
  top,
  item,
  current,
  background,
  played,
  sub,
  onPick,
}: {
  top: number;
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
        ) : item?.kind === 'audio' ? (
          <MusicIcon size={18} />
        ) : item?.kind === 'video' ? (
          <VideoIcon size={18} />
        ) : (
          <ImageIcon size={18} />
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
  const className = styles.row;
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
