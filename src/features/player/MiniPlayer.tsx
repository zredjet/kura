import type { CSSProperties } from 'react';

import { CloseIcon } from '../../components/icons/CloseIcon';
import { ExpandIcon } from '../../components/icons/ExpandIcon';
import { KindIcon } from '../../components/icons/KindIcon';
import { PauseIcon } from '../../components/icons/PauseIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { SkipBackIcon } from '../../components/icons/SkipBackIcon';
import { SkipForwardIcon } from '../../components/icons/SkipForwardIcon';
import { ja } from '../../i18n/ja';
import { hasDuration, kindLabel } from '../../ipc/kinds';
import { thumbUrl } from '../../ipc/media';
import { formatCount, formatDuration } from '../../lib/format';
import { closePlayer, playerController } from './playerActions';
import { usePlayerStore } from './playerStore';
import styles from './MiniPlayer.module.css';

const t = ja.player;

/** ミニプレーヤーが出ているか（取り込みの進捗をその上に積むため）。 */
export function useMiniPlayerVisible(): boolean {
  return usePlayerStore((s) => s.active && s.mode === 'mini');
}

/**
 * ミニプレーヤー（Main.dc.html のグリッドの下中央）：ジャケット、題名と「6 / 42 · 次は画像（8秒）」、
 * 前・再生／一時停止・次、シアター表示で開く。下端に再生位置の線。
 */
export function MiniPlayer() {
  const visible = useMiniPlayerVisible();
  const snapshot = usePlayerStore((s) => s.snapshot);
  const queue = usePlayerStore((s) => s.queue);
  const title = usePlayerStore((s) => s.title);
  const setMode = usePlayerStore((s) => s.setMode);
  if (!visible || !queue) {
    return null;
  }
  const controller = playerController();
  const item = snapshot.itemId !== null ? controller.item(snapshot.itemId) : undefined;
  const nextId = queue.itemIds[snapshot.index + 1];
  const next = nextId !== undefined ? controller.item(nextId) : undefined;
  const duration = snapshot.durationMs ?? item?.durationMs ?? null;
  const pct = duration ? Math.min(100, (snapshot.positionMs / duration) * 100) : 0;

  let upNext: string = t.lastItem;
  if (next) {
    const kind = kindLabel(next.kind);
    const ms =
      next.kind === 'image'
        ? snapshot.imageOverridden
          ? snapshot.imageMs
          : (queue.imageDurationMs[snapshot.index + 1] ?? snapshot.imageMs)
        : hasDuration(next.kind)
          ? next.durationMs
          : null;
    const length =
      ms === null
        ? ''
        : next.kind === 'image'
          ? t.seconds(String(Math.round(ms / 1000)))
          : formatDuration(ms);
    upNext = length ? t.nextUp(kind, length) : kind;
  }
  const position = t.position(formatCount(snapshot.index + 1), formatCount(snapshot.length));

  return (
    <div className={styles.capsule} role="region" aria-label={t.miniLabel}>
      <span className={styles.art} aria-hidden="true">
        {item?.thumbState === 'ready' ? (
          <img src={thumbUrl(item.hash, 's')} alt="" />
        ) : (
          <KindIcon kind={item?.kind} size={18} />
        )}
      </span>
      <span className={styles.text}>
        <span className={styles.title}>{item?.title ?? ''}</span>
        <span className={styles.sub}>{[title, position, upNext].filter(Boolean).join(' · ')}</span>
      </span>
      <button
        type="button"
        className={styles.button}
        aria-label={t.previous}
        onClick={() => controller.prev()}
      >
        <SkipBackIcon size={16} />
      </button>
      <button
        type="button"
        className={styles.play}
        aria-label={snapshot.playing ? t.pause : t.play}
        onClick={() => controller.togglePlay()}
      >
        {snapshot.playing ? <PauseIcon size={14} /> : <PlayIcon size={14} />}
      </button>
      <button
        type="button"
        className={styles.button}
        aria-label={t.next}
        onClick={() => controller.next()}
      >
        <SkipForwardIcon size={16} />
      </button>
      <button
        type="button"
        className={styles.button}
        aria-label={t.openTheater}
        onClick={() => setMode('theater')}
      >
        <ExpandIcon size={16} />
      </button>
      <button type="button" className={styles.button} aria-label={t.close} onClick={closePlayer}>
        <CloseIcon size={14} strokeWidth={2.2} />
      </button>
      <span
        className={styles.progress}
        style={{ '--pct': `${pct}%` } as CSSProperties}
        aria-hidden="true"
      />
    </div>
  );
}
