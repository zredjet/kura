/**
 * エディタのタイルを読んで解く（docs/editing.md §12.13）。
 *
 * - 中身はどの画素の形式でも RGBA8（ストレート）。拡張子なしは行を詰めた非圧縮（`w × h × 4`）、`.png` は PNG。
 * - 既定の形式は仮に RGBA8（段 5 の最後に WebKit で両方を測って決める。測るのは `src/dev/tileCheck.ts`
 *   （`VITE_KURA_CHECK=1`））。
 */

import type { EditTileFormat } from '../../ipc/media';

/** 既定のタイルの形式（段 5 の最後に測って決める）。 */
export const TILE_FORMAT: EditTileFormat = 'rgba';

/** タイルの応えが 200 でなかった。 */
export class TileHttpError extends Error {
  constructor(readonly status: number) {
    super(`タイルを読めませんでした（${status}）`);
    this.name = 'TileHttpError';
  }
}

/** 解いたタイル（ImageBitmap）。 */
export type DecodedTile = ImageBitmap;

/** 行を詰めた RGBA8 を ImageBitmap にする（長さが合わなければ例外）。 */
export async function decodeRgba(bytes: ArrayBuffer, w: number, h: number): Promise<DecodedTile> {
  if (bytes.byteLength !== w * h * 4 || w <= 0 || h <= 0) {
    throw new Error(`タイルの大きさが合いません（${bytes.byteLength} バイト。${w}×${h}）`);
  }
  const data = new ImageData(new Uint8ClampedArray(bytes), w, h);
  return createImageBitmap(data);
}

/** PNG を ImageBitmap にする。 */
export function decodePng(blob: Blob): Promise<DecodedTile> {
  return createImageBitmap(blob);
}

/** タイルを読んで解く（`w`・`h` は段の画素のタイルの大きさ。RGBA8 の長さの確かめに使う）。 */
export async function loadTile(
  url: string,
  format: EditTileFormat,
  w: number,
  h: number,
  init?: RequestInit,
): Promise<DecodedTile> {
  const res = await fetch(url, init);
  if (!res.ok) throw new TileHttpError(res.status);
  if (format === 'png') return decodePng(await res.blob());
  return decodeRgba(await res.arrayBuffer(), w, h);
}
