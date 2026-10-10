/** 「前へ」で頭に戻すか前のスロットへ行くかの境目（docs/playlists.md §6.3）。 */
export const RESTART_THRESHOLD_MS = 3000;
/** 画像の表示時間の範囲（1〜60 秒）。 */
export const IMAGE_MS_MIN = 1000;
export const IMAGE_MS_MAX = 60_000;

/** 「前へ」の行き先：再生位置が 3 秒以上なら今のスロットの頭、未満なら前のスロット（先頭なら頭）。 */
export function prevTarget(index: number, positionMs: number): { index: number; restart: boolean } {
  if (positionMs >= RESTART_THRESHOLD_MS || index <= 0) {
    return { index, restart: true };
  }
  return { index: index - 1, restart: false };
}

/** 次のスロット。終わりなら、リピートで先頭へ、でなければ null。 */
export function nextIndex(index: number, length: number, repeat: boolean): number | null {
  if (index + 1 < length) {
    return index + 1;
  }
  return repeat && length > 0 ? 0 : null;
}

/** 画像の表示時間を `deltaMs` だけ変える（1〜60 秒に収める）。 */
export function stepImageMs(currentMs: number, deltaMs: number): number {
  return Math.min(IMAGE_MS_MAX, Math.max(IMAGE_MS_MIN, currentMs + deltaMs));
}

/** シークで飛ばした分を数えないための、再生時間の増分（0〜1.5 秒のときだけ数える）。 */
export function playedDelta(previousSec: number, currentSec: number): number {
  const delta = currentSec - previousSec;
  return delta > 0 && delta <= 1.5 ? delta * 1000 : 0;
}
