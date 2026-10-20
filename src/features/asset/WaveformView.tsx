import { useEffect, useMemo, useRef, useState } from 'react';

import { ja } from '../../i18n/ja';
import { peaksUrl } from '../../ipc/media';
import {
  columns,
  parsePeaks,
  type Peaks,
  pickLevel,
  regionX,
  waveformPath,
} from '../../lib/asset/peaks';
import styles from './Asset.module.css';

/** 幅を測れないとき（測る前・jsdom）の幅：インスペクタの中身の幅（300 − 14 × 2）。 */
const FALLBACK_WIDTH = 272;
/** viewBox の高さ（表示の高さは CSS の --h-waveform。縦は伸ばして描く）。 */
const VIEW_HEIGHT = 48;

/** 描く関数（テストで差し替える）。 */
export interface FrameClock {
  request(cb: () => void): number;
  cancel(id: number): void;
}

const realFrames: FrameClock = {
  request: (cb) => requestAnimationFrame(cb),
  cancel: (id) => cancelAnimationFrame(id),
};

async function fetchPeaks(url: string): Promise<ArrayBuffer | null> {
  const res = await fetch(url);
  return res.ok ? res.arrayBuffer() : null;
}

interface WaveformViewProps {
  hash: string;
  /** 読み直すきっかけ（解析の時刻など。解析が終わってから段のファイルができる）。 */
  version?: number | string | null;
  /** ループの区間 [始まり, 終わり)（フレーム）。 */
  loop: readonly [number, number] | null;
  /** 試聴中なら、鳴っている元のファイルの位置（フレーム）を返す。 */
  position?: (() => number | null) | null;
  /** 大きさを変えるとき（クイックルックなど。既定は高さ --h-waveform、幅いっぱい）。 */
  className?: string;
  /** テスト用：段のファイルを読む口・描く時刻・装置の画素の比。 */
  load?: (url: string) => Promise<ArrayBuffer | null>;
  frames?: FrameClock;
  dpr?: number;
}

/**
 * 波形（design.md §3.1「波形」）：`/peaks/{hash}` の段から、列ごとの縦の線を SVG で描く（チャンネルはまとめて 1 本、
 * 色は currentColor）。ループの区間は `--loop-region` の矩形と両端の 1px の `--accent` の線、試聴中の位置は 1.5px の線。
 * 段のファイルがない（解析の前・失敗）ときは何も描かない。
 */
export function WaveformView({
  hash,
  version = null,
  loop,
  position = null,
  className,
  load = fetchPeaks,
  frames: clock = realFrames,
  dpr = typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1,
}: WaveformViewProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(FALLBACK_WIDTH);
  const [peaks, setPeaks] = useState<{ key: string; peaks: Peaks | null } | null>(null);
  const [playhead, setPlayhead] = useState<number | null>(null);
  const key = `${hash}:${String(version)}`;
  const loadRef = useRef(load);
  useEffect(() => {
    loadRef.current = load;
  });

  useEffect(() => {
    let alive = true;
    loadRef
      .current(peaksUrl(hash))
      .then(
        (buf) => (buf ? parsePeaks(buf) : null),
        () => null,
      )
      .then((value) => {
        if (alive) setPeaks({ key, peaks: value });
      });
    return () => {
      alive = false;
    };
  }, [hash, key]);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const observer = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width;
      if (w && w > 0) setWidth(w);
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  // 試聴中の位置（描くたびに読む。変わったときだけ描き直す）
  useEffect(() => {
    if (!position) {
      return;
    }
    let id = 0;
    let last: number | null = null;
    const step = () => {
      const now = position();
      if (now !== last) {
        last = now;
        setPlayhead(now);
      }
      id = clock.request(step);
    };
    id = clock.request(step);
    return () => {
      clock.cancel(id);
      setPlayhead(null);
    };
  }, [position, clock]);

  const data = peaks?.key === key ? peaks.peaks : null;
  const n = Math.max(1, Math.round(width * dpr));
  const path = useMemo(() => {
    if (!data) return null;
    const level = pickLevel(data, data.frames, n);
    if (!level) return null;
    return waveformPath(columns(level, data.channels, data.frames, 0, data.frames, n), VIEW_HEIGHT);
  }, [data, n]);

  const total = data?.frames ?? 0;
  const region = data && loop ? regionX(loop[0], loop[1], 0, total, n) : null;
  const head =
    data && playhead !== null && total > 0
      ? Math.min(n, Math.max(0, (playhead / total) * n))
      : null;

  return (
    <div
      ref={ref}
      className={[styles.waveform, className].filter(Boolean).join(' ')}
      role="img"
      aria-label={ja.asset.waveform}
      data-loaded={data ? 'true' : 'false'}
    >
      {path !== null && (
        <svg viewBox={`0 0 ${n} ${VIEW_HEIGHT}`} preserveAspectRatio="none" aria-hidden="true">
          {region && (
            <>
              <rect
                className={styles.loopRegion}
                x={region.x}
                y={0}
                width={region.width}
                height={VIEW_HEIGHT}
              />
              <line
                className={styles.loopEdge}
                x1={region.x}
                x2={region.x}
                y1={0}
                y2={VIEW_HEIGHT}
                vectorEffect="non-scaling-stroke"
                data-edge="start"
              />
              <line
                className={styles.loopEdge}
                x1={region.x + region.width}
                x2={region.x + region.width}
                y1={0}
                y2={VIEW_HEIGHT}
                vectorEffect="non-scaling-stroke"
                data-edge="end"
              />
            </>
          )}
          <path className={styles.wavePath} d={path} />
          {head !== null && (
            <line
              className={styles.playhead}
              x1={head}
              x2={head}
              y1={0}
              y2={VIEW_HEIGHT}
              vectorEffect="non-scaling-stroke"
              data-playhead=""
            />
          )}
        </svg>
      )}
    </div>
  );
}
