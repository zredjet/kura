import {
  type CSSProperties,
  type ReactNode,
  type SyntheticEvent,
  useLayoutEffect,
  useRef,
  useState,
} from 'react';

import type { AssetAlphaDto } from '../../ipc/bindings';
import { hasThumb, thumbUrl } from '../../ipc/media';
import { type Fit, pixelFit, renderingFor, showChecker, type Size } from '../../lib/asset/pixel';
import styles from './MediaImage.module.css';

/** 絵の元（アイテムの概要か、ユニットの表紙。どちらもこの形を持つ）。 */
export interface MediaSource {
  hash: string;
  /** 種類（`ItemKindDto`）。フォントは mask、画像だけに市松模様。 */
  kind: string;
  thumbState: string;
  thumbRev: number;
  /** 解析で分かった透過（未解析は null）。 */
  alpha: AssetAlphaDto | null;
  /** ドット絵として表示するか（実効値）。 */
  pixelArt: boolean;
}

export interface MediaImageProps {
  source: MediaSource;
  /** サムネイルの大きさ（s：グリッド・行、l：プレビュー）。 */
  size: 's' | 'l';
  /**
   * 枠への収め方。cover：枠いっぱいに切り抜く（タイル・行）。contain：枠に収める（インスペクタ・クイックルック・
   * シアター。ドット絵は装置の画素の整数倍）。
   */
  fit: Fit;
  /** 枠の大きさ（CSS px）。分かっていれば渡す（タイル・行）。省けば contain のとき親要素を測る。 */
  box?: Size;
  /** サムネイルの前に試す元のファイル（クイックルック・シアター）。読めなければサムネイルにする。 */
  original?: string;
  /** 絵がないとき（サムネイルがまだ・作れなかった・読めなかった）に出すもの。 */
  fallback?: ReactNode;
  className?: string;
  loading?: 'lazy' | 'eager';
}

/** 装置の画素の比（測れなければ 1）。 */
function devicePixelRatio(): number {
  const dpr = typeof window === 'undefined' ? 1 : window.devicePixelRatio;
  return Number.isFinite(dpr) && dpr > 0 ? dpr : 1;
}

/** contain の表示の大きさ（max-width・max-height で広げない。CSS と同じ）。 */
function containSize(natural: Size, box: Size): Size {
  const k = Math.min(1, box.w / natural.w, box.h / natural.h);
  return { w: natural.w * k, h: natural.h * k };
}

/** 表示の大きさ・描き方・大きさの指定（読めた絵の大きさ `loaded` と枠 `area` が分かっていれば）。 */
function layoutOf(
  fit: Fit,
  pixelArt: boolean,
  loaded: Size | null,
  area: Size | null,
  dpr: number,
): { shown: Size | null; rendering: 'pixelated' | 'auto'; style?: CSSProperties } {
  if (!loaded || !area) {
    // 読めるまでは枠の大きさで見積もる（contain で小さく出るものは、読めたら決め直す）
    return { shown: area, rendering: 'auto' };
  }
  if (fit === 'cover') {
    return { shown: area, rendering: renderingFor(pixelArt, loaded, area, dpr, 'cover') };
  }
  if (!pixelArt) {
    return { shown: containSize(loaded, area), rendering: 'auto' };
  }
  const fitted = pixelFit(loaded.w, loaded.h, area.w, area.h, dpr);
  return {
    shown: { w: fitted.width, h: fitted.height },
    rendering: fitted.integer ? 'pixelated' : 'auto',
    style: { width: fitted.width, height: fitted.height },
  };
}

/** 親要素の中身の大きさを測る（`enabled` のときだけ）。 */
function useParentBox(
  ref: { current: HTMLElement | null },
  enabled: boolean,
  key: string,
): Size | null {
  const [box, setBox] = useState<Size | null>(null);
  useLayoutEffect(() => {
    const parent = ref.current?.parentElement;
    if (!enabled || !parent) {
      return;
    }
    const measure = (w: number, h: number) =>
      setBox((prev) => (prev && prev.w === w && prev.h === h ? prev : { w, h }));
    const style = getComputedStyle(parent);
    const padX = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight) || 0;
    const padY = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) || 0;
    measure(parent.clientWidth - padX, parent.clientHeight - padY);
    const observer = new ResizeObserver(([entry]) => {
      if (entry) {
        measure(entry.contentRect.width, entry.contentRect.height);
      }
    });
    observer.observe(parent);
    return () => observer.disconnect();
  }, [ref, enabled, key]);
  return box;
}

