import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { Select } from '../../components/Select/Select';
import { ChevronLeftIcon } from '../../components/icons/ChevronLeftIcon';
import { ChevronRightIcon } from '../../components/icons/ChevronRightIcon';
import { PauseIcon } from '../../components/icons/PauseIcon';
import { PlayIcon } from '../../components/icons/PlayIcon';
import { ja } from '../../i18n/ja';
import type { AnimDto } from '../../ipc/asset';
import { animUrl } from '../../ipc/media';
import { type AlphaKind, checkerFor, pixelFit } from '../../lib/asset/pixel';
import {
  type AnimTag,
  type Cell,
  frameAt,
  startOfStep,
  stripCell,
  track,
} from '../../lib/asset/timeline';
import { formatCount } from '../../lib/format';
import styles from './Asset.module.css';

const t = ja.asset;

/** 幅を測れないとき（測る前・jsdom）の枠の幅：インスペクタの中身の幅。 */
const FALLBACK_WIDTH = 272;
/** インスペクタのプレビューの枠の高さ（Inspector.module.css の .preview と同じ）。 */
const FRAME_HEIGHT = 220;
/** 枠を広げる（クイックルック）ときに、高さを測れないときの高さ。 */
const FALLBACK_FILL_HEIGHT = 480;

/** 回すコマの元（帯の PNG か、格子を切る元の画像）。 */
export interface AnimSource {
  url: string;
  /** 1 コマの大きさ（キャンバス・セル）。 */
  canvasW: number;
  canvasH: number;
  frameCount: number;
  /** コマの長さ（ms。0 は 100ms として描く）。 */
  durations: readonly number[];
  tags: readonly AnimTag[];
  /** コマ f を切り出す元の矩形。 */
  rect(frame: number): Cell;
}

/** item_anim_get の帯（キャンバスの大きさのセルを行ごとに左から cols 個ずつ並べた PNG）。 */
export function stripSource(anim: AnimDto): AnimSource {
  return {
    url: animUrl(anim.hash, anim.rev),
    canvasW: anim.canvasW,
    canvasH: anim.canvasH,
    frameCount: anim.frameCount,
    durations: anim.durations,
    tags: anim.tags,
    rect: (f) => ({
      ...stripCell(f, anim.cols, anim.canvasW, anim.canvasH),
      w: anim.canvasW,
      h: anim.canvasH,
    }),
  };
}

/** 格子のシート（元の画像からセルを切る。格子のシートのその場のプレビュー）。 */
export function gridSource(url: string, cells: readonly Cell[], frameMs: number): AnimSource {
  const first = cells[0];
  return {
    url,
    canvasW: first?.w ?? 0,
    canvasH: first?.h ?? 0,
    frameCount: cells.length,
    durations: cells.map(() => frameMs),
    tags: [],
    rect: (f) => cells[f] ?? first ?? { x: 0, y: 0, w: 0, h: 0 },
  };
}

/** 時刻と描く時刻（テストで偽の時計にする）。 */
export interface AnimClock {
  now(): number;
  request(cb: () => void): number;
  cancel(id: number): void;
}

const realClock: AnimClock = {
  now: () => performance.now(),
  request: (cb) => requestAnimationFrame(cb),
  cancel: (id) => cancelAnimationFrame(id),
};

/** コマを canvas に描く（元の矩形を canvas いっぱいに）。補間しないときは最近傍。 */
export type DrawFrame = (
  canvas: HTMLCanvasElement,
  image: CanvasImageSource,
  src: Cell,
  smoothing: boolean,
) => void;

export const drawFrame: DrawFrame = (canvas, image, src, smoothing) => {
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  ctx.imageSmoothingEnabled = smoothing;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(image, src.x, src.y, src.w, src.h, 0, 0, canvas.width, canvas.height);
};

/** 画像を読む（decode を待つ）。 */
export async function loadImage(url: string): Promise<CanvasImageSource> {
  const img = new Image();
  img.src = url;
  await img.decode();
  return img;
}

/** 「視差効果を減らす」がオンか。 */
export function prefersReducedMotion(): boolean {
  return typeof window !== 'undefined' && typeof window.matchMedia === 'function'
    ? window.matchMedia('(prefers-reduced-motion: reduce)').matches
    : false;
}

