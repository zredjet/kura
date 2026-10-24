import { type PointerEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';

import { MediaImage } from '../../components/MediaImage/MediaImage';
import { ja } from '../../i18n/ja';
import type { Point } from '../../lib/paint';
import styles from './Editor.module.css';
import {
  editorController,
  fillAt,
  sampleAt,
  setEditorPointer,
  strokeCancel,
  strokeEnd,
  strokeMove,
  strokeStart,
  useEditorStore,
} from './editorStore';
import { type EditorTool, useToolStore } from './toolStore';
import { type Insets, wheelPixels, wheelZoomFactor } from './viewMath';

const t = ja.editor;

/** 「全体を表示」で上部・下のカプセルからさらにあける量（CSS px）。 */
const FIT_GAP = 12;
/** 上部・下のカプセルが測れないときの量。 */
const DEFAULT_INSETS: Insets = { top: 64, right: 24, bottom: 96, left: 24 };

/** 上部（`data-editor-top`）と下のカプセル（`data-editor-capsule`）を除いた「全体を表示」の枠の余白。 */
function measureInsets(host: HTMLElement): Insets {
  const stage = host.parentElement;
  const box = host.getBoundingClientRect();
  const top = stage?.querySelector('[data-editor-top]')?.getBoundingClientRect();
  const capsule = stage?.querySelector('[data-editor-capsule]')?.getBoundingClientRect();
  return {
    top: top && top.height > 0 ? Math.max(0, top.bottom - box.top + FIT_GAP) : DEFAULT_INSETS.top,
    right: DEFAULT_INSETS.right,
    bottom:
      capsule && capsule.height > 0
        ? Math.max(0, box.bottom - capsule.top + FIT_GAP)
        : DEFAULT_INSETS.bottom,
    left: DEFAULT_INSETS.left,
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
  clientY: number;
}

/** 今の道具（押している間だけの道具を優先。⌥ を押していればペン・消しゴム・塗りつぶしはスポイト）。 */
function effectiveTool(altKey: boolean): EditorTool {
  const temporary = useEditorStore.getState().temporary;
  if (temporary) return temporary;
  const tool = useToolStore.getState().tool;
  if (altKey && (tool === 'pen' || tool === 'eraser' || tool === 'fill')) return 'eyedropper';
  return tool;
}

/**
 * キャンバス（1 枚の canvas と、その下の市松模様）。描くのは `CanvasController`。ポインタは道具に振り分ける：
 * 手のひら（Space・中ボタンも）はドラッグで動かし、ペン・消しゴムは描き、塗りつぶし・スポイトはクリック。
 * ホイールは動かし、⌃ホイール・ピンチは拡大縮小。開いた直後は、絵が出そろうまで大きいサムネイルを置く。
 */
export function EditorCanvas() {
  const hostRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const checkerRef = useRef<HTMLDivElement>(null);
  const openId = useEditorStore((s) => s.openId);
  const item = useEditorStore((s) => s.item);
  const ready = useEditorStore((s) => s.ready);
  const imageRect = useEditorStore((s) => s.imageRect);
  const temporary = useEditorStore((s) => s.temporary);
  const drawing = useEditorStore((s) => s.drawing);
  const phase = useEditorStore((s) => s.phase);
  const tool = useToolStore((s) => s.tool);
  const pan = useRef<{ x: number; y: number; id: number } | null>(null);
  const [panning, setPanning] = useState(false);

  // 描く先をつなぐ（開くたびに制御が替わる）。下のカプセルは開いてから出るので、段階が替わったら測り直す
  useLayoutEffect(() => {
    const controller = editorController();
    const canvas = canvasRef.current;
    const host = hostRef.current;
    if (!controller || !canvas || !host) return;
    controller.attach({ canvas, checker: checkerRef.current });
    const measure = () => {
      const r = host.getBoundingClientRect();
      controller.setInsets(measureInsets(host));
      controller.resize(r.width, r.height, dpr());
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    window.addEventListener('resize', measure);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', measure);
      controller.detach(canvas);
    };
  }, [openId, phase]);

  // ホイール（動かす・⌃で拡大縮小）とピンチ。ページの拡大・戻るの身ぶりを止めるため passive にしない
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let gesture: { start: number } | null = null;
    const local = (e: { clientX: number; clientY: number }) => {
      const r = host.getBoundingClientRect();
      return { x: e.clientX - r.left, y: e.clientY - r.top, w: r.width, h: r.height };
    };
    const onWheel = (e: WheelEvent) => {
      const controller = editorController();
      if (!controller) return;
      e.preventDefault();
      const p = local(e);
      const dx = wheelPixels(e.deltaX, e.deltaMode, p.w);
      const dy = wheelPixels(e.deltaY, e.deltaMode, p.h);
      if (e.ctrlKey || e.metaKey) {
        // ピンチを GestureEvent で受けている間は、同じ動きの ⌃ホイールを数えない
        if (!gesture) controller.zoomAt(wheelZoomFactor(dy), p.x, p.y);
      } else {
        controller.panBy(-dx, -dy);
      }
    };
    const onGestureStart = (e: Event) => {
      e.preventDefault();
      gesture = { start: editorController()?.zoom ?? 1 };
    };
    const onGestureChange = (e: Event) => {
      e.preventDefault();
      const g = e as GestureLike;
      const controller = editorController();
      if (!controller || !gesture) return;
      const p = local(g);
      controller.zoomGesture(gesture.start, g.scale, p.x, p.y);
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

  const pointOf = (e: { clientX: number; clientY: number }): Point | null => {
    const controller = editorController();
    const host = hostRef.current;
    if (!controller || !host) return null;
    const r = host.getBoundingClientRect();
    return controller.imagePoint(e.clientX - r.left, e.clientY - r.top);
  };

  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    const now = effectiveTool(e.altKey);
    if (e.button === 1 || (e.button === 0 && now === 'hand')) {
      e.preventDefault();
      e.currentTarget.setPointerCapture(e.pointerId);
      pan.current = { x: e.clientX, y: e.clientY, id: e.pointerId };
      setPanning(true);
      return;
    }
    if (e.button !== 0) return;
    const p = pointOf(e);
    if (!p) return;
    if (now === 'pen' || now === 'eraser') {
      if (strokeStart(p, e.shiftKey)) e.currentTarget.setPointerCapture(e.pointerId);
    } else if (now === 'fill') {
      fillAt(p);
    } else if (now === 'eyedropper') {
      void sampleAt(p);
    }
  };

  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    const controller = editorController();
    if (!controller) return;
    const drag = pan.current;
    if (drag && drag.id === e.pointerId) {
      controller.panBy(e.clientX - drag.x, e.clientY - drag.y);
      drag.x = e.clientX;
      drag.y = e.clientY;
      return;
    }
    const p = pointOf(e);
    setEditorPointer(p && controller.contains(p) ? p : null);
    if (controller.drawing) {
      const events = e.nativeEvent.getCoalescedEvents?.() ?? [];
      const points = (events.length > 0 ? events : [e.nativeEvent])
        .map(pointOf)
        .filter((q): q is Point => q !== null);
      strokeMove(points);
    }
  };

  const finish = (e: PointerEvent<HTMLCanvasElement>, canceled: boolean) => {
    if (pan.current?.id === e.pointerId) {
      pan.current = null;
      setPanning(false);
      return;
    }
    if (editorController()?.drawing) {
      if (canceled) strokeCancel();
      else strokeEnd();
    }
  };

  const shown = temporary ?? tool;
  const cursor = panning ? 'grabbing' : shown;

  return (
    <div ref={hostRef} className={styles.canvasHost} data-cursor={cursor}>
      <div ref={checkerRef} className={styles.checker} hidden aria-hidden="true" />
      <canvas
        ref={canvasRef}
        className={styles.canvas}
        role="img"
        aria-label={item ? `${t.canvas}：${item.title}` : t.canvas}
        data-drawing={drawing || undefined}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={(e) => finish(e, false)}
        onPointerCancel={(e) => finish(e, true)}
        onPointerLeave={() => setEditorPointer(null)}
        onContextMenu={(e) => e.preventDefault()}
      />
      {item && !ready && imageRect && imageRect.w > 0 && imageRect.h > 0 && (
        <div
          className={styles.placeholder}
          style={{
            left: imageRect.x,
            top: imageRect.y,
            width: imageRect.w,
            height: imageRect.h,
          }}
          aria-hidden="true"
        >
          <MediaImage
            source={item}
            size="l"
            fit="contain"
            box={{ w: imageRect.w, h: imageRect.h }}
            className={styles.placeholderImage}
          />
        </div>
      )}
    </div>
  );
}
