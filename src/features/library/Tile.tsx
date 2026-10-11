import { type MouseEvent, type PointerEvent, memo } from 'react';

import { CameraIcon } from '../../components/icons/CameraIcon';
import { DownloadIcon } from '../../components/icons/DownloadIcon';
import { MusicIcon } from '../../components/icons/MusicIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { UnitIcon } from '../../components/icons/UnitIcon';
import { Waveform } from '../../components/icons/Waveform';
import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { thumbUrl } from '../../ipc/media';
import type { UnitSummaryDto } from '../../ipc/units';
import { formatCount, formatDuration } from '../../lib/format';
import { spokenLabel } from './describeItem';
import styles from './Tile.module.css';

/** ダウンロードで取得したものの取得元。 */
const DOWNLOAD_SITES = new Set(['youtube', 'pixiv', 'x', 'nicovideo', 'soundcloud']);

/** タイルのマウス・ドラッグの受け口（グリッドが持つ）。 */
export interface TileHandlers {
  onClick: (id: number, e: MouseEvent) => void;
  onOpen: (id: number) => void;
  onContextMenu: (id: number, e: MouseEvent) => void;
  /** ドラッグの見張りを始める（dragState.ts）。 */
  onPointerDown: (id: number, e: PointerEvent) => void;
}

interface TileProps {
  id: number;
  item: ItemSummaryDto | undefined;
  width: number;
  height: number;
  selected: boolean;
  /** ドラッグで落とす位置（このタイルの前か後ろ）。 */
  dropSide?: 'before' | 'after';
  handlers: TileHandlers;
}

/** グリッドの 1 枚（design.md §4 のタイル）。サムネイルは kura:// から読む。 */
export const Tile = memo(function Tile({
  id,
  item,
  width,
  height,
  selected,
  dropSide,
  handlers,
}: TileProps) {
  const kind = item?.kind;
  const duration = item?.durationMs ?? null;
  const site = item?.sourceSite ?? 'local';
  return (
    <button
      type="button"
      className={styles.tile}
      style={{ width, height }}
      aria-pressed={selected}
      aria-label={item ? spokenLabel(item) : ''}
      data-kind={kind}
      data-entry={id}
      data-drop-entry={id}
      data-drop={dropSide}
      onClick={(e) => handlers.onClick(id, e)}
      onDoubleClick={() => handlers.onOpen(id)}
      onContextMenu={(e) => handlers.onContextMenu(id, e)}
      onPointerDown={(e) => handlers.onPointerDown(id, e)}
    >
      {item?.thumbState === 'ready' && (
        <img
          className={styles.thumb}
          src={thumbUrl(item.hash, 's')}
          alt=""
          decoding="async"
          draggable={false}
        />
      )}
      {kind === 'audio' && (
        <>
          {item?.thumbState !== 'ready' && <Waveform className={styles.waveform} />}
          <span className={`${styles.badge} ${styles.bottomLeft}`}>
            <MusicIcon size={10} strokeWidth={2.4} />
            {ja.tile.music}
          </span>
        </>
      )}
      {(kind === 'audio' || kind === 'video') && duration !== null && (
        <span className={`${styles.badge} ${styles.bottomRight}`}>
          {kind === 'video' && <PlayIcon size={8} />}
          {formatDuration(duration)}
        </span>
      )}
      {DOWNLOAD_SITES.has(site) && (
        <span className={`${styles.mark} ${styles.topLeft}`} title={ja.tile.downloaded}>
          <DownloadIcon size={12} strokeWidth={2.4} />
        </span>
      )}
      {site === 'capture' && (
        <span className={`${styles.mark} ${styles.topLeft}`} title={ja.tile.captured}>
          <CameraIcon size={12} strokeWidth={2.2} />
        </span>
      )}
    </button>
  );
});

interface UnitTileProps {
  /** 一覧の ID（`LIST_FLAG | unitId`）。 */
  id: number;
  unit: UnitSummaryDto | undefined;
  width: number;
  height: number;
  selected: boolean;
  handlers: TileHandlers;
}

/**
 * ユニットのタイル（design.md §3.1）：表紙の奥に 2 枚の板（上に 3px と 6px ずらす）、右下に件数。
 * ダブルクリック・Return で中身を開く。
 */
export const UnitTile = memo(function UnitTile({
  id,
  unit,
  width,
  height,
  selected,
  handlers,
}: UnitTileProps) {
  const cover = unit?.cover ?? null;
  const count = unit?.count ?? 0;
  return (
    <button
      type="button"
      className={`${styles.tile} ${styles.unit}`}
      style={{ width, height }}
      aria-pressed={selected}
      aria-label={ja.tile.unit(unit?.name ?? '', formatCount(count))}
      data-entry={id}
      onClick={(e) => handlers.onClick(id, e)}
      onDoubleClick={() => handlers.onOpen(id)}
      onContextMenu={(e) => handlers.onContextMenu(id, e)}
      onPointerDown={(e) => handlers.onPointerDown(id, e)}
    >
      <span className={`${styles.plate} ${styles.plateBack}`} />
      <span className={`${styles.plate} ${styles.plateMiddle}`} />
      <span className={styles.cover} data-kind={cover?.kind}>
        {cover?.thumbState === 'ready' && (
          <img
            className={styles.thumb}
            src={thumbUrl(cover.hash, 's')}
            alt=""
            decoding="async"
            draggable={false}
          />
        )}
        {cover?.kind === 'audio' && cover.thumbState !== 'ready' && (
          <Waveform className={styles.waveform} />
        )}
        <span className={`${styles.badge} ${styles.bottomRight}`}>
          {formatCount(count)}
          <UnitIcon size={11} strokeWidth={2.2} />
        </span>
      </span>
    </button>
  );
});
