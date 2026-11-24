/**
 * シークの計算（ADR-230。シークバー・波形・キーで共有する。純粋な関数）。
 *
 * - シークは今のアイテムの中だけ：0 と終わりの `END_GUARD_MS` 手前で止める（終わりちょうどへ飛ぶと `ended` が来て
 *   次のアイテムへ進むため）。
 * - ドラッグは「錨」から測る：押した点（x と時刻）を錨にし、動いた px × 1px あたりの時刻 × 速さを足す。⌥ を押して
 *   いる間は速さ 1/10。⌥ を押し離ししたら、その時点の位置で錨を置き直す（位置が飛ばない）。
 * - コマ送りはコマの真ん中を狙う（境目の小数の誤差と、先頭が 0 から少しずれた動画に強い）。
 */

/** シークを終わりのどれだけ手前で止めるか（ms）。 */
export const END_GUARD_MS = 100;
/** ⌥ を押しながらのドラッグの速さ（ポインタの動きの 1/10）。 */
export const FINE_SCRUB_RATE = 0.1;
/** コマの長さが分からないとき・動画でないときの細かい送り（ms）。 */
export const FINE_STEP_MS = 100;

/**
 * シークの先を `[0, 長さ − guardMs]` に収める（長さが分からなければ 0 以上だけ）。`guardMs` は既定で
 * `END_GUARD_MS`（呼ぶ側が先に収めたもの・コマ送りの先は 0 で渡す）。
 */
export function clampSeek(ms: number, durationMs: number | null, guardMs = END_GUARD_MS): number {
  const lo = Number.isFinite(ms) ? Math.max(0, ms) : 0;
  if (durationMs === null || !(durationMs > 0)) {
    return lo;
  }
  return Math.min(lo, Math.max(0, durationMs - guardMs));
}

/** バーの中の x（px）を時刻（ms）にする（外側は端に寄せる。幅・長さがなければ 0）。 */
export function msAtX(x: number, width: number, durationMs: number): number {
  if (!(width > 0) || !(durationMs > 0) || !Number.isFinite(x)) {
    return 0;
  }
  return (Math.min(width, Math.max(0, x)) / width) * durationMs;
}

/** ドラッグの錨：ポインタがこの x のとき、この時刻。`fine` は ⌥ を押しているか。 */
export interface ScrubAnchor {
  x: number;
  ms: number;
  fine: boolean;
}

/** 錨から x のときの時刻（`[0, 長さ]` に収める）。 */
function valueAt(anchor: ScrubAnchor, x: number, width: number, durationMs: number): number {
  if (!(width > 0) || !(durationMs > 0)) {
    return 0;
  }
  const rate = anchor.fine ? FINE_SCRUB_RATE : 1;
  const ms = anchor.ms + (x - anchor.x) * (durationMs / width) * rate;
  return Math.min(durationMs, Math.max(0, ms));
}

/**
 * 押したとき：⌥ なしならポインタの下の時刻から、⌥ ありなら今の位置のまま始める（そこから 1/10 の速さで動く）。
 */
export function startScrub(
  x: number,
  width: number,
  durationMs: number,
  currentMs: number,
  fine: boolean,
): ScrubAnchor {
  const ms = fine
    ? Math.min(Math.max(0, durationMs), Math.max(0, currentMs))
    : msAtX(x, width, durationMs);
  return { x, ms, fine };
}

/**
 * 動かしたとき：錨からの時刻。⌥ の押し離しがあれば、その時点の x で前の速さのまま測った時刻を新しい錨にする。
 */
export function moveScrub(
  anchor: ScrubAnchor,
  x: number,
  width: number,
  durationMs: number,
  fine: boolean,
): { anchor: ScrubAnchor; ms: number } {
  const ms = valueAt(anchor, x, width, durationMs);
  if (fine === anchor.fine) {
    return { anchor, ms };
  }
  return { anchor: { x, ms, fine }, ms };
}

/** 吹き出しの真ん中の x（左右の端からはみ出さないように寄せる。幅が足りなければバーの真ん中）。 */
export function tooltipLeft(x: number, width: number, tipWidth: number): number {
  if (!(width > tipWidth)) {
    return width / 2;
  }
  const half = tipWidth / 2;
  return Math.min(width - half, Math.max(half, x));
}

/**
 * 1 コマ送った先（ms）。今のコマの番号に `dir` を足し、そのコマの真ん中へ。0 より前・最後のコマより後へは行かない
 * （終わりが分かれば、終わりから半コマ手前まで）。
 */
export function frameStepTarget(
  positionMs: number,
  frameMs: number,
  dir: 1 | -1,
  endMs: number | null,
): number {
  if (!(frameMs > 0)) {
    return clampSeek(positionMs + dir * FINE_STEP_MS, endMs);
  }
  const index = Math.floor(Math.max(0, positionMs) / frameMs + 1e-6);
  const target = (Math.max(0, index + dir) + 0.5) * frameMs;
  if (endMs === null || !(endMs > 0)) {
    return target;
  }
  return Math.max(0, Math.min(target, endMs - frameMs / 2));
}
