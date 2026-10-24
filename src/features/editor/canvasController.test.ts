import { describe, expect, it, type Mock, vi } from 'vitest';

import type { EditViewDto, EditViewportDto } from '../../ipc/edit';
import { CanvasController, IDLE_MS, type Scene, type Scheduler } from './canvasController';
import type { TileBitmap } from './tileTable';
import { TileHttpError } from './tiles';

const SESSION = 'abc123';

function view(over: Partial<EditViewDto> = {}): EditViewDto {
  return {
    rev: 1,
    level: 0,
    fallback: null,
    fullWidth: 600,
    fullHeight: 300,
    width: 600,
    height: 300,
    frames: [{ index: 0, x: 0, y: 0, w: 600, h: 300, durationMs: null }],
    format: 'rgba8',
    tile: 256,
    changed: { type: 'all' },
    metaChanged: true,
    warnings: [],
    drawnNodes: 0,
    ms: 1,
    ...over,
  };
}

const WHOLE: EditViewportDto = { frame: 0, rect: null, scale: 1, interacting: false, focus: null };

/** 手で進める時計。 */
function fakeScheduler() {
  const frames: (() => void)[] = [];
  const timers = new Map<number, { cb: () => void; ms: number }>();
  let id = 0;
  const scheduler: Scheduler = {
    frame: (cb) => {
      frames.push(cb);
      return ++id;
    },
    cancelFrame: () => undefined,
    later: (cb, ms) => {
      timers.set(++id, { cb, ms });
      return id;
    },
    cancelLater: (t) => {
      timers.delete(t);
    },
  };
  return {
    scheduler,
    runFrames: () => frames.splice(0).forEach((cb) => cb()),
    runTimers: () => {
      const list = [...timers.values()];
      timers.clear();
      list.forEach((t) => t.cb());
    },
    timers,
  };
}

interface FakeBitmap extends TileBitmap {
  url: string;
  close: Mock<() => void>;
}

/** 読み込みを手で返す偽物。 */
function fakeLoader() {
  const pending = new Map<
    string,
    { resolve: (b: FakeBitmap) => void; reject: (e: unknown) => void; w: number; h: number }
  >();
  const urls: string[] = [];
  const load = vi.fn(
    (url: string, _format: string, w: number, h: number) =>
      new Promise<TileBitmap>((resolve, reject) => {
        urls.push(url);
        pending.set(url, { resolve, reject, w, h });
      }),
  );
  const resolve = async (url: string) => {
    const p = pending.get(url);
    if (!p) throw new Error(`読んでいない：${url}`);
    pending.delete(url);
    const b: FakeBitmap = { url, width: p.w, height: p.h, close: vi.fn<() => void>() };
    p.resolve(b);
    await new Promise((r) => setTimeout(r, 0));
    return b;
  };
  const reject = async (url: string, error: unknown) => {
    const p = pending.get(url);
    if (!p) throw new Error(`読んでいない：${url}`);
    pending.delete(url);
    p.reject(error);
    await new Promise((r) => setTimeout(r, 0));
  };
  return { load, urls, pending, resolve, reject };
}

const url = (rev: number, level: number, tx: number, ty: number, frame = 0) =>
  `kura://localhost/edit/${SESSION}/${rev}/${level}/${frame}/${tx}/${ty}`;

function setup(options: { pixelArt?: boolean; dpr?: number; w?: number; h?: number } = {}) {
  const clock = fakeScheduler();
  const loader = fakeLoader();
  const scenes: Scene[] = [];
  const onIdle = vi.fn();
  const onInfo = vi.fn();
  const c = new CanvasController({
    scheduler: clock.scheduler,
    load: loader.load,
    draw: (_s, scene) => scenes.push(scene),
    format: 'rgba',
    onIdle,
    onInfo,
  });
  c.setSession(SESSION);
  c.setImage({ w: options.w ?? 600, h: options.h ?? 300, pixelArt: options.pixelArt ?? false });
  c.resize(800, 600, options.dpr ?? 1);
  clock.runTimers();
  onIdle.mockClear();
  return { c, clock, loader, scenes, onIdle, onInfo };
}

