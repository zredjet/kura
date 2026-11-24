import { type PointerEvent, useEffect, useLayoutEffect, useRef } from 'react';

import { ja } from '../../../i18n/ja';
import { useEditorStore } from '../editorStore';
import { wheelPixels, wheelZoomFactor } from '../image/viewMath';
import styles from './Audio.module.css';
import { audioController, audioPlayhead, setCursor, useAudioStore } from './audioStore';
import { AUDIO_POINTER } from './audioTools';
import {
  DEFAULT_COLORS,
  DEFAULT_METRICS,
  type WaveColors,
  type WaveInsets,
  type WaveMetrics,
} from './waveController';

const t = ja.editor.audio;

/** 上部・下のカプセルからさらにあける量（CSS px）。 */
const FIT_GAP = 12;
/** 上部・下のカプセルが測れないときの量。 */
const DEFAULT_INSETS: WaveInsets = { top: 64, right: 16, bottom: 96, left: 16 };

/** 上部（`data-editor-top`）と下のカプセル（`data-editor-capsule`）を避ける余白。左右は --space-4。 */
export function measureWaveInsets(host: HTMLElement): WaveInsets {
  // スペクトルの面の帯（M17 段 5）は舞台の中の箱に入るので、エディタの根から探す
  const stage = host.closest('[data-editor]') ?? host.parentElement;
  const box = host.getBoundingClientRect();
  const top = stage?.querySelector('[data-editor-top]')?.getBoundingClientRect();
  const capsule = stage?.querySelector('[data-editor-capsule]')?.getBoundingClientRect();
  const side =
    Number.parseFloat(getComputedStyle(host).getPropertyValue('--space-4')) || DEFAULT_INSETS.left;
  return {
    top: top && top.height > 0 ? Math.max(0, top.bottom - box.top + FIT_GAP) : DEFAULT_INSETS.top,
    right: side,
    bottom:
      capsule && capsule.height > 0
        ? Math.max(0, box.bottom - capsule.top + FIT_GAP)
        : DEFAULT_INSETS.bottom,
    left: side,
  };
}

/** CSS の色の変数を canvas に渡せる色にする（`color-mix` も計算した色で読む）。読めなければ既定。 */
export function resolveWaveColors(host: HTMLElement): WaveColors {
  const probe = document.createElement('span');
  probe.style.display = 'none';
  host.appendChild(probe);
  const read = (token: string, fallback: string): string => {
    probe.style.color = '';
    probe.style.color = `var(${token})`;
    const value = getComputedStyle(probe).color;
    return value && value !== '' ? value : fallback;
  };
  const d = DEFAULT_COLORS;
  const colors: WaveColors = {
    wave: read('--text-secondary', d.wave),
    loop: read('--loop-region', d.loop),
    accent: read('--accent', d.accent),
    selection: read('--wave-selection', d.selection),
    outside: read('--wave-outside', d.outside),
    handle: read('--text-on-media', d.handle),
    cursor: read('--text-primary', d.cursor),
    separator: read('--stroke-1', d.separator),
    label: read('--text-tertiary', d.label),
    overviewWindow: read('--fill-2', d.overviewWindow),
    overviewFrame: read('--text-secondary', d.overviewFrame),
  };
  probe.remove();
  return colors;
}

/** 描く大きさ（--h-wave-ruler・--h-wave-overview）と文字（--text-xs・--font-ui）。 */
export function readWaveMetrics(host: HTMLElement): WaveMetrics {
  const cs = getComputedStyle(host);
  const px = (token: string, fallback: number) =>
    Number.parseFloat(cs.getPropertyValue(token)) || fallback;
  const size = cs.getPropertyValue('--text-xs').trim() || '11px';
  const family = cs.getPropertyValue('--font-ui').trim() || 'sans-serif';
  return {
    ruler: px('--h-wave-ruler', DEFAULT_METRICS.ruler),
    overview: px('--h-wave-overview', DEFAULT_METRICS.overview),
    font: `${size} ${family}`,
  };
}

/** 装置の画素の比（測れなければ 1）。 */
function dpr(): number {
  const d = window.devicePixelRatio;
  return Number.isFinite(d) && d > 0 ? d : 1;
}

/** Safari のピンチ（GestureEvent）の要るところ。 */
interface GestureLike extends Event {
  scale: number;
  clientX: number;
}

/**
 * 波形の舞台（1 枚の canvas と、再生位置の要素）。描くのは `WaveController`。ホイール・横スワイプで動かし、⌘／⌃ホイール・
 * ピンチでポインタを中心に拡大縮小する。再生している間は、描く時刻ごとに再生位置を読んで要素を動かし、外へ出たら
 * 1 画面ぶん飛ぶ。
 */
