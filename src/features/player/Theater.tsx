import { type ReactNode, useCallback, useState } from 'react';

import { ChevronLeftIcon } from '../../components/icons/ChevronLeftIcon';
import { InfoIcon } from '../../components/icons/InfoIcon';
import { InspectorIcon } from '../../components/icons/InspectorIcon';
import { MusicIcon } from '../../components/icons/MusicIcon';
import { TagIcon } from '../../components/icons/TagIcon';
import { Waveform } from '../../components/icons/Waveform';
import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { playUrl, thumbUrl } from '../../ipc/media';
import { formatCount, formatDuration } from '../../lib/format';
import { PlayBar } from './PlayBar';
import { closePlayer, openCurrentExternally, playerController } from './playerActions';
import { usePlayerStore } from './playerStore';
import { SidePanel } from './SidePanel';
import styles from './Theater.module.css';
import { usePlayerKeys } from './usePlayerKeys';

const t = ja.player;

/**
 * シアター（Player.dc.html）：黒地にメディアを中央に置き、上に戻る・見出し、下に再生バー、右にキュー。
 * 再生キューがあるあいだは隠しても残しておく（`<video>` を DOM から外すと再生が止まるため）。
 */
export function Theater() {
  const active = usePlayerStore((s) => s.active);
  const mode = usePlayerStore((s) => s.mode);
  const panel = usePlayerStore((s) => s.panel);
  const visible = active && mode === 'theater';
  usePlayerKeys(visible);

  return (
    <div
      className={styles.theater}
      hidden={!visible}
      data-theater={visible ? '' : undefined}
      aria-label={t.label}
      role="region"
    >
      <section className={styles.stage} aria-label={t.label}>
        {visible && <TopBar />}
        <Stage />
        {visible && <BgmPill />}
        {visible && <PlayBar />}
      </section>
      {visible && panel !== null && <SidePanel panel={panel} />}
    </div>
  );
}

/** 上部：戻る・見出し・右パネルの切り替え。 */
function TopBar() {
  const title = usePlayerStore((s) => s.title);
  const snapshot = usePlayerStore((s) => s.snapshot);
  const shuffle = usePlayerStore((s) => s.queue?.playlist?.shuffle ?? s.shuffle);
  const seed = usePlayerStore((s) => s.queue?.seed ?? '');
  const panel = usePlayerStore((s) => s.panel);
  const togglePanel = usePlayerStore((s) => s.togglePanel);
  const requestTagFocus = usePlayerStore((s) => s.requestTagFocus);
  const setMode = usePlayerStore((s) => s.setMode);

  return (
    <div className={styles.top} data-tauri-drag-region>
      <button
        type="button"
        className={styles.back}
        aria-label={t.back}
        onClick={() => setMode('mini')}
      >
        <ChevronLeftIcon size={16} strokeWidth={2} />
      </button>
      <div className={styles.heading} data-tauri-drag-region>
        <span className={styles.title}>{title}</span>
        <span className={styles.position}>
          {[
            snapshot.groupIndex !== null
              ? t.unitPosition(
                  formatCount(snapshot.groupIndex + 1),
                  formatCount(snapshot.groupCount),
                  formatCount(snapshot.indexInUnit + 1),
                  formatCount(snapshot.unitLen),
                )
              : t.position(formatCount(snapshot.index + 1), formatCount(snapshot.length)),
            shuffle ? t.shuffleSeed(seed.slice(-4).toUpperCase()) : null,
          ]
            .filter(Boolean)
            .join(' · ')}
        </span>
      </div>
      <div className={styles.segments} role="group" aria-label={t.panels}>
        <button
          type="button"
          className={styles.segment}
          aria-label={t.info}
          aria-pressed={panel === 'info'}
          onClick={() => togglePanel('info')}
        >
          <InfoIcon size={16} />
        </button>
        <button
          type="button"
          className={styles.segment}
          aria-label={t.editTags}
          onClick={requestTagFocus}
        >
          <TagIcon size={16} />
        </button>
        <button
          type="button"
          className={styles.segment}
          aria-label={t.showQueue}
          aria-pressed={panel === 'queue'}
          onClick={() => togglePanel('queue')}
        >
          <InspectorIcon size={16} />
        </button>
      </div>
    </div>
  );
}

