import { useEffect } from 'react';

import { PauseIcon } from '../../components/icons/PauseIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { VolumeIcon } from '../../components/icons/VolumeIcon';
import { SeekRow } from '../../components/SeekBar/SeekRow';
import { ja } from '../../i18n/ja';
import { watchFrames } from '../../lib/frameProbe';
import { DEFAULT_FRAME_MS } from '../../lib/frameRate';
import { clampSeek } from '../../lib/seek';
import { JUMP_MS } from '../../lib/seekKeys';
import { updatePlayerPrefs } from '../player/playerActions';
import { usePlayerStore } from '../player/playerStore';
import styles from './QuickLook.module.css';
import {
  applyVolume,
  beginMediaScrub,
  endMediaScrub,
  mediaSeekTarget,
  registerQuickLookMedia,
  seekMedia,
  toggleMedia,
} from './quickLookMedia';
import { useMediaElement } from './useMediaElement';

const t = ja.quickLook;

/**
 * クイックルックの再生の操作（design.md §3.1「クイックルック」・ADR-230）：ステージの下のガラスのカプセルに、再生／
 * 一時停止・5 秒戻す・5 秒進める・シークの行（時刻の入力・ドラッグ・⌥ の微調整）・音量（プレーヤーと同じ設定）。
 * 付いている間は、この要素をシークのキー（, . < > J K L）の当て先にし、動画ならコマの長さを集める。
 */
export function QuickLookControls({
  media,
  itemId,
}: {
  media: HTMLMediaElement;
  /** アイテム（替わったらドラッグを捨てる）。 */
  itemId: number;
}) {
  const state = useMediaElement(media);
  const volume = usePlayerStore((s) => s.prefs.volume);

  useEffect(() => {
    applyVolume(media, volume);
  }, [media, volume]);

  useEffect(() => {
    const watch = media instanceof HTMLVideoElement ? watchFrames(media) : null;
    const unregister = registerQuickLookMedia(
      mediaSeekTarget(media, () => (watch ? (watch.frameMs() ?? DEFAULT_FRAME_MS) : null)),
    );
    return () => {
      unregister();
      watch?.stop();
    };
  }, [media]);

  const playing = !state.paused && !state.ended;
  const jump = (delta: number) =>
    seekMedia(media, clampSeek(state.currentMs + delta, state.durationMs));

  return (
    <div className={styles.controls} role="group" aria-label={t.controls}>
      <button
        type="button"
        className={styles.play}
        aria-label={playing ? t.pause : t.play}
        title={`${playing ? t.pause : t.play}（K）`}
        onClick={() => toggleMedia(media)}
      >
        {playing ? <PauseIcon size={18} /> : <PlayIcon size={18} />}
      </button>
      <button
        type="button"
        className={styles.skip}
        aria-label={t.back}
        title={`${t.back}（J）`}
        disabled={state.durationMs === null}
        onClick={() => jump(-JUMP_MS)}
      >
        {t.backText}
      </button>
      <button
        type="button"
        className={styles.skip}
        aria-label={t.forward}
        title={`${t.forward}（L）`}
        disabled={state.durationMs === null}
        onClick={() => jump(JUMP_MS)}
      >
        {t.forwardText}
      </button>
      <SeekRow
        className={styles.seekRow}
        positionMs={state.currentMs}
        durationMs={state.durationMs}
        onSeek={(ms) => seekMedia(media, ms)}
        onScrubStart={() => beginMediaScrub(media)}
        onScrubEnd={() => endMediaScrub(media)}
        resetKey={itemId}
      />
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
  );
}