/**
 * サムネイル（と元のファイル）を出す唯一の部品（docs/design.md §3.1「素材の表示」）。`thumbUrl` はここでだけ呼ぶ。
 * - 市松模様：解析で透過と分かった画像だけ、`<img>` 自身の地に敷く（`data-checker`）。表示の短い辺が 48px 未満には敷かない。
 * - ドット絵：広げるときだけ `image-rendering: pixelated`（`data-render`）。contain では装置の画素の整数倍に置く。
 * - フォント：字の形だけのアルファの PNG を `mask-image` にして、色を tokens から付ける（`data-render="mask"`）。
 * - `stale`（作り直し中）も今の絵を出し、`thumbRev` を URL の `?r=` に付ける。
 */
export function MediaImage({
  source,
  size,
  fit,
  box,
  original,
  fallback = null,
  className,
  loading,
}: MediaImageProps) {
  const ref = useRef<HTMLImageElement | null>(null);
  // 読めなかった URL（元のファイル → サムネイル → fallback の順に落とす）
  const [broken, setBroken] = useState<readonly string[]>([]);
  // 読めた絵の大きさ（ドット絵の倍率・contain の表示の大きさに使う。要らなければ持たない）
  const [natural, setNatural] = useState<{ src: string; w: number; h: number } | null>(null);

  const thumb = hasThumb(source.thumbState)
    ? thumbUrl(source.hash, size, source.thumbRev)
    : undefined;
  const src = [original, thumb].find((url): url is string => !!url && !broken.includes(url));
  const checkerCandidate = source.kind === 'image' && showChecker(source.alpha);
  // 絵の大きさが要るか（ドット絵の倍率、contain で表示の短い辺を決めるとき）。要らなければ読めても描き直さない
  const needNatural = source.pixelArt || (fit === 'contain' && checkerCandidate);
  // 枠の大きさが要るか（渡されていなければ親要素を測る）
  const needArea = needNatural || checkerCandidate;
  const parentBox = useParentBox(ref, needArea && !box && src !== undefined, src ?? '');

  if (src === undefined) {
    return <>{fallback}</>;
  }

  const classes = className ? `${styles.media} ${className}` : styles.media;

  // フォント（サムネイルのとき）：字の形を mask にして色を付ける。地は枠のまま
  if (source.kind === 'font' && src === thumb) {
    const mask = `url("${src}")`;
    const style: CSSProperties = {
      WebkitMaskImage: mask,
      maskImage: mask,
      WebkitMaskSize: fit,
      maskSize: fit,
    };
    return (
      <span
        className={`${classes} ${styles.mask}`}
        data-render="mask"
        style={style}
        aria-hidden="true"
      />
    );
  }

  const loaded = natural !== null && natural.src === src ? natural : null;
  const { shown, rendering, style } = layoutOf(
    fit,
    source.pixelArt,
    loaded,
    box ?? parentBox,
    devicePixelRatio(),
  );
  // 表示の大きさが分からないうちは敷かない（小さい行で一瞬だけ出ないように）
  const checker =
    checkerCandidate && shown !== null && showChecker(source.alpha, Math.min(shown.w, shown.h));

  const onLoad = (e: SyntheticEvent<HTMLImageElement>) => {
    if (!needNatural) {
      return;
    }
    const img = e.currentTarget;
    if (img.naturalWidth > 0 && img.naturalHeight > 0) {
      setNatural({ src, w: img.naturalWidth, h: img.naturalHeight });
    }
  };
  const onError = () => setBroken((list) => (list.includes(src) ? list : [...list, src]));

  return (
    <img
      ref={ref}
      className={classes}
      src={src}
      alt=""
      decoding="async"
      loading={loading}
      draggable={false}
      data-checker={checker || undefined}
      data-render={rendering}
      style={style}
      onLoad={onLoad}
      onError={onError}
    />
  );
}
