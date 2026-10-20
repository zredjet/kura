import { type MouseEvent, type PointerEvent, memo } from 'react';

import { CameraIcon } from '../../components/icons/CameraIcon';
import { DownloadIcon } from '../../components/icons/DownloadIcon';
import { KindIcon } from '../../components/icons/KindIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { UnitIcon } from '../../components/icons/UnitIcon';
import { Waveform } from '../../components/icons/Waveform';
import { FramesIcon } from '../../components/icons/FramesIcon';
import { RepeatIcon } from '../../components/icons/RepeatIcon';
import { KindArt } from '../../components/KindArt/KindArt';
import { MediaImage } from '../../components/MediaImage/MediaImage';
import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import { hasDuration, isItemKind, isPlayable, type ItemKind, kindLabel } from '../../ipc/kinds';
import type { UnitSummaryDto } from '../../ipc/units';
import { formatCount, formatDuration } from '../../lib/format';
import { spokenLabel } from './describeItem';
import styles from './Tile.module.css';

/** ダウンロードで取得したものの取得元。 */
const DOWNLOAD_SITES = new Set(['youtube', 'pixiv', 'x', 'nicovideo', 'soundcloud']);

/** 再生しない素材（フォント・3D）か。サムネイルがなければ種類の絵を出す（design.md §3.1）。 */
function isAsset(kind: string | undefined): kind is ItemKind {
  return isItemKind(kind) && !isPlayable(kind);
}

/** 左下に種類のバッジを出す種類（音楽と、フォント・3D。design.md §4 のタイル）。 */
function hasKindBadge(kind: string | undefined): kind is ItemKind {
  return kind === 'audio' || isAsset(kind);
}

/** ユニットの表紙を下げる分（Tile.module.css の .cover の top）。 */
const UNIT_COVER_TOP = 6;

/** サムネイルがないとき（まだ・作れなかった）の絵：音楽は波形の飾り、フォント・3D は種類の絵。 */
function placeholderOf(kind: string) {
  if (kind === 'audio') {
    return <Waveform className={styles.waveform} />;
  }
  return isAsset(kind) ? <KindArt kind={kind} /> : null;
}

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
      {item && (
        <MediaImage
          source={item}
          size="s"
          fit="cover"
          box={{ w: width, h: height }}
          className={styles.thumb}
          fallback={placeholderOf(item.kind)}
        />
      )}
      {hasKindBadge(kind) && (
        <span className={`${styles.badge} ${styles.bottomLeft}`}>
          <KindIcon kind={kind} size={10} strokeWidth={2.4} />
          {kindLabel(kind)}
        </span>
      )}
      {hasDuration(kind) && duration !== null && (
        <span className={`${styles.badge} ${styles.bottomRight}`}>
          {kind === 'video' && <PlayIcon size={8} />}
          {kind === 'audio' && item?.hasLoop && (
            <span className={styles.badgeIcon} title={ja.mediaView.loop}>
              <RepeatIcon size={10} strokeWidth={2.4} />
            </span>
          )}
          {formatDuration(duration)}
        </span>
      )}
      {kind === 'image' && item?.animated && (
        <span
          className={`${styles.badge} ${styles.bottomRight}`}
          title={item.frameCount === null ? ja.mediaView.animated : undefined}
        >
          <FramesIcon size={10} strokeWidth={2.4} />
          {item.frameCount !== null && ja.mediaView.frames(formatCount(item.frameCount))}
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
        {cover && (
          <MediaImage
            source={cover}
            size="s"
            fit="cover"
            box={{ w: width, h: height - UNIT_COVER_TOP }}
            className={styles.thumb}
            fallback={placeholderOf(cover.kind)}
          />
        )}
        <span className={`${styles.badge} ${styles.bottomRight}`}>
          {formatCount(count)}
          <UnitIcon size={11} strokeWidth={2.2} />
        </span>
      </span>
    </button>
  );
});
