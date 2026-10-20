import { type ReactNode, useEffect, useState } from 'react';

import { ja } from '../../i18n/ja';
import type { AnimDto, AssetDto } from '../../ipc/asset';
import { fileUrl } from '../../ipc/media';
import { AnimPlayer, stripSource } from './AnimPlayer';
import styles from './Asset.module.css';
import type { AssetData } from './useAssetData';

const t = ja.asset;

/** FontFace を作る口（テストで偽物にする。jsdom には FontFace がない）。 */
export interface FontLoader {
  load(family: string, url: string): Promise<{ release(): void }>;
}

/**
 * ライブラリのフォントを FontFace で kura:// から読んで登録する（design.md §3.1「フォントの見本」。外部のフォントは
 * 読まないが、ライブラリのフォントの試し表示は例外。CSP の font-src kura:）。
 */
export const fontLoader: FontLoader = {
  async load(family, url) {
    const face = new FontFace(family, `url("${url}")`);
    await face.load();
    document.fonts.add(face);
    return { release: () => document.fonts.delete(face) };
  },
};

/** 書体の名前（ハッシュごとに 1 つ。利用者の書体名とぶつからないよう接頭辞を付ける）。 */
export function specimenFamily(hash: string): string {
  return `kura-specimen-${hash.slice(0, 16)}`;
}

/** ライブラリのフォントを読む。読めたら書体の名前、読めない・読んでいる間は null。離れたら登録を外す。 */
export function useFontFace(hash: string | null, loader: FontLoader = fontLoader): string | null {
  const [loaded, setLoaded] = useState<string | null>(null);
  useEffect(() => {
    if (hash === null) {
      return;
    }
    const family = specimenFamily(hash);
    let alive = true;
    let release: (() => void) | null = null;
    let pending: Promise<{ release(): void }>;
    try {
      pending = loader.load(family, fileUrl(hash));
    } catch {
      return;
    }
    pending.then(
      (handle) => {
        if (alive) {
          release = () => handle.release();
          setLoaded(family);
        } else {
          handle.release();
        }
      },
      () => undefined,
    );
    return () => {
      alive = false;
      release?.();
    };
  }, [hash, loader]);
  return hash !== null && loaded === specimenFamily(hash) ? loaded : null;
}

/** フォントの見本（上に「永あAa」を --text-specimen、下に「いろはにほへと ABCabc 0123」を --text-lg）。 */
export function FontSpecimen({ family, name }: { family: string; name: string }) {
  return (
    <div className={styles.frame}>
      <div
        className={styles.specimen}
        style={{ fontFamily: `"${family}"` }}
        role="img"
        aria-label={t.specimenLabel(name)}
      >
        <span className={styles.specimenLarge} aria-hidden="true">
          {t.specimenLarge}
        </span>
        <span className={styles.specimenSmall} aria-hidden="true">
          {t.specimenSmall}
        </span>
      </div>
    </div>
  );
}

/** アニメのプレビュー（インスペクタの 220px の枠と、その下の操作の行）。 */
export function AnimStage({ asset, anim }: { asset: AssetDto; anim: AnimDto }) {
  return (
    <div className={styles.stage}>
      <AnimPlayer
        key={`${anim.hash}:${anim.rev}`}
        source={stripSource(anim)}
        pixelArt={asset.effective.pixelArt}
        alpha={asset.facts?.image.alpha ?? null}
        initialTag={asset.settings.animTag}
      />
    </div>
  );
}

/**
 * インスペクタのプレビューを置き換えるもの（アニメ・フォントの見本）。置き換えないときは null（今のプレビューの枠の
 * まま。帯を作れない・フォントを読めないときもサムネイルに戻る）。
 */
export function useAssetStage(data: AssetData, loader?: FontLoader): ReactNode | null {
  const { asset, anim } = data;
  const family = useFontFace(asset?.kind === 'font' ? asset.hash : null, loader);
  if (!asset) {
    return null;
  }
  if (asset.kind === 'font' && family) {
    return (
      <div className={styles.stage}>
        <FontSpecimen family={family} name={asset.font?.family ?? asset.hash} />
      </div>
    );
  }
  if (asset.kind === 'image' && anim.status === 'ready') {
    return <AnimStage asset={asset} anim={anim.anim} />;
  }
  return null;
}
