import { useCallback, useEffect, useId, useRef } from 'react';

import { ChevronLeftIcon } from '../../components/icons/ChevronLeftIcon';
import { ChevronRightIcon } from '../../components/icons/ChevronRightIcon';
import { CloseIcon } from '../../components/icons/CloseIcon';
import { KindArt } from '../../components/KindArt/KindArt';
import { ja } from '../../i18n/ja';
import { isUnitEntry } from '../../ipc/items';
import { isPlayable } from '../../ipc/kinds';
import { fileUrl, playUrl, thumbUrl } from '../../ipc/media';
import { formatCount } from '../../lib/format';
import { getCached, requestItems, useItemCacheVersion } from '../../stores/itemCache';
import { useLibraryStore } from '../../stores/libraryStore';
import { useViewStore } from '../../stores/viewStore';
import { FontSpecimen, QuickLookAnim, useFontFace } from '../asset/AssetStage';
import { useAssetData } from '../asset/useAssetData';
import { WaveformView } from '../asset/WaveformView';
import styles from './QuickLook.module.css';

const t = ja.quickLook;

/** クイックルック（design.md §3.1：黒地の全画面、上に名前と「3 / 128」、左右に移動、esc で閉じる）。 */
export function QuickLook() {
  const open = useViewStore((s) => s.quickLook);
  const selectedId = useViewStore((s) => s.selection.focus);
  const select = useViewStore((s) => s.select);
  const setQuickLook = useViewStore((s) => s.setQuickLook);
  const ids = useLibraryStore((s) => s.list?.ids);
  useItemCacheVersion((s) => s.version);
  const titleId = useId();

  const index = selectedId !== null && ids ? ids.indexOf(selectedId) : -1;
  const item = selectedId !== null ? getCached(selectedId) : undefined;
  // 素材（M11）：アニメは帯を canvas で回し、音声は波形と再生の位置を出す
  const assetData = useAssetData(open && item && item.kind !== 'video' ? item.id : null);
  const asset = assetData.asset?.itemId === item?.id ? assetData.asset : null;
  const anim = asset && assetData.anim.status === 'ready' ? assetData.anim.anim : null;
  const audioRef = useRef<HTMLAudioElement>(null);
  const audioSeconds = useCallback(() => audioRef.current?.currentTime ?? null, []);
  const specimen = useFontFace(open && item?.kind === 'font' ? item.hash : null);

  useEffect(() => {
    if (open && selectedId !== null && !item) {
      requestItems([selectedId]);
    }
  }, [open, selectedId, item]);

  if (!open || selectedId === null || !ids || index < 0) {
    return null;
  }
  // ユニットのタイルは飛ばして、前後のアイテムへ
  const neighbor = (delta: number): number | undefined => {
    for (let i = index + delta; i >= 0 && i < ids.length; i += delta) {
      const id = ids[i];
      if (id !== undefined && !isUnitEntry(id)) {
        return id;
      }
    }
    return undefined;
  };
  const go = (delta: number) => {
    const next = neighbor(delta);
    if (next !== undefined) {
      select(next);
    }
  };

  return (
    <div className={styles.overlay} role="dialog" aria-modal="true" aria-labelledby={titleId}>
      <header className={styles.header}>
        <h2 id={titleId} className={styles.title}>
          {item?.title ?? ''}
        </h2>
        <span className={styles.position}>
          {t.position(formatCount(index + 1), formatCount(ids.length))}
        </span>
        <button
          type="button"
          className={styles.close}
          aria-label={t.close}
          onClick={() => setQuickLook(false)}
        >
          <CloseIcon size={16} strokeWidth={2.2} />
        </button>
      </header>

      <div className={styles.stage}>
        {asset && anim && (
          <QuickLookAnim key={`${anim.hash}:${anim.rev}`} asset={asset} anim={anim} />
        )}
        {item?.kind === 'image' && !anim && (
          <img
            key={item.id}
            className={styles.media}
            src={fileUrl(item.hash)}
            alt=""
            // ブラウザで表示できない形式は大きいサムネイルにする
            onError={(e) => {
              e.currentTarget.src = thumbUrl(item.hash, 'l');
            }}
          />
        )}
        {item?.kind === 'video' && (
          // 手元のメディアには字幕ファイルがないので、字幕の規則は当てはまらない
          // eslint-disable-next-line jsx-a11y/media-has-caption
          <video
            key={item.id}
            className={styles.media}
            src={playUrl(item.hash)}
            controls
            autoPlay
          />
        )}
        {item?.kind === 'audio' && (
          <div key={item.id} className={styles.audio}>
            {item.thumbState === 'ready' && (
              <img className={styles.art} src={thumbUrl(item.hash, 'l')} alt="" />
            )}
            {/* 手元のメディアには字幕ファイルがない */}
            {/* eslint-disable-next-line jsx-a11y/media-has-caption */}
            <audio ref={audioRef} src={playUrl(item.hash)} controls autoPlay />
            <WaveformView
              hash={item.hash}
              version={asset?.facts?.analyzedAt ?? null}
              loop={
                asset?.facts?.audio.loopRange
                  ? [asset.facts.audio.loopRange.start, asset.facts.audio.loopRange.end]
                  : null
              }
              positionSeconds={audioSeconds}
              className={styles.wave}
            />
          </div>
        )}
        {item?.kind === 'font' && specimen && (
          <FontSpecimen
            key={item.id}
            family={specimen}
            name={asset?.font?.family ?? item.title}
            className={styles.specimen}
          />
        )}
        {/* フォント・3D：見本・描いたサムネイルは段 3。それまでは種類の絵 */}
        {item && !isPlayable(item.kind) && !(item.kind === 'font' && specimen) && (
          <KindArt key={item.id} kind={item.kind} className={styles.kindArt} />
        )}
      </div>

      <button
        type="button"
        className={`${styles.nav} ${styles.prev}`}
        aria-label={t.previous}
        disabled={neighbor(-1) === undefined}
        onClick={() => go(-1)}
      >
        <ChevronLeftIcon size={22} strokeWidth={2} />
      </button>
      <button
        type="button"
        className={`${styles.nav} ${styles.next}`}
        aria-label={t.next}
        disabled={neighbor(1) === undefined}
        onClick={() => go(1)}
      >
        <ChevronRightIcon size={22} strokeWidth={2} />
      </button>
    </div>
  );
}
