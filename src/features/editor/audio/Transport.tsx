import { useEffect, useRef } from 'react';

import { PlayIcon } from '../../../components/icons/PlayIcon';
import { RepeatIcon } from '../../../components/icons/RepeatIcon';
import { SkipBackIcon } from '../../../components/icons/SkipBackIcon';
import { StopIcon } from '../../../components/icons/StopIcon';
import { Segmented } from '../../../components/Segmented/Segmented';
import { ja } from '../../../i18n/ja';
import editorStyles from '../Editor.module.css';
import { useEditorStore } from '../editorStore';
import styles from './Audio.module.css';
import {
  audioPlayhead,
  playheadToStart,
  toggleAB,
  toggleLoopAudition,
  togglePlay,
  useAudioStore,
} from './audioStore';
import type { PlaySource } from './playback';
import { timeText } from './timeText';

const t = ja.editor.audio;

/** 「0:12.345 / 3:12.500」（鳴らしている間は描く時刻ごとに書き換える。React は描き直さない）。 */
function Position() {
  const ref = useRef<HTMLSpanElement>(null);
  const cursor = useAudioStore((s) => s.cursor);
  const rate = useAudioStore((s) => s.view?.rate ?? 0);
  const frames = useAudioStore((s) => s.view?.frames ?? 0);
  const playing = useAudioStore((s) => s.play.status === 'playing');
  const total = timeText(frames, rate);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const write = (pos: number) => {
      el.textContent = t.position(timeText(pos, rate), total);
    };
    write(cursor);
    if (!playing) return;
    let id = 0;
    const step = () => {
      const pos = audioPlayhead();
      if (pos !== null) write(pos);
      id = requestAnimationFrame(step);
    };
    id = requestAnimationFrame(step);
    return () => cancelAnimationFrame(id);
  }, [cursor, rate, total, playing]);
  return <span ref={ref} className={styles.position} aria-live="off" />;
}

/**
 * 再生の操作（下のカプセルの左）：先頭へ・再生（Space）・ループを試聴（L）・聞き比べ「元｜編集後」（B）・時刻。
 */
export function Transport() {
  const open = useEditorStore((s) => s.phase === 'open');
  const play = useAudioStore((s) => s.play);
  const ab = useAudioStore((s) => s.ab);
  const canCompare = useAudioStore((s) => s.view?.sourceMap != null && s.source !== null);
  const hasLoop = useAudioStore((s) =>
    s.local && 'loop' in s.local ? s.local.loop != null : s.view?.loop != null,
  );
  const playing = play.status === 'playing';
  const looping = playing && play.mode === 'loop';
  return (
    <div className={styles.transport} role="group" aria-label={t.transport}>
      <button
        type="button"
        className={editorStyles.iconButton}
        aria-label={t.toStart}
        title={t.toStart}
        disabled={!open}
        onClick={playheadToStart}
      >
        <SkipBackIcon size={14} />
      </button>
      <button
        type="button"
        className={`${editorStyles.iconButton} ${styles.playButton}`}
        aria-label={playing ? t.stop : t.play}
        title={ja.editor.withKey(playing ? t.stop : t.play, 'Space')}
        aria-pressed={playing && !looping}
        disabled={!open}
        onClick={togglePlay}
      >
        {playing ? <StopIcon size={14} /> : <PlayIcon size={14} />}
      </button>
      <button
        type="button"
        className={editorStyles.iconButton}
        aria-label={looping ? t.loopAuditionStop : t.loopAudition}
        title={ja.editor.withKey(looping ? t.loopAuditionStop : t.loopAudition, 'L')}
        aria-pressed={looping}
        disabled={!open || !hasLoop}
        onClick={toggleLoopAudition}
      >
        <RepeatIcon size={14} />
      </button>
      <span title={canCompare ? ja.editor.withKey(t.abLabel, 'B') : t.abUnavailable}>
        <Segmented<PlaySource>
          label={t.abLabel}
          value={ab}
          disabled={!open || !canCompare}
          options={[
            { value: 'source', label: t.abSource },
            { value: 'edited', label: t.abEdited },
          ]}
          onChange={(v) => {
            if (v !== ab) toggleAB();
          }}
        />
      </span>
      <Position />
    </div>
  );
}