describe('表示の結果とタイル', () => {
  it('最初の表示（all）では見えているタイルを今の rev で取りに行く（同時に 6 枚まで）', () => {
    const { c, loader } = setup();
    expect(c.zoom).toBe(1); // 600×300 は 800×600 に収まる（広げない）
    c.applyView(view(), 0, WHOLE);
    expect(loader.urls).toEqual([
      url(1, 0, 0, 0),
      url(1, 0, 1, 0),
      url(1, 0, 2, 0),
      url(1, 0, 0, 1),
      url(1, 0, 1, 1),
      url(1, 0, 2, 1),
    ]);
    // 右端・下端のタイルは小さい（RGBA8 の長さの確かめに使う）
    expect(loader.load).toHaveBeenCalledWith(url(1, 0, 2, 1), 'rgba', 88, 44);
  });

  it('変わったタイルだけを、その rev で取り直す（ほかは同じ URL・持っている絵のまま）', async () => {
    const { c, loader } = setup();
    c.applyView(view(), 0, WHOLE);
    for (const u of [...loader.urls]) await loader.resolve(u);
    loader.urls.length = 0;
    c.applyView(
      view({ rev: 2, changed: { type: 'tiles', frames: [{ frame: 0, tiles: [[1, 0]] }] } }),
      1,
      WHOLE,
    );
    expect(loader.urls).toEqual([url(2, 0, 1, 0)]);
  });

  it('要求した範囲の外のタイルは取りに行かない（有効でない。止まったら範囲を広げて頼み直す）', () => {
    const { c, loader } = setup();
    c.applyView(view(), 0, { ...WHOLE, rect: { x: 0, y: 0, w: 100, h: 100 } });
    expect(loader.urls).toEqual([url(1, 0, 0, 0)]);
  });

  it('古い rev の URL が 404 なら、今の表示の rev で取り直す（中身は同じ）', async () => {
    const { c, loader } = setup();
    c.applyView(view(), 0, { ...WHOLE, rect: { x: 0, y: 0, w: 100, h: 100 } });
    await loader.resolve(url(1, 0, 0, 0));
    // rev 5 の表示で、範囲を広げた：(1,0) は前の rev 1 のまま（報告に挙がらない）
    c.applyView(view({ rev: 5, changed: { type: 'tiles', frames: [] } }), 1, {
      ...WHOLE,
      rect: { x: 0, y: 0, w: 300, h: 100 },
    });
    expect(loader.urls).toContain(url(1, 0, 1, 0));
    await loader.reject(url(1, 0, 1, 0), new TileHttpError(404));
    expect(loader.urls).toContain(url(5, 0, 1, 0));
    await loader.resolve(url(5, 0, 1, 0));
    expect(c.tiles.get('0/0/1/0')?.rev).toBe(1);
  });

  it('描く中身：見えているタイルを画像の矩形の中に、拡大は最近傍で。市松模様は透明を持てる形式だけ', async () => {
    const { c, loader, clock, scenes } = setup();
    c.attach({ canvas: document.createElement('canvas'), checker: null });
    c.applyView(view(), 0, WHOLE);
    for (const u of [...loader.urls]) await loader.resolve(u);
    clock.runFrames();
    const scene = scenes[scenes.length - 1];
    expect(scene?.clip).toEqual({ x: 100, y: 150, w: 600, h: 300 });
    expect(scene?.tiles).toHaveLength(6);
    expect(scene?.tiles[0]).toMatchObject({ dx: 100, dy: 150, dw: 256, dh: 256, smoothing: false });
    expect(scene?.checker).toEqual({ x: 100, y: 150, w: 600, h: 300 });
    c.applyView(view({ rev: 2, format: 'rgb8', changed: { type: 'tiles', frames: [] } }), 1, WHOLE);
    expect(c.scene().checker).toBeNull();
  });

  it('見えているタイルが出そろったら一度だけ ready を知らせる', async () => {
    const { c, loader, onInfo } = setup();
    c.applyView(view(), 0, WHOLE);
    expect(c.scene().tiles).toHaveLength(0);
    for (const u of [...loader.urls]) await loader.resolve(u);
    c.scene();
    expect(onInfo).toHaveBeenLastCalledWith(expect.objectContaining({ ready: true }));
  });

  it('段が替わった直後は、前の段のタイルで足りない所を埋める（その段の古い絵は出さない）', async () => {
    const { c, loader } = setup({ w: 4000, h: 2000 });
    const big = (over: Partial<EditViewDto>) =>
      view({
        fullWidth: 4000,
        fullHeight: 2000,
        width: 1000,
        height: 500,
        level: 2,
        frames: [{ index: 0, x: 0, y: 0, w: 1000, h: 500, durationMs: null }],
        ...over,
      });
    c.applyView(big({}), 0, WHOLE);
    for (const u of [...loader.urls]) await loader.resolve(u);
    c.setZoom(1);
    c.applyView(
      big({
        level: 0,
        width: 4000,
        height: 2000,
        frames: [{ index: 0, x: 0, y: 0, w: 4000, h: 2000, durationMs: null }],
      }),
      1,
      WHOLE,
    );
    const scene = c.scene();
    expect(scene.tiles.length).toBeGreaterThan(0);
    expect(scene.tiles.every((t) => t.clip !== null)).toBe(true);
  });

  it('閉じたら絵をすべて close し、あとから届いた絵も close する', async () => {
    const { c, loader } = setup();
    c.applyView(view(), 0, WHOLE);
    const first = await loader.resolve(url(1, 0, 0, 0));
    c.dispose();
    expect(first.close).toHaveBeenCalledOnce();
    const late = await loader.resolve(url(1, 0, 1, 0));
    expect(late.close).toHaveBeenCalledOnce();
  });
});

