import { type PointerEvent, useEffect, useReducer, useRef } from 'react';

import { ja } from '../../i18n/ja';
import { formatCount } from '../../lib/format';
import { CROP_HANDLES, type CropHandle, dragCrop, newCropRect } from './crop';
import styles from './Editor.module.css';
import { editorController, setCropRect, useEditorStore } from './editorStore';
import { type Rect, screenToImage, toScreen } from './viewMath';

const t = ja.editor;

/** つかんだ所と、つかんだ時の枠・点（画像の座標）。 */
interface Grab {
  id: number;
  handle: CropHandle | 'new';
  start: Rect;
  x: number;
  y: number;
}

/**
 * 切り抜きの枠（C）：その操作の入力の画像の上に枠を重ね、外を暗くする。4 隅と 4 辺のハンドルで広げ、中をドラッグして
 * 動かし、枠の外をドラッグすると新しく取る。⇧ で比を保つ。整数に吸着する。矢印・↵・esc はエディタのキー（editorKeys）。
 * パン・ズームに毎フレーム合わせる（CanvasController の変換を購読する）。
 */
export function CropOverlay() {
  const crop = useEditorStore((s) => s.crop);
  const openId = useEditorStore((s) => s.openId);
  const temporary = useEditorStore((s) => s.temporary);
  const [, redraw] = useReducer((n: number) => n + 1, 0);
  const grab = useRef<Grab | null>(null);
  const layerRef = useRef<HTMLDivElement>(null);

  const active = crop !== null;
  useEffect(() => (active ? editorController()?.subscribe(redraw) : undefined), [openId, active]);

  if (!crop) return null;
  const controller = editorController();
  if (!crop.bounds || !controller) {
    return (
      <p className={styles.cropWaiting} role="status">
        {t.cropWaiting}
      </p>
    );
  }
  const bounds = crop.bounds;
  const box = toScreen(controller.transform, crop.rect);

  const local = (e: { clientX: number; clientY: number }) => {
    const r = layerRef.current?.getBoundingClientRect();
    return screenToImage(
      controller.transform,
      e.clientX - (r?.left ?? 0),
      e.clientY - (r?.top ?? 0),
    );
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const target = e.target instanceof HTMLElement ? e.target : null;
    const handle = (target?.dataset.handle as CropHandle | undefined) ?? 'new';
    const p = local(e);
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    grab.current = { id: e.pointerId, handle, start: crop.rect, x: p.x, y: p.y };
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const g = grab.current;
    if (!g || g.id !== e.pointerId) return;
    const p = local(e);
    if (g.handle === 'new') {
      // 枠の外から：押した画素と今の画素を含む矩形（画像の外へはみ出したら端で止める）
      setCropRect(newCropRect(g.x, g.y, p.x, p.y, bounds));
      return;
    }
    setCropRect(dragCrop(g.start, g.handle, p.x - g.x, p.y - g.y, bounds, e.shiftKey));
  };

  const end = (e: PointerEvent<HTMLDivElement>) => {
    if (grab.current?.id === e.pointerId) grab.current = null;
  };

  return (
    <div
      ref={layerRef}
      className={styles.cropLayer}
      role="group"
      aria-label={t.cropFrame}
      data-passthrough={temporary === 'hand' || undefined}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={end}
      onPointerCancel={end}
    >
      <div
        className={styles.cropBox}
        data-handle="move"
        style={{ left: box.x, top: box.y, width: box.w, height: box.h }}
      >
        {CROP_HANDLES.map((h) => (
          <span key={h} className={styles.cropHandle} data-handle={h} data-pos={h} />
        ))}
        <span className={styles.cropSize} aria-live="off">
          {t.size(formatCount(crop.rect.w), formatCount(crop.rect.h))}
        </span>
      </div>
    </div>
  );
}