/** 表示の大きさ（CSS px）と補間するか。ドット絵は箱に収まる最大の整数倍（装置の画素で）、ほかは広げない。 */
export function displaySize(
  w: number,
  h: number,
  boxW: number,
  boxH: number,
  dpr: number,
  pixelArt: boolean,
): { width: number; height: number; smoothing: boolean } {
  if (!(w > 0 && h > 0 && boxW > 0 && boxH > 0)) {
    return { width: 0, height: 0, smoothing: true };
  }
  if (pixelArt) {
    const fit = pixelFit(w, h, boxW, boxH, dpr);
    return { width: fit.width, height: fit.height, smoothing: !fit.integer };
  }
  const s = Math.min(1, boxW / w, boxH / h);
  return { width: w * s, height: h * s, smoothing: true };
}

interface AnimPlayerProps {
  source: AnimSource;
  /** ドット絵の実効値（広げるときだけ最近傍・整数倍）。 */
  pixelArt: boolean;
  /** 解析の透過（二値・半透明なら市松模様を敷く）。 */
  alpha: AlphaKind | null;
  /** 初めに選ぶタグ（素材の設定の animTag）。 */
  initialTag?: string | null;
  /** 枠の高さ（CSS px）。'fill' は親いっぱいに広げて測る（クイックルック）。 */
  frameHeight?: number | 'fill';
  /** テスト用：時計・描く関数・画像を読む口・動きを減らす設定・装置の画素の比。 */
  clock?: AnimClock;
  draw?: DrawFrame;
  load?: (url: string) => Promise<CanvasImageSource>;
  reducedMotion?: boolean;
  dpr?: number;
}

/**
 * アニメのプレビュー（design.md §3.1）：canvas の drawImage でコマを描く（隣のコマがにじまない）。経過時間から
 * `frameAt` でコマを決め、タイマーの刻みに頼らない。ピンポンは端を重ねず、タグの繰り返しの回数とファイルのループの
 * 回数を使わずに回し続ける。「視差効果を減らす」では自動で再生しない（先頭のコマと再生ボタン）。
 * 枠の下に 28px の行：再生／一時停止、「3 / 8」、一時停止中はコマ送り、タグがあればタグの選択（保存しない）。
 */