export function WaveStage() {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const playheadRef = useRef<HTMLDivElement>(null);
  const openId = useEditorStore((s) => s.openId);
  const phase = useEditorStore((s) => s.phase);
  const item = useEditorStore((s) => s.item);
  const playing = useAudioStore((s) => s.play.status === 'playing');
  const drag = useRef<{ id: number; overview: boolean } | null>(null);

  // 描く先をつなぐ（開くたびに制御が替わる）。下のカプセルは開いてから出るので、段階が替わったら測り直す
  useLayoutEffect(() => {
    const controller = audioController();
    const canvas = canvasRef.current;
    const host = hostRef.current;
    if (!controller || !canvas || !host) return;
    controller.colors = resolveWaveColors(host);
    controller.metrics = readWaveMetrics(host);
    controller.attach(canvas);
    const measure = () => {
      const r = host.getBoundingClientRect();
      controller.setInsets(measureWaveInsets(host));
      controller.resize(r.width, r.height, dpr());
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    // 下のカプセルの高さは道具・一言・折り返しで替わる（2 段）ので、それも測り直す
    const capsule = (host.closest('[data-editor]') ?? host.parentElement)?.querySelector(
      '[data-editor-capsule]',
    );
    if (capsule) observer.observe(capsule);
    window.addEventListener('resize', measure);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', measure);
      controller.detach(canvas);
    };
  }, [openId, phase]);

  // ホイール（動かす・⌘／⌃で拡大縮小）とピンチ。ページの拡大・戻るの身ぶりを止めるため passive にしない
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let gesture: { last: number } | null = null;
    const localX = (e: { clientX: number }) => e.clientX - host.getBoundingClientRect().left;
    const onWheel = (e: WheelEvent) => {
      const controller = audioController();
      if (!controller) return;
      e.preventDefault();
      const w = host.getBoundingClientRect().width;
      const dx = wheelPixels(e.deltaX, e.deltaMode, w);
      const dy = wheelPixels(e.deltaY, e.deltaMode, w);
      if (e.ctrlKey || e.metaKey) {
        if (!gesture) controller.zoomAt(wheelZoomFactor(dy), localX(e));
      } else {
        // 縦のホイールも横のパンにする（波形は横にしか動かない）
        controller.panBy(Math.abs(dx) >= Math.abs(dy) ? dx : dy);
      }
    };
    const onGestureStart = (e: Event) => {
      e.preventDefault();
      gesture = { last: 1 };
    };
    const onGestureChange = (e: Event) => {
      e.preventDefault();
      const g = e as GestureLike;
      const controller = audioController();
      if (!controller || !gesture || !(g.scale > 0)) return;
      controller.zoomAt(g.scale / gesture.last, localX(g));
      gesture.last = g.scale;
    };
    const onGestureEnd = (e: Event) => {
      e.preventDefault();
      gesture = null;
    };
    host.addEventListener('wheel', onWheel, { passive: false });
    host.addEventListener('gesturestart', onGestureStart);
    host.addEventListener('gesturechange', onGestureChange);
    host.addEventListener('gestureend', onGestureEnd);
    return () => {
      host.removeEventListener('wheel', onWheel);
      host.removeEventListener('gesturestart', onGestureStart);
      host.removeEventListener('gesturechange', onGestureChange);
      host.removeEventListener('gestureend', onGestureEnd);
    };
  }, []);

  // 再生位置：描く時刻ごとに読んで動かす（canvas は描き直さない）。外へ出たら 1 画面ぶん飛ぶ
  useEffect(() => {
    const el = playheadRef.current;
    if (!el) return;
    if (!playing) {
      el.hidden = true;
      return;
    }
    let id = 0;
    const step = () => {
      const controller = audioController();
      const pos = audioPlayhead();
      if (controller && pos !== null) {
        controller.follow(pos);
        const inner = controller.inner();
        const layout = controller.layout();
        const first = layout.lanes[0];
        const last = layout.lanes[layout.lanes.length - 1];
        const top = inner.y + (first?.y ?? 0);
        const bottom = inner.y + (last ? last.y + last.h : 0);
        el.style.transform = `translate(${controller.xOf(pos)}px, ${top}px)`;
        el.style.height = `${Math.max(0, bottom - top)}px`;
        el.hidden = false;
      } else {
        el.hidden = true;
      }
      id = requestAnimationFrame(step);
    };
    id = requestAnimationFrame(step);
    return () => cancelAnimationFrame(id);
  }, [playing]);

  const local = (e: { clientX: number; clientY: number }) => {
    const r = hostRef.current?.getBoundingClientRect();
    return { x: e.clientX - (r?.left ?? 0), y: e.clientY - (r?.top ?? 0) };
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    if (e.button !== 0) return;
    const controller = audioController();
    if (!controller) return;
    const { x, y } = local(e);
    if (AUDIO_POINTER.down(e, x, y)) {
      e.currentTarget.setPointerCapture(e.pointerId);
      drag.current = { id: e.pointerId, overview: false };
      return;
    }
    const hit = controller.hit(x, y, { loop: null, fade: null, trim: null, selection: null });
    if (hit.kind === 'overview') {
      controller.panToOverview(x);
      e.currentTarget.setPointerCapture(e.pointerId);
      drag.current = { id: e.pointerId, overview: true };
    } else if (hit.kind === 'lanes' || hit.kind === 'ruler') {
      setCursor(controller.frameAt(x));
    }
  };

  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    const { x, y } = local(e);
    const host = hostRef.current;
    if (!d || d.id !== e.pointerId) {
      if (host) host.dataset.cursor = AUDIO_POINTER.cursor(x, y) ?? '';
      return;
    }
    if (d.overview) audioController()?.panToOverview(x);
    else AUDIO_POINTER.move(e, x, y);
  };

  const finish = (e: PointerEvent<HTMLCanvasElement>, canceled: boolean) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    drag.current = null;
    if (!d.overview) {
      const { x, y } = local(e);
      AUDIO_POINTER.up(e, x, y, canceled);
    }
  };

  return (
    <div ref={hostRef} className={styles.waveHost}>
      <canvas
        ref={canvasRef}
        className={styles.wave}
        role="img"
        aria-label={item ? t.waveOf(item.title) : t.wave}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={(e) => finish(e, false)}
        onPointerCancel={(e) => finish(e, true)}
        onContextMenu={(e) => e.preventDefault()}
      />
      <div ref={playheadRef} className={styles.playhead} hidden aria-hidden="true" />
    </div>
  );
}
