/**
 * エディタのキャンバスに描く（`CanvasController` の `Scene` を 2D の canvas に描く。テストでは差し替える）。
 *
 * - タイルは画像の矩形の中だけに `drawImage`。拡大は最近傍（`imageSmoothingEnabled = false`）、縮小は補間。
 * - 仮の筆の区画は、広げるときは最近傍、縮めるときは補間で描く。重ねるは色を source-over、置き換えは覆いで destination-out してから色、消しゴムは
 *   覆いで destination-out（canvas の下の市松模様が見える）。区画の帯は区画ごとの canvas に貼り、中身が変わったときだけ
 *   貼り直す。
 * - 市松模様は canvas の下の要素の位置・大きさを決め、模様を面に固定する（`background-position`）。
 */

import type { DrawScene, Scene } from './canvasController';
import { CHUNK, type OverlayChunk } from './stroke';

interface ChunkSurfaces {
  version: number;
  color: HTMLCanvasElement;
  mask: HTMLCanvasElement;
}

const chunkSurfaces = new WeakMap<OverlayChunk, ChunkSurfaces>();

function blankCanvas(): HTMLCanvasElement {
  const c = document.createElement('canvas');
  c.width = CHUNK;
  c.height = CHUNK;
  return c;
}

function upload(target: HTMLCanvasElement, data: Uint8ClampedArray): void {
  const ctx = target.getContext('2d');
  if (!ctx) return;
  ctx.putImageData(new ImageData(new Uint8ClampedArray(data), CHUNK, CHUNK), 0, 0);
}

/** 区画の canvas（中身が変わっていれば貼り直す）。 */
function surfacesOf(chunk: OverlayChunk): ChunkSurfaces {
  let s = chunkSurfaces.get(chunk);
  if (!s) {
    s = { version: -1, color: blankCanvas(), mask: blankCanvas() };
    chunkSurfaces.set(chunk, s);
  }
  if (s.version !== chunk.version) {
    upload(s.color, chunk.color);
    upload(s.mask, chunk.mask);
    s.version = chunk.version;
  }
  return s;
}

/** 市松模様の要素を、画像の見えている矩形に合わせる。 */
function placeChecker(el: HTMLElement, scene: Scene): void {
  const r = scene.checker;
  if (!r) {
    el.hidden = true;
    return;
  }
  el.hidden = false;
  el.style.left = `${r.x}px`;
  el.style.top = `${r.y}px`;
  el.style.width = `${r.w}px`;
  el.style.height = `${r.h}px`;
  el.style.backgroundPosition = `${-r.x}px ${-r.y}px`;
}

export const drawScene: DrawScene = ({ canvas, checker }, scene) => {
  if (checker) placeChecker(checker, scene);
  const ctx = canvas.getContext('2d');
  if (!ctx) return;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.globalCompositeOperation = 'source-over';
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  const clip = scene.clip;
  if (!clip) return;
  ctx.save();
  ctx.beginPath();
  ctx.rect(clip.x, clip.y, clip.w, clip.h);
  ctx.clip();
  for (const tile of scene.tiles) {
    ctx.imageSmoothingEnabled = tile.smoothing;
    if (tile.smoothing) ctx.imageSmoothingQuality = 'high';
    if (tile.clip) {
      ctx.save();
      ctx.beginPath();
      ctx.rect(tile.clip.x, tile.clip.y, tile.clip.w, tile.clip.h);
      ctx.clip();
    }
    ctx.drawImage(tile.image as unknown as CanvasImageSource, tile.dx, tile.dy, tile.dw, tile.dh);
    if (tile.clip) ctx.restore();
  }
  for (const overlay of scene.overlays) {
    ctx.imageSmoothingEnabled = overlay.smoothing;
    for (const c of overlay.chunks) {
      const s = surfacesOf(c.chunk);
      if (overlay.mode !== 'over') {
        ctx.globalCompositeOperation = 'destination-out';
        ctx.drawImage(s.mask, c.dx, c.dy, c.dw, c.dh);
      }
      if (overlay.mode !== 'erase') {
        ctx.globalCompositeOperation = 'source-over';
        ctx.drawImage(s.color, c.dx, c.dy, c.dw, c.dh);
      }
    }
  }
  ctx.globalCompositeOperation = 'source-over';
  ctx.restore();
};
