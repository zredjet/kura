/** ユニットの中の並べ替え（純粋関数）。選んだものはまとめて動かし、互いの順は変えない。 */

/** 選んだものを 1 つ前（-1）か後ろ（+1）へずらす（⌥⌘← →）。端にあるものはそのまま。 */
export function moveBlock(
  order: readonly number[],
  moving: ReadonlySet<number>,
  delta: -1 | 1,
): number[] {
  const out = [...order];
  if (delta < 0) {
    for (let i = 1; i < out.length; i += 1) {
      const current = out[i] as number;
      const previous = out[i - 1] as number;
      if (moving.has(current) && !moving.has(previous)) {
        out[i - 1] = current;
        out[i] = previous;
      }
    }
  } else {
    for (let i = out.length - 2; i >= 0; i -= 1) {
      const current = out[i] as number;
      const next = out[i + 1] as number;
      if (moving.has(current) && !moving.has(next)) {
        out[i + 1] = current;
        out[i] = next;
      }
    }
  }
  return out;
}

/** 選んだものを `before` の前（null なら最後）へまとめて移す（ドラッグ）。 */
export function insertBefore(
  order: readonly number[],
  moving: ReadonlySet<number>,
  before: number | null,
): number[] {
  if (before !== null && moving.has(before)) {
    return [...order];
  }
  const kept = order.filter((id) => !moving.has(id));
  const block = order.filter((id) => moving.has(id));
  const at = before === null ? -1 : kept.indexOf(before);
  if (at < 0) {
    return [...kept, ...block];
  }
  return [...kept.slice(0, at), ...block, ...kept.slice(at)];
}