describe('ズーム・パンと頼み直し', () => {
  it('止まって 100ms で、見えている範囲と倍率 × DPR を頼み直す', () => {
    const { c, clock, onIdle } = setup({ dpr: 2, w: 4000, h: 3000 });
    c.setZoom(1);
    c.panBy(-10, 0);
    expect(onIdle).not.toHaveBeenCalled();
    expect([...clock.timers.values()].map((t) => t.ms)).toEqual([IDLE_MS]);
    clock.runTimers();
    expect(onIdle).toHaveBeenCalledOnce();
    const vp = onIdle.mock.calls[0]?.[0] as EditViewportDto;
    expect(vp.scale).toBe(2);
    expect(vp.rect).not.toBeNull();
    expect(vp.interacting).toBe(false);
  });

  it('画像全体が入るなら範囲は null（全体）', () => {
    const { c } = setup();
    expect(c.viewport().rect).toBeNull();
  });

  it('ドット絵は装置の画素の整数倍に吸着し、位置も装置の画素にそろえる', () => {
    const { c } = setup({ pixelArt: true, dpr: 2, w: 64, h: 64 });
    expect((c.zoom * 2) % 1).toBe(0);
    c.zoomAt(1.13, 333.3, 222.2);
    expect((c.zoom * 2) % 1).toBe(0);
    expect((c.transform.ox * 2) % 1).toBe(0);
    c.actualSize();
    expect(c.zoom).toBe(1);
    expect(c.fitting).toBe(false);
    c.fit();
    expect(c.fitting).toBe(true);
  });

  it('フレームを替えたらすぐ頼み直す', () => {
    const { c, onIdle } = setup();
    c.setFrame(2);
    expect(onIdle).toHaveBeenCalledWith(expect.objectContaining({ frame: 2 }));
  });
});

describe('仮の筆', () => {
  const spec = {
    tool: 'pen' as const,
    color: { rgba: [255, 0, 0, 255] as [number, number, number, number], index: null },
    blend: 'over' as const,
    brush: { shape: 'square' as const, size: 1 },
    pixelPerfect: false,
    frame: 0,
    bounds: { x: 0, y: 0, w: 600, h: 300 },
    indexed: false,
  };

  it('描き終えたら動作を返し、その要求の表示のタイルを解き終えたら外す', async () => {
    const { c, loader } = setup();
    c.applyView(view(), 0, WHOLE);
    for (const u of [...loader.urls]) await loader.resolve(u);
    c.beginStroke(spec, [1, 1]);
    c.extendStroke([[3, 1]]);
    expect(c.drawing).toBe(true);
    expect(c.scene().overlays).toHaveLength(1);
    const done = c.endStroke();
    expect(done?.action).toMatchObject({ type: 'stroke', tool: 'pen' });
    expect(c.lineStart(0)).toEqual([3, 1]);
    if (!done) return;
    c.strokeSent(done.pending, 3, 0);
    c.applyView(
      view({ rev: 3, changed: { type: 'tiles', frames: [{ frame: 0, tiles: [[0, 0]] }] } }),
      3,
      WHOLE,
    );
    // タイルを解くまでは残す
    expect(c.scene().overlays).toHaveLength(1);
    await loader.resolve(url(3, 0, 0, 0));
    expect(c.scene().overlays).toHaveLength(0);
  });

  it('断られた・やめた筆はすぐ外す', () => {
    const { c } = setup();
    c.beginStroke(spec, [1, 1]);
    expect(c.cancelStroke()).toBe(true);
    expect(c.scene().overlays).toHaveLength(0);
    c.beginStroke(spec, [1, 1]);
    const done = c.endStroke();
    if (!done) throw new Error('筆がない');
    c.strokeSent(done.pending, 4, 0);
    c.onRejected(4, 0);
    expect(c.scene().overlays).toHaveLength(0);
  });

  it('描ける範囲と点の判定（フレームの列はそのフレームの矩形）', () => {
    const { c } = setup();
    expect(c.contains([0, 0])).toBe(true);
    expect(c.contains([600, 0])).toBe(false);
    c.applyView(
      view({
        frames: [
          { index: 0, x: 0, y: 0, w: 32, h: 32, durationMs: 100 },
          { index: 1, x: 10, y: 10, w: 8, h: 8, durationMs: 100 },
        ],
      }),
      0,
      WHOLE,
    );
    c.setFrame(1);
    expect(c.frameBounds()).toEqual({ x: 10, y: 10, w: 8, h: 8 });
    expect(c.contains([5, 5])).toBe(false);
  });
});

describe('自己点検に渡すタイル', () => {
  it('今の表示の有効なタイルを、見えているものから', () => {
    const { c } = setup();
    expect(c.probeTiles()).toBeNull();
    c.applyView(view({ rev: 4 }), 0, WHOLE);
    const probe = c.probeTiles(2);
    expect(probe).toMatchObject({ rev: 4, level: 0, frame: 0 });
    expect(probe?.tiles).toEqual([
      { tx: 0, ty: 0, w: 256, h: 256 },
      { tx: 1, ty: 0, w: 256, h: 256 },
    ]);
  });
});
