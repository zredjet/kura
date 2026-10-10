import { memo } from 'react';

import { CameraIcon } from '../../components/icons/CameraIcon';
import { DownloadIcon } from '../../components/icons/DownloadIcon';
import { MusicIcon } from '../../components/icons/MusicIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { Waveform } from '../../components/icons/Waveform';
import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { thumbUrl } from '../../ipc/media';
import { formatDuration } from '../../lib/format';
import styles from './Tile.module.css';

/** ダウンロードで取得したものの取得元。 */
const DOWNLOAD_SITES = new Set(['youtube', 'pixiv', 'x', 'nicovideo', 'soundcloud']);

interface TileProps {
  id: number;
  item: ItemSummaryDto | undefined;
  width: number;
  height: number;
  selected: boolean;
  onSelect: (id: number) => void;
  onOpen: (id: number) => void;
}

/** グリッドの 1 枚（design.md §4 のタイル）。サムネイルは kura:// から読む。 */
export const Tile = memo(function Tile({
  id,
  item,
  width,
  height,
  selected,
  onSelect,
  onOpen,
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
      aria-label={item?.title ?? ''}
      data-kind={kind}
      onClick={() => onSelect(id)}
      onDoubleClick={() => onOpen(id)}
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
