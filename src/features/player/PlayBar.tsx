import type { CSSProperties } from 'react';

import { PauseIcon } from '../../components/icons/PauseIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { RepeatIcon } from '../../components/icons/RepeatIcon';
import { SkipBackIcon } from '../../components/icons/SkipBackIcon';
import { SkipForwardIcon } from '../../components/icons/SkipForwardIcon';
import { VolumeIcon } from '../../components/icons/VolumeIcon';
import { ja } from '../../i18n/ja';
import { getItemDetail } from '../../ipc/items';
import { formatDuration } from '../../lib/format';
import { useLoaded } from '../library/inspector/useLoaded';
import { playerController, updatePlayerPrefs } from './playerActions';
import { usePlayerStore } from './playerStore';
import styles from './PlayBar.module.css';

const t = ja.player;

/** 画像の表示時間を変える幅（[ / ] と −／＋）。 */
export const IMAGE_STEP_MS = 1000;

/**
 * 再生バー（Player.dc.html の下のガラスのカプセル）：再生位置、題名と「種類 · タグ」、
 * 前・再生／一時停止・次・リピート、画像の表示時間、音量。シャッフルはプレイリストと一緒に M6 で足す。
 */
export function PlayBar() {
  const snapshot = usePlayerStore((s) => s.snapshot);
  const volume = usePlayerStore((s) => s.prefs.volume);
  const controller = playerController();
  const item = snapshot.itemId !== null ? controller.item(snapshot.itemId) : undefined;
  const duration = snapshot.durationMs ?? item?.durationMs ?? null;
  const pct = duration ? Math.min(100, (snapshot.positionMs / duration) * 100) : 0;
  const kindLabel = snapshot.kind ? ja.kinds[snapshot.kind] : '';

  return (
    <div className={styles.bar}>
      <div className={styles.progressRow}>
        <span>{formatDuration(snapshot.positionMs)}</span>
        <input
          type="range"
          className={styles.seek}
          aria-label={t.seek}
          min={0}
          max={duration ?? 0}
          step={100}
          value={Math.min(snapshot.positionMs, duration ?? 0)}
          disabled={duration === null}
          style={{ '--pct': `${pct}%` } as CSSProperties}
          onChange={(e) => controller.seek(Number(e.target.value))}
        />
        <span>{duration !== null ? formatDuration(duration) : '--:--'}</span>
      </div>
      <div className={styles.controls}>
        <div className={styles.meta}>
          <span className={styles.title}>{item?.title ?? ''}</span>
          <span className={styles.sub}>
            {snapshot.itemId !== null && <TagLine itemId={snapshot.itemId} kind={kindLabel} />}
          </span>
        </div>
        <div className={styles.transport}>
          <button
            type="button"
            className={styles.small}
            aria-label={t.previous}
            onClick={() => controller.prev()}
          >
            <SkipBackIcon size={20} />
          </button>
          <button
            type="button"
            className={styles.play}
            aria-label={snapshot.playing ? t.pause : t.play}
            onClick={() => controller.togglePlay()}
          >
            {snapshot.playing ? <PauseIcon size={18} /> : <PlayIcon size={18} />}
          </button>
          <button
            type="button"
            className={styles.small}
            aria-label={t.next}
            onClick={() => controller.next()}
          >
            <SkipForwardIcon size={20} />
          </button>
          <button
            type="button"
            className={styles.toggle}
            aria-label={t.repeat}
            aria-pressed={snapshot.repeat}
            onClick={() => controller.setRepeat(!snapshot.repeat)}
          >
            <RepeatIcon size={17} strokeWidth={2} />
          </button>
        </div>
        <div className={styles.side}>
          <div className={styles.stepper} role="group" aria-label={t.imageDuration}>
            <button
              type="button"
              className={styles.stepButton}
              aria-label={t.shorter}
              onClick={() => controller.stepImage(-IMAGE_STEP_MS)}
            >
              −
            </button>
            <span className={styles.stepValue}>
              {t.imageSeconds(String(Math.round(snapshot.imageMs / 1000)))}
            </span>
            <button
              type="button"
              className={styles.stepButton}
              aria-label={t.longer}
              onClick={() => controller.stepImage(IMAGE_STEP_MS)}
            >
              ＋
            </button>
          </div>
          <label className={styles.volume}>
            <VolumeIcon size={16} />
            <input
              type="range"
              className={styles.volumeInput}
              min={0}
              max={100}
              value={volume}
              aria-label={t.volume}
              onChange={(e) => updatePlayerPrefs({ volume: Number(e.target.value) })}
            />
          </label>
        </div>
      </div>
    </div>
  );
}

/** 「種類 · タグ · タグ」。 */
function TagLine({ itemId, kind }: { itemId: number; kind: string }) {
  const detail = useLoaded(`player-${itemId}`, () => getItemDetail(itemId));
  const tags = (detail?.tags ?? [])
    .slice(0, 3)
    .map((tag) => (tag.namespace ? `${tag.namespace}:${tag.name}` : tag.name));
  return <>{[kind, ...tags].filter(Boolean).join(' · ')}</>;
}
