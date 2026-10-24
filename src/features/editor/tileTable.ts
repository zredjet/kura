/**
 * エディタのタイルの表（React の外。docs/editing.md §12.13）。
 *
 * - **rev の表**：各タイルを「最後に変わった rev」で指す。結果の `changed` が `all` なら、その段のタイルはすべてその rev、
 *   `tiles` なら挙がったタイルだけをその rev にする（変わらないタイルは同じ URL のまま。undo は前の rev を返すので、
 *   前に取ったものがそのまま当たる）。
 * - **解いた絵**（ImageBitmap）を LRU で持つ。バイトの上限（既定 64 MiB）を超えたら、見えていないものから古い順に
 *   `close` して捨てる。閉じたらすべて `close` する。
 * - 読み込みの状態（読み込み中・失敗した rev）も持つ（同じものを二重に取りに行かない・404 を繰り返さない）。
 */

/** 解いたタイル（ImageBitmap など。`close` で手放す）。 */
export interface TileBitmap {
  readonly width: number;
  readonly height: number;
  close(): void;
}

/** フロントが持つタイルのバイトの上限。 */
export const TILE_BUDGET = 64 * 1024 * 1024;

/** タイルの鍵（段・フレーム・位置）。 */
export function tileKey(level: number, frame: number, tx: number, ty: number): string {
  return `${level}/${frame}/${tx}/${ty}`;
}

interface Entry<B extends TileBitmap> {
  bitmap: B | null;
  bitmapRev: number | null;
  bytes: number;
  loadingRev: number | null;
  failedRev: number | null;
  used: number;
}

export class TileTable<B extends TileBitmap = TileBitmap> {
  /** 段ごとの「すべて」の rev（`changed: all`）。 */
  private readonly base = new Map<number, number>();
  /** そのあとに変わったタイルの rev。 */
  private readonly changed = new Map<string, number>();
  private readonly entries = new Map<string, Entry<B>>();
  private clock = 0;
  private total = 0;

  constructor(readonly budget = TILE_BUDGET) {}

  /** 持っている絵のバイト数。 */
  get bytes(): number {
    return this.total;
  }

  /** 持っている絵の数。 */
  get count(): number {
    let n = 0;
    for (const e of this.entries.values()) if (e.bitmap) n++;
    return n;
  }

  /** 段 `level` のタイルがすべて rev `rev` で変わった（最初の表示・段や形が変わった）。 */
  markAll(level: number, rev: number): void {
    const prefix = `${level}/`;
    for (const key of [...this.changed.keys()]) {
      if (key.startsWith(prefix)) this.changed.delete(key);
    }
    this.base.set(level, rev);
  }

  /** 挙がったタイルが rev `rev` で変わった。 */
  markTiles(
    level: number,
    frame: number,
    tiles: readonly (readonly [number, number])[],
    rev: number,
  ): void {
    for (const [tx, ty] of tiles) this.changed.set(tileKey(level, frame, tx, ty), rev);
  }

  /** タイルが最後に変わった rev（まだ知らなければ null）。 */
  rev(level: number, frame: number, tx: number, ty: number): number | null {
    return this.changed.get(tileKey(level, frame, tx, ty)) ?? this.base.get(level) ?? null;
  }

  /** 持っている絵（古い rev のものも返す。使った順を進める）。 */
  get(key: string): { bitmap: B; rev: number } | null {
    const e = this.entries.get(key);
    if (!e?.bitmap || e.bitmapRev === null) return null;
    e.used = ++this.clock;
    return { bitmap: e.bitmap, rev: e.bitmapRev };
  }

  /** rev `rev` の絵を取りに行くべきか（もう持っている・読み込み中・その rev で失敗したなら偽）。 */
  needsLoad(key: string, rev: number): boolean {
    const e = this.entries.get(key);
    return !e || (e.bitmapRev !== rev && e.loadingRev !== rev && e.failedRev !== rev);
  }

  /** 読み込み中か。 */
  isLoading(key: string): boolean {
    return (this.entries.get(key)?.loadingRev ?? null) !== null;
  }

  /** 読み込みを始めた。 */
  startLoad(key: string, rev: number): void {
    const e = this.entry(key);
    e.loadingRev = rev;
  }

  /** 読めた（前の絵は閉じる）。 */
  finishLoad(key: string, rev: number, bitmap: B): void {
    const e = this.entry(key);
    if (e.bitmap && e.bitmap !== bitmap) {
      e.bitmap.close();
      this.total -= e.bytes;
    }
    e.bitmap = bitmap;
    e.bitmapRev = rev;
    e.bytes = bitmap.width * bitmap.height * 4;
    this.total += e.bytes;
    e.used = ++this.clock;
    if (e.loadingRev === rev) e.loadingRev = null;
    if (e.failedRev === rev) e.failedRev = null;
  }

  /** 読めなかった（その rev では取り直さない。次の表示の結果で `clearFailures`）。 */
  failLoad(key: string, rev: number): void {
    const e = this.entry(key);
    if (e.loadingRev === rev) e.loadingRev = null;
    e.failedRev = rev;
  }

  /** 失敗の印を消す（新しい表示が来たら取り直してよい）。 */
  clearFailures(): void {
    for (const e of this.entries.values()) e.failedRev = null;
  }

  /**
   * 段 `level` の絵のうち rev `rev` でないものを捨てる（段を替えて戻ってきたとき、ずっと前の中身を出さずに前の段の
   * タイルで埋めるため）。読み込み中の印は残す。
   */
  dropStale(level: number, rev: number): void {
    const prefix = `${level}/`;
    for (const [key, e] of this.entries) {
      if (!key.startsWith(prefix) || !e.bitmap || e.bitmapRev === rev) continue;
      e.bitmap.close();
      this.total -= e.bytes;
      e.bitmap = null;
      e.bitmapRev = null;
      e.bytes = 0;
    }
  }

  /** 上限を超えていれば、`keep`（見えているタイル）でないものから古い順に捨てる。捨てた鍵を返す。 */
  evict(keep: ReadonlySet<string>): string[] {
    const dropped: string[] = [];
    if (this.total <= this.budget) return dropped;
    const candidates = [...this.entries.entries()]
      .filter(([key, e]) => e.bitmap && !keep.has(key))
      .sort((a, b) => a[1].used - b[1].used);
    for (const [key, e] of candidates) {
      if (this.total <= this.budget) break;
      e.bitmap?.close();
      this.total -= e.bytes;
      dropped.push(key);
      if (e.loadingRev === null) {
        this.entries.delete(key);
      } else {
        e.bitmap = null;
        e.bitmapRev = null;
        e.bytes = 0;
      }
    }
    return dropped;
  }

  /** すべて捨てる（絵は close）。 */
  clear(): void {
    for (const e of this.entries.values()) e.bitmap?.close();
    this.entries.clear();
    this.changed.clear();
    this.base.clear();
    this.total = 0;
  }

  private entry(key: string): Entry<B> {
    let e = this.entries.get(key);
    if (!e) {
      e = { bitmap: null, bitmapRev: null, bytes: 0, loadingRev: null, failedRev: null, used: 0 };
      this.entries.set(key, e);
    }
    return e;
  }
}
