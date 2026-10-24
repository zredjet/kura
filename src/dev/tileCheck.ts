/**
 * エディタのタイルの形式の自己点検（`VITE_KURA_CHECK=1` で起動・ビルドしたときだけ読み込む。通常のビルドには入らない。
 * docs/editing.md §12.13「どちらにするかは段 5 で WebKit で測って決める」）。
 *
 * エディタを開いて最初の表示が届いたら（少し待って面の大きさが決まってから）、その表示の有効なタイル（見えているものを
 * 先に 24 枚まで）を、非圧縮の RGBA8（拡張子なし）と PNG（`.png`）の両方で読んで解く時間を測る。窓が隠れていても
 * 測れるよう、`fetch`（本体を読み終えるまで）と、解く時間（RGBA8 は `ImageData` ＋ `createImageBitmap`、PNG は
 * `createImageBitmap(blob)`）だけを数える（描画・rAF は使わない）。WebKit のキャッシュに当たらないよう `no-store` で読む。
 *
 * 1 回の測りは、1 枚ずつ順に読む（1 枚の時間の分布）と、全部を同時に読む（壁時計の時間）の 2 つ。形式の順を入れ替え
 * ながら 3 回測り、`bench_report('edit-tiles', …)` と console.warn に出す。
 */

import { commands } from '../ipc/bindings';
import { editTileUrl, type EditTileFormat } from '../ipc/media';
import type { CanvasController } from '../features/editor/canvasController';

/** 測るタイル。 */
export interface TileProbe {
  tx: number;
  ty: number;
  w: number;
  h: number;
}

/** 測る口（テストで差し替える）。 */
export interface CheckIo {
  now(): number;
  /** 読む（本体を読み終えるまで）。RGBA8 は ArrayBuffer、PNG は Blob。 */
  read(url: string, format: EditTileFormat): Promise<ArrayBuffer | Blob>;
  /** 解く（解いた絵は閉じる）。 */
  decode(body: ArrayBuffer | Blob, format: EditTileFormat, w: number, h: number): Promise<void>;
}

/** 分布の要約（ミリ秒）。 */
export interface Stats {
  p50: number;
  p95: number;
  max: number;
  mean: number;
}

/** 形式ごとの結果。 */
export interface FormatReport {
  format: EditTileFormat;
  tiles: number;
  /** 1 回分の本体のバイト数の合計。 */
  bytes: number;
  /** 1 枚ずつ読んだときの、読む時間・解く時間・その和（全回の全タイル）。 */
  fetch: Stats;
  decode: Stats;
  total: Stats;
  /** 全部を同時に読んで解いた壁時計の時間（回ごと）。 */
  parallelMs: number[];
  errors: number;
}

export interface TileCheckReport {
  session: string;
  rev: number;
  level: number;
  frame: number;
  dpr: number;
  rounds: number;
  rgba: FormatReport;
  png: FormatReport;
  /** 1 枚の和の中央値と、同時に読んだ壁時計の中央値で速かった形式。 */
  fasterPerTile: EditTileFormat;
  fasterParallel: EditTileFormat;
  ms: number;
}

/** 分布の要約（空なら 0）。 */
export function stats(values: readonly number[]): Stats {
  if (values.length === 0) return { p50: 0, p95: 0, max: 0, mean: 0 };
  const s = [...values].sort((a, b) => a - b);
  const at = (q: number) =>
    s[Math.min(s.length - 1, Math.max(0, Math.ceil(q * s.length) - 1))] ?? 0;
  const round = (v: number) => Math.round(v * 100) / 100;
  return {
    p50: round(at(0.5)),
    p95: round(at(0.95)),
    max: round(s[s.length - 1] ?? 0),
    mean: round(s.reduce((a, b) => a + b, 0) / s.length),
  };
}

function median(values: readonly number[]): number {
  return stats(values).p50;
}

const realIo: CheckIo = {
  now: () => performance.now(),
  read: async (url, format) => {
    const res = await fetch(url, { cache: 'no-store' });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return format === 'png' ? res.blob() : res.arrayBuffer();
  },
  decode: async (body, format, w, h) => {
    const bitmap =
      format === 'png' || body instanceof Blob
        ? await createImageBitmap(body as Blob)
        : await createImageBitmap(new ImageData(new Uint8ClampedArray(body), w, h));
    bitmap.close();
  },
};

function sizeOf(body: ArrayBuffer | Blob): number {
  return body instanceof Blob ? body.size : body.byteLength;
}

