import { PauseIcon } from '../../components/icons/PauseIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { RepeatIcon } from '../../components/icons/RepeatIcon';
import { RepeatOneIcon } from '../../components/icons/RepeatOneIcon';
import { SeekRow } from '../../components/SeekBar/SeekRow';
import { ShuffleIcon } from '../../components/icons/ShuffleIcon';
import { SkipBackIcon } from '../../components/icons/SkipBackIcon';
import { SkipForwardIcon } from '../../components/icons/SkipForwardIcon';
import { VolumeIcon } from '../../components/icons/VolumeIcon';
import { ja } from '../../i18n/ja';
import { getItemDetail } from '../../ipc/items';
import { kindLabel } from '../../ipc/kinds';
import { useLoaded } from '../library/inspector/useLoaded';
import { cycleRepeat, playerController, toggleShuffle, updatePlayerPrefs } from './playerActions';
import { usePlayerStore } from './playerStore';
import styles from './PlayBar.module.css';

const t = ja.player;

/** 画像の表示時間を変える幅（[ / ] と −／＋）。 */
export const IMAGE_STEP_MS = 1000;

/**
 * 再生バー（Player.dc.html の下のガラスのカプセル）：再生位置（共通のシークの行。時刻の入力・ドラッグ・⌥ の微調整。
 * ADR-230）、題名と「種類 · タグ」、シャッフル・前・再生／一時停止・次・リピート（オフ → 全体 → 1 曲）、画像の表示時間、音量。
 */
export function PlayBar() {
  const snapshot = usePlayerStore((s) => s.snapshot);
  const volume = usePlayerStore((s) => s.prefs.volume);
  const shuffle = usePlayerStore((s) => s.queue?.playlist?.shuffle ?? s.shuffle);
  const controller = playerController();
  const item = snapshot.itemId !== null ? controller.item(snapshot.itemId) : undefined;
  const length = snapshot.durationMs ?? item?.durationMs ?? null;
  // トリムの終わりがあれば、シークの範囲はそこまで（コントローラの seek と同じ終わり。ADR-230）
  const trimEnd = usePlayerStore((s) => s.queue?.endMs[s.snapshot.index] ?? null);
  const duration = slotEnd(length, trimEnd);
  const kindText = kindLabel(snapshot.kind);

  return (
    <div className={styles.bar}>
      <SeekRow
        positionMs={snapshot.positionMs}
        durationMs={duration}
        onSeek={(ms) => controller.seek(ms)}
        onScrubStart={() => controller.beginScrub()}
        onScrubEnd={() => controller.endScrub()}
        resetKey={`${snapshot.index}:${snapshot.itemId ?? ''}`}
        disabled={snapshot.itemId === null}
      />
      <div className={styles.controls}>
        <div className={styles.meta}>
          <span className={styles.title}>{item?.title ?? ''}</span>
          <span className={styles.sub}>
            {snapshot.itemId !== null && <TagLine itemId={snapshot.itemId} kind={kindText} />}
          </span>
        </div>
        <div className={styles.transport}>
          <button
            type="button"
            className={styles.toggle}
            aria-label={t.shuffle}
            aria-pressed={shuffle}
            onClick={() => void toggleShuffle()}
          >
            <ShuffleIcon size={17} strokeWidth={2} />
          </button>
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
            aria-label={snapshot.repeat === 'one' ? t.repeatOne : t.repeat}
            aria-pressed={snapshot.repeat !== 'off'}
            onClick={cycleRepeat}
          >
            {snapshot.repeat === 'one' ? (
              <RepeatOneIcon size={17} strokeWidth={2} />
            ) : (
              <RepeatIcon size={17} strokeWidth={2} />
            )}
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

/** スロットの終わり（ms）：長さとトリムの終わりの早いほう。どちらも分からなければ null。 */
export function slotEnd(length: number | null, trimEnd: number | null): number | null {
  if (length === null) {
    return trimEnd;
  }
  return trimEnd !== null ? Math.min(length, trimEnd) : length;
}

/** 「種類 · タグ · タグ」。 */
function TagLine({ itemId, kind }: { itemId: number; kind: string }) {
  const detail = useLoaded(`player-${itemId}`, () => getItemDetail(itemId));
  const tags = (detail?.tags ?? [])
    .slice(0, 3)
    .map((tag) => (tag.namespace ? `${tag.namespace}:${tag.name}` : tag.name));
  return <>{[kind, ...tags].filter(Boolean).join(' · ')}</>;
}
