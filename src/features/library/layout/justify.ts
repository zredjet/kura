/**
 * ジャスティファイ配置（docs/architecture.md §12）。
 * 行の高さの目標・コンテナ幅・各アイテムの縦横比から行分割を決める純粋関数。
 * 縦横比は `items_query_ids` が ID と一緒に全件分返すので、詳細を取る前に全体の高さが決まる。
 */

export interface JustifyInput {
  /** 縦横比×1000（音楽と不明は 1000）。 */
  aspects: ArrayLike<number>;
  /** 日付のグループの開始位置（昇順。先頭は 0）。空なら全体で 1 グループ。 */
  groupStarts: readonly number[];
  /** タイルを並べる幅（px）。 */
  width: number;
  /** 行の高さの目標（px）。 */
  targetHeight: number;
  /** タイルの間隔（px）。 */
  gap: number;
  /** 見出しの行の高さ（px）。 */
  headingHeight: number;
}

/** 見出しの行。 */
export interface HeadingRow {
  kind: 'heading';
  group: number;
  /** グループの件数。 */
  count: number;
  y: number;
  height: number;
}

/** タイルの行（`start` 番目から `end` 番目の手前まで）。 */
export interface TileRow {
  kind: 'tiles';
  start: number;
  end: number;
  y: number;
  height: number;
  /** 行の幅いっぱいに広げたか（グループの最後の行は広げない）。 */
  justified: boolean;
}

export type LayoutRow = HeadingRow | TileRow;

export interface Layout {
  rows: LayoutRow[];
  totalHeight: number;
}

/** 縦横比の上限と下限（極端に細長いものが行を壊さないように）。 */
const MIN_RATIO = 0.25;
const MAX_RATIO = 4;

/** 縦横比×1000 を実際の比にする。 */
export function ratioOf(aspect: number): number {
  const ratio = aspect > 0 ? aspect / 1000 : 1;
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio));
}

/** 行を決める。行と行の間には `gap` をあける（見出しの下も同じ）。 */
export function justify(input: JustifyInput): Layout {
  const { aspects, width, targetHeight, gap, headingHeight } = input;
  const count = aspects.length;
  const starts = input.groupStarts.length > 0 ? input.groupStarts : [0];
  const rows: LayoutRow[] = [];
  let y = 0;
  if (count === 0 || width <= 0) {
    return { rows, totalHeight: 0 };
  }

  for (let g = 0; g < starts.length; g += 1) {
    const groupStart = starts[g] ?? 0;
    const groupEnd = starts[g + 1] ?? count;
    if (groupEnd <= groupStart) {
      continue;
    }
    rows.push({
      kind: 'heading',
      group: g,
      count: groupEnd - groupStart,
      y,
      height: headingHeight,
    });
    y += headingHeight + gap;

    let rowStart = groupStart;
    let ratioSum = 0;
    for (let i = groupStart; i < groupEnd; i += 1) {
      const ratio = ratioOf(aspects[i] ?? 1000);
      ratioSum += ratio;
      const n = i - rowStart + 1;
      if (ratioSum * targetHeight + gap * (n - 1) < width) {
        continue;
      }
      // 目標の高さで並べると幅を超えた。i を入れて縮めるか、i の手前で区切って広げるか、
      // 高さが目標に近い方を選ぶ
      const withItem = (width - gap * (n - 1)) / ratioSum;
      const withoutItem = n > 1 ? (width - gap * (n - 2)) / (ratioSum - ratio) : Infinity;
      const closer = (h: number) => Math.abs(Math.log(h / targetHeight));
      if (closer(withoutItem) < closer(withItem)) {
        rows.push({
          kind: 'tiles',
          start: rowStart,
          end: i,
          y,
          height: withoutItem,
          justified: true,
        });
        y += withoutItem + gap;
        rowStart = i;
        ratioSum = ratio;
        // i だけで幅を超えるほど横長なら、その 1 枚で 1 行にする
        if (ratio * targetHeight >= width) {
          const height = width / ratio;
          rows.push({ kind: 'tiles', start: i, end: i + 1, y, height, justified: true });
          y += height + gap;
          rowStart = i + 1;
          ratioSum = 0;
        }
      } else {
        rows.push({
          kind: 'tiles',
          start: rowStart,
          end: i + 1,
          y,
          height: withItem,
          justified: true,
        });
        y += withItem + gap;
        rowStart = i + 1;
        ratioSum = 0;
      }
    }
    if (rowStart < groupEnd) {
      // 最後の半端な行は目標の高さのまま左に寄せる
      rows.push({
        kind: 'tiles',
        start: rowStart,
        end: groupEnd,
        y,
        height: targetHeight,
        justified: false,
      });
      y += targetHeight + gap;
    }
  }
  return { rows, totalHeight: Math.max(0, y - gap) };
}

/** 行の中の各タイルの幅（端数は最後のタイルで吸収して、行の幅にぴったり合わせる）。 */
export function tileWidths(
  row: TileRow,
  aspects: ArrayLike<number>,
  width: number,
  gap: number,
): number[] {
  const widths: number[] = [];
  for (let i = row.start; i < row.end; i += 1) {
    widths.push(Math.round(ratioOf(aspects[i] ?? 1000) * row.height));
  }
  if (row.justified && widths.length > 0) {
    const used = widths.reduce((a, b) => a + b, 0) + gap * (widths.length - 1);
    const last = widths.length - 1;
    widths[last] = (widths[last] ?? 0) + (width - used);
  }
  return widths;
}

/** `index` 番目のアイテムがある行（二分探索）。 */
export function rowOfItem(layout: Layout, index: number): number {
  const rows = layout.rows;
  let lo = 0;
  let hi = rows.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const row = rows[mid];
    if (!row) {
      break;
    }
    if (row.kind === 'heading') {
      // 見出しの直後の行と比べる
      const next = rows[mid + 1];
      if (next && next.kind === 'tiles' && index < next.start) {
        hi = mid - 1;
      } else {
        lo = mid + 1;
      }
    } else if (index < row.start) {
      hi = mid - 1;
    } else if (index >= row.end) {
      lo = mid + 1;
    } else {
      return mid;
    }
  }
  return -1;
}
