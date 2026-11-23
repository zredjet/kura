/**
 * 動画の 1 コマの長さの推定（ADR-230。DB にコマの速さを持たないので、再生中に見えたコマの時刻の差から決める。
 * 純粋な関数）。
 *
 * - 差が 5 個に満たなければ分からない（null）。
 * - 差の中央値（たまに落ちたコマ・二重のコマに強い）を、よくある速さに ±1.5% で寄せる（いちばん近いもの）。
 * - 1/240 秒より短い・1 秒より長い値は捨てる。
 */

/** 分からないときの 1 コマ（30 fps）。 */
export const DEFAULT_FRAME_MS = 1000 / 30;

/** 推定に要る差の数。 */
export const MIN_FRAME_SAMPLES = 5;

/** よくあるコマの速さ（fps）。 */
const COMMON_FPS = [24000 / 1001, 24, 25, 30000 / 1001, 30, 48, 50, 60000 / 1001, 60, 120];
/** 寄せる幅（比）。 */
const SNAP_TOLERANCE = 0.015;
const MIN_FRAME_MS = 1000 / 240;
const MAX_FRAME_MS = 1000;

/** コマの時刻の差（ms）の並びから 1 コマの長さを推定する（分からなければ null）。 */
export function estimateFrameMs(deltasMs: readonly number[]): number | null {
  const valid = deltasMs
    .filter((d) => Number.isFinite(d) && d >= MIN_FRAME_MS * 0.98 && d <= MAX_FRAME_MS)
    .sort((a, b) => a - b);
  if (valid.length < MIN_FRAME_SAMPLES) {
    return null;
  }
  const mid = valid.length >> 1;
  const median =
    valid.length % 2 === 1 ? (valid[mid] ?? 0) : ((valid[mid - 1] ?? 0) + (valid[mid] ?? 0)) / 2;
  let best: number | null = null;
  let bestErr = Number.POSITIVE_INFINITY;
  for (const fps of COMMON_FPS) {
    const frame = 1000 / fps;
    const err = Math.abs(median - frame) / frame;
    if (err <= SNAP_TOLERANCE && err < bestErr) {
      best = frame;
      bestErr = err;
    }
  }
  return best ?? median;
}