/** 1 つの形式を 1 回測る（1 枚ずつと、同時に）。 */
async function measureOnce(
  urlOf: (p: TileProbe, format: EditTileFormat) => string,
  probes: readonly TileProbe[],
  format: EditTileFormat,
  io: CheckIo,
  acc: {
    fetch: number[];
    decode: number[];
    total: number[];
    parallel: number[];
    bytes: number;
    errors: number;
  },
): Promise<void> {
  let bytes = 0;
  for (const p of probes) {
    try {
      const t0 = io.now();
      const body = await io.read(urlOf(p, format), format);
      const t1 = io.now();
      await io.decode(body, format, p.w, p.h);
      const t2 = io.now();
      acc.fetch.push(t1 - t0);
      acc.decode.push(t2 - t1);
      acc.total.push(t2 - t0);
      bytes += sizeOf(body);
    } catch {
      acc.errors++;
    }
  }
  acc.bytes = bytes;
  const start = io.now();
  const results = await Promise.allSettled(
    probes.map(async (p) => io.decode(await io.read(urlOf(p, format), format), format, p.w, p.h)),
  );
  acc.parallel.push(Math.round((io.now() - start) * 100) / 100);
  acc.errors += results.filter((r) => r.status === 'rejected').length;
}

/** 両方の形式を、順を入れ替えながら `rounds` 回測る。 */
export async function runTileCheck(
  input: {
    session: string;
    rev: number;
    level: number;
    frame: number;
    dpr: number;
    tiles: TileProbe[];
  },
  io: CheckIo = realIo,
  rounds = 3,
): Promise<TileCheckReport> {
  const started = io.now();
  const urlOf = (p: TileProbe, format: EditTileFormat) =>
    editTileUrl(input.session, input.rev, input.level, input.frame, p.tx, p.ty, format);
  const fresh = () => ({ fetch: [], decode: [], total: [], parallel: [], bytes: 0, errors: 0 });
  const acc: Record<EditTileFormat, ReturnType<typeof fresh>> = { rgba: fresh(), png: fresh() };
  for (let r = 0; r < rounds; r++) {
    const order: EditTileFormat[] = r % 2 === 0 ? ['rgba', 'png'] : ['png', 'rgba'];
    for (const format of order) {
      await measureOnce(urlOf, input.tiles, format, io, acc[format]);
    }
  }
  const report = (format: EditTileFormat): FormatReport => ({
    format,
    tiles: input.tiles.length,
    bytes: acc[format].bytes,
    fetch: stats(acc[format].fetch),
    decode: stats(acc[format].decode),
    total: stats(acc[format].total),
    parallelMs: acc[format].parallel,
    errors: acc[format].errors,
  });
  const rgba = report('rgba');
  const png = report('png');
  return {
    session: input.session,
    rev: input.rev,
    level: input.level,
    frame: input.frame,
    dpr: input.dpr,
    rounds,
    rgba,
    png,
    fasterPerTile: rgba.total.p50 <= png.total.p50 ? 'rgba' : 'png',
    fasterParallel: median(rgba.parallelMs) <= median(png.parallelMs) ? 'rgba' : 'png',
    ms: Math.round(io.now() - started),
  };
}

/** 面の大きさが決まり、最初の頼み直しが届くまで待つ時間（ms）。 */
const SETTLE_MS = 1500;

/** 今の表示で 1 回測る（結果を出す）。 */
function measureNow(controller: CanvasController, session: string): Promise<void> {
  const probe = controller.probeTiles();
  if (!probe || probe.tiles.length === 0) {
    send({ session, error: 'タイルがありません' });
    return Promise.resolve();
  }
  const dpr = window.devicePixelRatio || 1;
  return runTileCheck({ session, dpr, ...probe }).then(send, (error: unknown) =>
    send({ session, error: error instanceof Error ? error.message : String(error) }),
  );
}

/**
 * エディタの最初の表示のあとに呼ぶ（待たない）。結果は console.warn と Rust のログ（bench_report）に出す。
 * ズームを変えてから測り直すときは、devtools のコンソールで `__kuraTileCheck()` を呼ぶ（今の表示の段で測る）。
 */
export function checkTiles(controller: CanvasController, session: string): void {
  (window as unknown as { __kuraTileCheck?: () => Promise<void> }).__kuraTileCheck = () =>
    measureNow(controller, session);
  setTimeout(() => void measureNow(controller, session), SETTLE_MS);
}

function send(value: unknown): void {
  const json = JSON.stringify(value);
  console.warn(`[check] edit-tiles ${json}`);
  void commands.benchReport('edit-tiles', json).catch(() => undefined);
}
