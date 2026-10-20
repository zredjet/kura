import { KindArt } from '../../../components/KindArt/KindArt';
import { MediaImage } from '../../../components/MediaImage/MediaImage';
import { ja } from '../../../i18n/ja';
import type { ItemSummaryDto } from '../../../ipc/items';
import { isPlayable } from '../../../ipc/kinds';
import { getCached, useItemCacheVersion } from '../../../stores/itemCache';
import styles from './Inspector.module.css';

/**
 * インスペクタのプレビュー（220px の枠）：大きいサムネイルを枠に収める（透過は市松模様、ドット絵は整数倍。
 * design.md §3.1）。一覧のキャッシュに新しい概要があればそちらを使う（サムネイルの作り直し・解析が届いたら替わる）。
 */
export function ItemPreview({ item }: { item: ItemSummaryDto }) {
  useItemCacheVersion((s) => s.version);
  const live = getCached(item.id) ?? item;
  const fallback = !isPlayable(live.kind) ? (
    <KindArt kind={live.kind} />
  ) : (
    <span className={styles.previewNote}>
      {live.thumbState === 'pending' ? ja.inspector.noThumbnail : ''}
    </span>
  );
  return (
    <div className={styles.preview}>
      <MediaImage
        source={live}
        size="l"
        fit="contain"
        className={styles.previewImage}
        fallback={fallback}
      />
    </div>
  );
}