/** メディア（動画・画像・音楽のジャケット）と、待ち・失敗・終わりの案内。 */
function Stage() {
  const snapshot = usePlayerStore((s) => s.snapshot);
  const attach = useCallback((el: HTMLVideoElement | null) => {
    playerController().attachVideo(el);
  }, []);
  const item = snapshot.itemId !== null ? playerController().item(snapshot.itemId) : undefined;
  const showMedia = snapshot.phase !== 'waiting' && snapshot.phase !== 'failed';

  return (
    <div className={styles.media}>
      {/* 手元のメディアには字幕ファイルがない */}
      {/* eslint-disable-next-line jsx-a11y/media-has-caption */}
      <video
        ref={attach}
        className={styles.video}
        hidden={snapshot.kind !== 'video' || !showMedia}
        playsInline
      />
      {item && snapshot.kind === 'image' && <ImageLayer key={item.id} item={item} />}
      {item && snapshot.kind === 'audio' && showMedia && <AudioArt key={item.id} item={item} />}
      {snapshot.phase === 'waiting' && (
        <Status title={t.waiting} hint={t.waitingHint}>
          <button type="button" className={styles.statusButton} onClick={openCurrentExternally}>
            {t.openExternal}
          </button>
        </Status>
      )}
      {snapshot.phase === 'failed' && (
        <Status title={t.failed} hint={t.failedHint}>
          <button type="button" className={styles.statusButton} onClick={openCurrentExternally}>
            {t.openExternal}
          </button>
          <button
            type="button"
            className={styles.statusButton}
            onClick={() => playerController().retry()}
          >
            {t.retry}
          </button>
        </Status>
      )}
      {snapshot.phase === 'ended' && (
        <Status title={t.ended}>
          <button
            type="button"
            className={styles.statusButton}
            onClick={() => playerController().play()}
          >
            {t.playAgain}
          </button>
          <button type="button" className={styles.statusButton} onClick={closePlayer}>
            {t.close}
          </button>
        </Status>
      )}
    </div>
  );
}

/** 画像（ブラウザで表示できない形式は大きいサムネイルにする）。 */
function ImageLayer({ item }: { item: ItemSummaryDto }) {
  const [fallback, setFallback] = useState(false);
  const src = fallback ? thumbUrl(item.hash, 'l') : playUrl(item.hash);
  return (
    <img
      className={styles.image}
      src={src}
      alt=""
      onError={() => {
        if (!fallback && item.thumbState === 'ready') {
          setFallback(true);
        }
      }}
    />
  );
}

/** 音楽：ジャケット（なければ音符）と波形の飾り。 */
function AudioArt({ item }: { item: ItemSummaryDto }) {
  return (
    <div className={styles.audio}>
      {item.thumbState === 'ready' ? (
        <img className={styles.art} src={thumbUrl(item.hash, 'l')} alt="" />
      ) : (
        <span className={styles.artEmpty} aria-hidden="true">
          <MusicIcon size={64} strokeWidth={1.4} />
        </span>
      )}
      <Waveform className={styles.waveform} />
    </div>
  );
}

function Status({ title, hint, children }: { title: string; hint?: string; children: ReactNode }) {
  return (
    <div className={styles.status} role="status">
      <p className={styles.statusTitle}>{title}</p>
      {hint && <p className={styles.statusHint}>{hint}</p>}
      <div className={styles.statusActions}>{children}</div>
    </div>
  );
}

/** 画像の間に流している音楽（Player.dc.html の「… を背景で再生中 · 2:41 / 4:18」）。 */
function BgmPill() {
  const bgm = usePlayerStore((s) => s.snapshot.bgm);
  const title = bgm ? playerController().item(bgm.itemId)?.title : undefined;
  if (!bgm || title === undefined) {
    return null;
  }
  const time =
    bgm.durationMs !== null
      ? `${formatDuration(bgm.positionMs)} / ${formatDuration(bgm.durationMs)}`
      : formatDuration(bgm.positionMs);
  return (
    <div className={styles.bgm}>
      <span className={styles.bgmIcon}>
        <MusicIcon size={12} strokeWidth={2.4} />
      </span>
      {t.bgm(title, time)}
    </div>
  );
}
