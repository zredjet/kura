/**
 * 出力と元のフレームの写し（M13 の段 6。設計 §2.5 の聞き比べ）。表示の結果の `sourceMap`（出力の区分ごとに、元の
 * 頭のフレームとレートの比。Rust の `audio/timeline.rs` の `source_map`）で写す。
 *
 * - `toSource`：出力の `[outStart, outEnd)` の位置 `p` → 元の `inStart + round((p − outStart) × inRate / outRate)`。
 * - `fromSource`：元の位置 → 出力の位置（区分を先頭から見て、最初に元の区間に入る区分で写す。ループで同じ元を 2 度
 *   使う出力はないので、ふつうは 1 つ）。
 * - 区分の外・写しがない（null）は null（聞き比べで再生位置を出さない）。
 */

import type { EditSourcePieceDto } from '../../../ipc/edit';

/** 区分の元の区間の長さ（フレーム）。 */
function inLength(p: EditSourcePieceDto): number {
  return Math.round(((p.outEnd - p.outStart) * p.inRate) / p.outRate);
}

/** 出力の位置を元の位置にする（写せなければ null）。 */
export function toSource(pos: number, map: readonly EditSourcePieceDto[] | null): number | null {
  if (!map || !Number.isFinite(pos)) return null;
  for (const p of map) {
    if (pos >= p.outStart && pos < p.outEnd && p.outRate > 0) {
      return p.inStart + Math.round(((pos - p.outStart) * p.inRate) / p.outRate);
    }
  }
  return null;
}

/** 元の位置を出力の位置にする（写せなければ null）。 */
export function fromSource(pos: number, map: readonly EditSourcePieceDto[] | null): number | null {
  if (!map || !Number.isFinite(pos)) return null;
  for (const p of map) {
    const len = inLength(p);
    if (pos >= p.inStart && pos < p.inStart + len && p.inRate > 0) {
      const out = p.outStart + Math.round(((pos - p.inStart) * p.outRate) / p.inRate);
      return Math.min(p.outEnd - 1, Math.max(p.outStart, out));
    }
  }
  return null;
}