export function AnimPlayer({
  source,
  pixelArt,
  alpha,
  initialTag = null,
  frameHeight = FRAME_HEIGHT,
  clock = realClock,
  draw = drawFrame,
  load = loadImage,
  reducedMotion = prefersReducedMotion(),
  dpr = typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1,
}: AnimPlayerProps) {
  const frameRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const fill = frameHeight === 'fill';
  // 測る前は仮の大きさで描く（measured は偽。市松模様は測ってから敷く）
  const [box, setBox] = useState({
    w: FALLBACK_WIDTH,
    h: fill ? FALLBACK_FILL_HEIGHT : frameHeight,
    measured: false,
  });
  const [image, setImage] = useState<{ url: string; image: CanvasImageSource | null } | null>(null);
  const [tagName, setTagName] = useState<string | null>(() =>
    source.tags.some((tag) => tag.name === initialTag) ? initialTag : null,
  );
  const [playing, setPlaying] = useState(!reducedMotion);
  const [step, setStep] = useState(0);
  /** 経過時間の基準：止めたときの経過（base）と、回し始めた時刻（startedAt）。 */
  const timeRef = useRef({ base: 0, startedAt: 0 });

  const tag = source.tags.find((x) => x.name === tagName);
  const tr = useMemo(() => track(source.durations, tag), [source.durations, tag]);
  const moving = playing && tr.steps.length > 1;

  const loadRef = useRef(load);
  useEffect(() => {
    loadRef.current = load;
  });
  useEffect(() => {
    let alive = true;
    loadRef.current(source.url).then(
      (img) => {
        if (alive) setImage({ url: source.url, image: img });
      },
      () => {
        if (alive) setImage({ url: source.url, image: null });
      },
    );
    return () => {
      alive = false;
    };
  }, [source.url]);
  const ready = image?.url === source.url ? image.image : undefined;

  useEffect(() => {
    const el = frameRef.current;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect && rect.width > 0 && rect.height > 0) {
        setBox({ w: rect.width, h: rect.height, measured: true });
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const boxH = fill ? box.h : frameHeight;
  const size = displaySize(source.canvasW, source.canvasH, box.w, boxH, dpr, pixelArt);
  const paint = useCallback(
    (frame: number) => {
      const canvas = canvasRef.current;
      if (canvas && ready) {
        draw(canvas, ready, source.rect(frame), size.smoothing);
      }
    },
    [ready, draw, source, size.smoothing],
  );

  // 回している間：描く時刻ごとに経過時間からコマを決め、変わったときだけ描く
  useEffect(() => {
    if (!ready || !moving) {
      return;
    }
    let id = 0;
    let lastFrame = -1;
    let lastStep = -1;
    const tick = () => {
      const time = timeRef.current;
      const { frame, step: s } = frameAt(tr, time.base + (clock.now() - time.startedAt));
      if (frame !== lastFrame) {
        lastFrame = frame;
        paint(frame);
      }
      if (s !== lastStep) {
        lastStep = s;
        setStep(s);
      }
      id = clock.request(tick);
    };
    id = clock.request(tick);
    return () => clock.cancel(id);
    // 大きさが変わると canvas が消えるので、回し直して描き直す
  }, [ready, moving, tr, paint, clock, size.width, size.height]);

  // 止めている間：選んでいる位置のコマ（大きさが変わったら描き直す）
  const shownFrame = tr.steps[step] ?? tr.steps[0] ?? 0;
  useEffect(() => {
    if (ready && !moving) {
      paint(shownFrame);
    }
  }, [ready, moving, shownFrame, paint, size.width, size.height]);

  // 回し始めの基準（初めの 1 回）
  useEffect(() => {
    timeRef.current = { base: 0, startedAt: clock.now() };
  }, [clock]);

  const togglePlay = () => {
    const time = timeRef.current;
    const now = clock.now();
    if (moving) {
      time.base += now - time.startedAt;
      setStep(frameAt(tr, time.base).step);
      setPlaying(false);
    } else {
      time.startedAt = now;
      setPlaying(true);
    }
  };
  const go = (delta: number) => {
    const n = tr.steps.length;
    if (n === 0) return;
    const next = (((step + delta) % n) + n) % n;
    timeRef.current.base = startOfStep(tr, next);
    setStep(next);
  };
  const chooseTag = (name: string) => {
    setTagName(name === '' ? null : name);
    timeRef.current = { base: 0, startedAt: clock.now() };
    setStep(0);
  };

  // MediaImage と同じ規則（アニメは画像だけ）。枠を測るまでは表示の大きさが分からないので敷かない
  const checker = checkerFor(
    'image',
    alpha,
    box.measured ? { w: size.width, h: size.height } : null,
  );
  const tagOptions = [
    { value: '', label: t.allFrames },
    ...source.tags.map((x) => ({ value: x.name, label: x.name })),
  ];
  return (
    <>
      <div
        ref={frameRef}
        className={fill ? `${styles.frame} ${styles.fill}` : styles.frame}
        style={fill ? undefined : { height: frameHeight }}
      >
        {ready === null ? (
          <p className={styles.note}>{t.animFailed}</p>
        ) : (
          <canvas
            ref={canvasRef}
            className={[styles.canvas, checker ? styles.checker : ''].filter(Boolean).join(' ')}
            width={Math.max(1, Math.round(size.width * dpr))}
            height={Math.max(1, Math.round(size.height * dpr))}
            style={{ width: size.width, height: size.height }}
            data-render={size.smoothing ? 'auto' : 'pixelated'}
            data-checker={checker ? 'true' : undefined}
          />
        )}
      </div>
      <div className={styles.controls}>
        <button
          type="button"
          className={styles.roundButton}
          aria-label={moving ? t.pause : t.play}
          onClick={togglePlay}
          disabled={tr.steps.length < 2}
        >
          {moving ? <PauseIcon size={12} /> : <PlayIcon size={12} />}
        </button>
        {!moving && tr.steps.length > 1 && (
          <>
            <button
              type="button"
              className={styles.stepButton}
              aria-label={t.previousFrame}
              onClick={() => go(-1)}
            >
              <ChevronLeftIcon size={12} strokeWidth={2} />
            </button>
            <button
              type="button"
              className={styles.stepButton}
              aria-label={t.nextFrame}
              onClick={() => go(1)}
            >
              <ChevronRightIcon size={12} strokeWidth={2} />
            </button>
          </>
        )}
        <span className={styles.position} aria-live="off">
          {t.framePosition(formatCount(shownFrame + 1), formatCount(source.frameCount))}
        </span>
        {source.tags.length > 0 && (
          <Select
            className={styles.tagSelect}
            aria-label={t.tag}
            value={tagName ?? ''}
            options={tagOptions}
            onChange={chooseTag}
          />
        )}
      </div>
    </>
  );
}
