import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ItemSummaryDto } from '../../../ipc/items';
import type { ItemKind } from '../../../ipc/kinds';
import { bgmOnEnter } from './bgm';
import { FADE_MS } from './fade';
import { prevTarget, stepImageMs } from './nav';
import { PlaybackController, POLL_MS, TICK_MS } from './PlaybackController';
import type { MediaPort, PlayerSnapshot, Queue, SlotReport } from './types';

/** 再生要素の偽物。play / pause / 再生位置 / イベントだけ真似る。 */
class FakeMedia {
  src = '';
  currentTime = 0;
  duration = Number.NaN;
  paused = true;
  ended = false;
  volume = 1;
  preload = '';
  private readonly handlers = new Map<string, Set<() => void>>();
  play = vi.fn(() => {
    this.paused = false;
    return Promise.resolve();
  });
  pause = vi.fn(() => {
    this.paused = true;
  });
  load = vi.fn();
  removeAttribute(name: string) {
    if (name === 'src') {
      this.src = '';
    }
  }
  addEventListener(type: string, fn: () => void) {
    const set = this.handlers.get(type) ?? new Set();
    set.add(fn);
    this.handlers.set(type, set);
  }
  removeEventListener(type: string, fn: () => void) {
    this.handlers.get(type)?.delete(fn);
  }
  fire(type: string) {
    this.handlers.get(type)?.forEach((fn) => fn());
  }
}

const EXT: Record<ItemKind, string> = {
  image: 'jpg',
  video: 'mkv',
  audio: 'ogg',
  font: 'ttf',
  model: 'glb',
};

function item(id: number, kind: ItemKind, extra: Partial<ItemSummaryDto> = {}): ItemSummaryDto {
  const media = kind === 'video' || kind === 'audio';
  return {
    id,
    hash: `h${id}`,
    ext: EXT[kind],
    kind,
    width: null,
    height: null,
    durationMs: media ? 180_000 : null,
    sizeBytes: 1,
    title: `t${id}`,
    rating: 0,
    sourceSite: null,
    importedAt: 0,
    capturedAt: null,
    thumbState: 'ready',
    proxyState: media ? 'ready' : 'not_needed',
    lastPlayedAt: null,
    ...extra,
  };
}

function queueOf(ids: number[], imageMs: (number | null)[] = []): Queue {
  return {
    itemIds: ids,
    unitIds: ids.map(() => null),
    indexInUnit: ids.map(() => 0),
    unitLen: ids.map(() => 1),
    imageDurationMs: ids.map((_, i) => imageMs[i] ?? null),
    startMs: ids.map(() => null),
    endMs: ids.map(() => null),
    groupIndex: [],
    groups: [],
    totalMs: 0,
    seed: '0000000000000001',
    granularity: 'file',
    startIndex: 0,
    startPositionMs: 0,
    playlist: null,
  };
}

/** ユニット単位のキュー（`sizes` 件ずつのまとまり）。 */
function unitQueue(ids: number[], sizes: number[]): Queue {
  const queue = queueOf(
    ids,
    ids.map(() => 1000),
  );
  let start = 0;
  queue.granularity = 'unit';
  sizes.forEach((len, g) => {
    queue.groups.push({
      entryId: g + 1,
      target: { kind: 'unit', id: g + 1 },
      name: `u${g + 1}`,
      start,
      len,
    });
    for (let i = 0; i < len; i++) {
      queue.groupIndex.push(g);
      queue.indexInUnit[start + i] = i;
      queue.unitLen[start + i] = len;
      queue.unitIds[start + i] = g + 1;
    }
    start += len;
  });
  return queue;
}

interface Harness {
  controller: PlaybackController;
  video: FakeMedia;
  /** BGM 用の 2 つ（作られた順）。 */
  tracks: FakeMedia[];
  /** 作られた要素すべて（BGM の 2 つと先読みの 2 つ）。 */
  created: FakeMedia[];
  reports: SlotReport[];
  requests: { ids: number[]; force: boolean }[];
  items: Map<number, ItemSummaryDto>;
  last: () => PlayerSnapshot;
}

function setup(rows: ItemSummaryDto[]): Harness {
  const items = new Map(rows.map((r) => [r.id, r]));
  const created: FakeMedia[] = [];
  const reports: SlotReport[] = [];
  const requests: { ids: number[]; force: boolean }[] = [];
  const controller = new PlaybackController({
    clock: {
      now: () => Date.now(),
      setTimeout: (fn, ms) => setTimeout(fn, ms) as unknown as number,
      clearTimeout: (id) => clearTimeout(id),
    },
    createMedia: () => {
      const media = new FakeMedia();
      created.push(media);
      return media as unknown as MediaPort;
    },
    fetchItems: (ids) =>
      Promise.resolve(ids.map((id) => items.get(id)).filter((r) => r !== undefined)),
    report: (r) => {
      reports.push(r);
      return Promise.resolve(true);
    },
    requestProxies: (ids, force) => {
      requests.push({ ids, force });
      return Promise.resolve(null);
    },
    playUrl: (hash, attempt) =>
      `kura://localhost/play/${hash}${attempt > 0 ? `?a=${attempt}` : ''}`,
    preloadImage: () => undefined,
  });
  const video = new FakeMedia();
  controller.attachVideo(video as unknown as MediaPort);
  controller.setVolume(1);
  controller.setPrefs({ throughImages: true, loop: false, imageMs: 8000 });
  let last: PlayerSnapshot = controller.snapshot();
  controller.subscribe((s) => {
    last = s;
  });
  return {
    controller,
    video,
    tracks: created.slice(0, 2),
    created,
    reports,
    requests,
    items,
    last: () => last,
  };
}

/** タイマーと Promise を進める。 */
async function advance(ms: number) {
  await vi.advanceTimersByTimeAsync(ms);
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('BGM（docs/playlists.md §6.2）', () => {
  it('動画のスロットに入ると 300ms でフェードアウトして一時停止し、次の画像で戻る', async () => {
    const h = setup([item(1, 'audio'), item(2, 'video'), item(3, 'image')]);
    h.controller.start(queueOf([1, 2, 3]));
    await advance(0);
    const [, music] = h.tracks;
    expect(music?.src).toBe('kura://localhost/play/h1');
    expect(music?.paused).toBe(false);
    expect(music?.volume).toBe(1);

    h.controller.next();
    await advance(0);
    expect(h.video.src).toBe('kura://localhost/play/h2');
    await advance(FADE_MS / 2);
    expect(music?.volume).toBeGreaterThan(0.3);
    expect(music?.volume).toBeLessThan(0.7);
    expect(music?.paused).toBe(false);
    await advance(FADE_MS / 2 + 20);
    expect(music?.volume).toBe(0);
    expect(music?.paused).toBe(true);

    // 動画が終わって画像へ：一時停止していた曲を 300ms で戻す
    h.video.fire('ended');
    await advance(0);
    expect(music?.paused).toBe(false);
    await advance(FADE_MS + 20);
    expect(music?.volume).toBe(1);
    expect(h.last().bgm?.itemId).toBe(1);
  });

  it('次の音楽のスロットでは 300ms でクロスフェードする', async () => {
    const h = setup([item(1, 'audio'), item(2, 'audio')]);
    h.controller.start(queueOf([1, 2]));
    await advance(0);
    const [second, first] = h.tracks;
    h.controller.next();
    await advance(0);
    expect(second?.src).toBe('kura://localhost/play/h2');
    expect(second?.paused).toBe(false);
    await advance(FADE_MS / 2);
    // 途中では両方が鳴っていて、合わせてほぼ 1
    expect((first?.volume ?? 0) + (second?.volume ?? 0)).toBeCloseTo(1, 1);
    expect(first?.paused).toBe(false);
    await advance(FADE_MS / 2 + 20);
    expect(first?.volume).toBe(0);
    expect(first?.paused).toBe(true);
    expect(second?.volume).toBe(1);
  });

  it('画像の間も直前の音楽を流し、オフなら止める', async () => {
    const h = setup([item(1, 'audio'), item(2, 'image'), item(3, 'image')]);
    h.controller.start(queueOf([1, 2, 3]));
    await advance(0);
    const music = h.tracks[1];
    h.controller.next();
    await advance(0);
    expect(music?.paused).toBe(false);
    expect(h.last().bgm?.itemId).toBe(1);

    h.controller.setPrefs({ throughImages: false, loop: false, imageMs: 8000 });
    await advance(FADE_MS + 20);
    expect(music?.paused).toBe(true);
    expect(h.last().bgm).toBeNull();
  });

  it('画像の間に曲が終わったら無音、「BGM をループ」なら頭から', async () => {
    const h = setup([item(1, 'audio'), item(2, 'image')]);
    h.controller.start(queueOf([1, 2], [null, 60_000]));
    await advance(0);
    const music = h.tracks[1];
    h.controller.next();
    await advance(0);
    music?.fire('ended');
    expect(h.last().bgm).toBeNull();

    h.controller.jump(0);
    await advance(0);
    h.controller.next();
    await advance(0);
    h.controller.setPrefs({ throughImages: true, loop: true, imageMs: 8000 });
    if (music) {
      music.currentTime = 170;
    }
    const plays = music?.play.mock.calls.length ?? 0;
    // jump で曲を切り替えたので、鳴っているのはもう一方の要素
    const active = h.tracks.find((t) => t.src.endsWith('h1') && !t.paused);
    active?.fire('ended');
    expect(active?.currentTime).toBe(0);
    expect(h.last().bgm?.itemId).toBe(1);
    expect(plays).toBeGreaterThan(0);
  });

  it('音楽のスロットが終わって画像に入ったとき、ループならその曲を頭から', () => {
    expect(bgmOnEnter('image', 'ended', { throughImages: true, loop: true })).toBe('restart');
    expect(bgmOnEnter('image', 'ended', { throughImages: true, loop: false })).toBe('keep');
    expect(bgmOnEnter('image', 'paused', { throughImages: false, loop: false })).toBe('stop');
    expect(bgmOnEnter('video', 'none', { throughImages: true, loop: false })).toBe('keep');
    expect(bgmOnEnter('audio', 'playing', { throughImages: false, loop: false })).toBe('switch');
  });
});

describe('スロットの進み方', () => {
  it('画像は表示時間で進み、最後まで表示したことを報告する', async () => {
    const h = setup([item(1, 'image'), item(2, 'image')]);
    h.controller.start(queueOf([1, 2], [2900, null]));
    await advance(0);
    expect(h.last().imageMs).toBe(2900);
    await advance(2900 + TICK_MS);
    expect(h.last().index).toBe(1);
    expect(h.last().imageMs).toBe(8000);
    await advance(0);
    expect(h.reports).toHaveLength(1);
    expect(h.reports[0]).toMatchObject({ itemId: 1, completed: true, durationMs: null });
    // 刻み（200ms）を待たずに、表示時間ちょうどで進む
    expect(h.reports[0]?.playedMs).toBe(2900);
  });

  it('一時停止のあいだは画像の時間が進まない', async () => {
    const h = setup([item(1, 'image'), item(2, 'image')]);
    h.controller.start(queueOf([1, 2], [2000, null]));
    await advance(1000);
    h.controller.pause();
    await advance(5000);
    expect(h.last().index).toBe(0);
    h.controller.play();
    await advance(1000 + TICK_MS);
    expect(h.last().index).toBe(1);
  });

  it('[ / ] で画像の表示時間を変える（1〜60 秒）', async () => {
    const h = setup([item(1, 'image')]);
    h.controller.start(queueOf([1]));
    await advance(0);
    h.controller.stepImage(1000);
    expect(h.last().imageMs).toBe(9000);
    expect(h.last().imageOverridden).toBe(true);
    expect(stepImageMs(1000, -1000)).toBe(1000);
    expect(stepImageMs(60_000, 1000)).toBe(60_000);
  });

  it('前へ：3 秒以上なら頭に戻り、未満なら前のスロット', async () => {
    expect(prevTarget(2, 3000)).toEqual({ index: 2, restart: true });
    expect(prevTarget(2, 2999)).toEqual({ index: 1, restart: false });
    expect(prevTarget(0, 100)).toEqual({ index: 0, restart: true });

    const h = setup([item(1, 'video'), item(2, 'video')]);
    h.controller.start(queueOf([1, 2]));
    await advance(0);
    h.controller.next();
    await advance(0);
    h.video.currentTime = 5;
    await advance(TICK_MS);
    h.controller.prev();
    expect(h.video.currentTime).toBe(0);
    expect(h.last().index).toBe(1);
    h.controller.prev();
    await advance(0);
    expect(h.last().index).toBe(0);
  });

  it('再生した時間だけを報告し、シークで飛ばした分は数えない', async () => {
    const h = setup([item(1, 'video'), item(2, 'image')]);
    h.controller.start(queueOf([1, 2]));
    await advance(0);
    for (let sec = 0.2; sec <= 2; sec += 0.2) {
      h.video.currentTime = sec;
      await advance(TICK_MS);
    }
    h.controller.seek(100_000);
    h.video.currentTime = 100.2;
    await advance(TICK_MS);
    h.controller.next();
    await advance(0);
    expect(h.reports).toHaveLength(1);
    expect(h.reports[0]).toMatchObject({ itemId: 1, completed: false, durationMs: 180_000 });
    expect(h.reports[0]?.playedMs).toBeGreaterThan(1900);
    expect(h.reports[0]?.playedMs).toBeLessThan(2500);
  });

  it('音楽の長さはミリ秒の整数で報告する（Rust は整数で受け取る）', async () => {
    const h = setup([item(1, 'audio'), item(2, 'image')]);
    h.controller.start(queueOf([1, 2]));
    await advance(0);
    const music = h.tracks[1];
    if (music) {
      music.duration = 8.006999;
      for (let sec = 0.2; sec <= 8; sec += 0.2) {
        music.currentTime = sec;
        await advance(TICK_MS);
      }
      music.fire('ended');
    }
    await advance(0);
    expect(h.reports[0]).toMatchObject({ itemId: 1, completed: true, durationMs: 8007 });
    expect(Number.isInteger(h.reports[0]?.playedMs)).toBe(true);
  });

  it('終わりまで再生したら止まり、リピートなら先頭へ', async () => {
    const h = setup([item(1, 'video')]);
    h.controller.start(queueOf([1]));
    await advance(0);
    h.video.fire('ended');
    expect(h.last().phase).toBe('ended');
    expect(h.last().playing).toBe(false);

    h.controller.setRepeat('all');
    h.controller.play();
    await advance(0);
    h.video.fire('ended');
    await advance(0);
    expect(h.last().phase).not.toBe('ended');
    expect(h.last().index).toBe(0);
  });

  it('ユニットの最初のスロットを報告するときは unitStart', async () => {
    const h = setup([item(1, 'image'), item(2, 'image')]);
    const queue = queueOf([1, 2], [2000, 2000]);
    queue.unitIds = [7, 7];
    queue.indexInUnit = [0, 1];
    queue.unitLen = [2, 2];
    h.controller.start(queue);
    await advance(4000 + TICK_MS * 2);
    expect(h.reports.map((r) => [r.itemId, r.unitId, r.unitStart])).toEqual([
      [1, 7, true],
      [2, 7, false],
    ]);
  });
});

describe('再生しない種類（フォント・3D。docs/playlists.md §1.1）', () => {
  it('キューに入っていても鳴らさず、先読みもせずに次のスロットへ進む', async () => {
    const h = setup([item(1, 'image'), item(2, 'font'), item(3, 'model'), item(4, 'audio')]);
    h.controller.start(queueOf([1, 2, 3, 4], [1000]));
    await advance(0);
    // 次のスロット（フォント）を音楽・動画の要素に読ませない
    expect(h.created.map((m) => m.src).filter((src) => /h[23]$/.test(src))).toEqual([]);
    await advance(1000 + TICK_MS);
    expect(h.last().index).toBe(3);
    expect(h.last().kind).toBe('audio');
    expect(h.created.map((m) => m.src).filter((src) => /h[23]$/.test(src))).toEqual([]);
    await advance(0);
    // 飛ばしたスロットは報告しない（再生済みにしない）
    expect(h.reports.map((r) => r.itemId)).toEqual([1]);
    expect(h.requests.flatMap((r) => r.ids)).not.toContain(2);
    expect(h.requests.flatMap((r) => r.ids)).not.toContain(3);
  });

  it('最後なら終わりにする（全体の繰り返しでも、飛ばすものだけのときは回り続けない）', async () => {
    const h = setup([item(1, 'font'), item(2, 'model')]);
    h.controller.setRepeat('all');
    h.controller.start(queueOf([1, 2]));
    await advance(0);
    expect(h.last().phase).toBe('ended');
    expect(h.last().kind).toBeNull();
    expect(h.reports).toEqual([]);
  });
});

describe('プロキシ', () => {
  it('作成待ちなら待ってから、できたプロキシで再生する', async () => {
    const h = setup([item(1, 'video', { proxyState: 'pending' })]);
    h.controller.start(queueOf([1]));
    await advance(0);
    expect(h.last().phase).toBe('waiting');
    expect(h.requests[0]).toEqual({ ids: [1], force: false });
    expect(h.video.src).toBe('');

    h.items.set(1, item(1, 'video', { proxyState: 'ready' }));
    await advance(POLL_MS);
    expect(h.video.src).toBe('kura://localhost/play/h1?a=1');
    h.video.fire('loadeddata');
    expect(h.last().phase).toBe('playing');
  });

  it('元のまま再生できなかったら作り直しを頼み、それでもだめなら外部プレイヤーへ', async () => {
    const h = setup([item(1, 'video', { proxyState: 'not_needed' })]);
    h.controller.start(queueOf([1]));
    await advance(0);
    h.video.fire('error');
    expect(h.last().phase).toBe('waiting');
    expect(h.requests.at(-1)).toEqual({ ids: [1], force: true });

    h.items.set(1, item(1, 'video', { proxyState: 'ready' }));
    await advance(POLL_MS);
    h.video.fire('error');
    expect(h.last().phase).toBe('failed');
  });
});

describe('プレイリスト（M6）', () => {
  it('⇧→ は次のユニットの先頭、⇧← は今のユニットの先頭（先頭なら前のユニット）', async () => {
    const ids = [1, 2, 3, 4, 5, 6];
    const h = setup(ids.map((id) => item(id, 'image')));
    h.controller.start(unitQueue(ids, [3, 2, 1]));
    await advance(0);
    h.controller.next();
    await advance(0);
    expect(h.last().index).toBe(1);
    expect([h.last().groupIndex, h.last().groupCount, h.last().indexInUnit]).toEqual([0, 3, 1]);
    h.controller.nextUnit();
    await advance(0);
    expect(h.last().index).toBe(3);
    h.controller.next();
    await advance(0);
    h.controller.prevUnit();
    await advance(0);
    expect(h.last().index).toBe(3);
    h.controller.prevUnit();
    await advance(0);
    expect(h.last().index).toBe(0);
    // ファイル単位では次・前のスロットと同じ
    const f = setup(ids.map((id) => item(id, 'image')));
    f.controller.start(queueOf(ids));
    await advance(0);
    f.controller.nextUnit();
    await advance(0);
    expect(f.last().index).toBe(1);
    expect(f.last().groupIndex).toBeNull();
  });

  it('1 曲リピートは最後まで再生したら同じスロット、次へは次のスロット', async () => {
    const h = setup([item(1, 'video'), item(2, 'video')]);
    h.controller.start(queueOf([1, 2]));
    h.controller.setRepeat('one');
    await advance(0);
    h.video.fire('ended');
    await advance(0);
    expect(h.last().index).toBe(0);
    expect(h.reports.at(-1)).toMatchObject({ itemId: 1, completed: true });
    h.controller.next();
    await advance(0);
    expect(h.last().index).toBe(1);
  });

  it('全体リピートの終わりで作り直しを頼む', async () => {
    const h = setup([item(1, 'video')]);
    let wraps = 0;
    h.controller.onWrap(() => {
      wraps += 1;
    });
    h.controller.start(queueOf([1]));
    h.controller.setRepeat('all');
    await advance(0);
    h.video.fire('ended');
    expect(wraps).toBe(1);
    expect(h.reports).toHaveLength(1);
  });

  it('トリムの始まりから再生し、終わりで次へ。再開の位置からも始められる', async () => {
    const h = setup([item(1, 'video'), item(2, 'image')]);
    const queue = queueOf([1, 2], [null, 8000]);
    queue.startMs[0] = 2000;
    queue.endMs[0] = 4000;
    h.controller.start(queue);
    await advance(0);
    expect(h.video.currentTime).toBe(2);
    h.video.currentTime = 4.1;
    await advance(TICK_MS);
    expect(h.last().index).toBe(1);

    const r = setup([item(1, 'image')]);
    r.controller.start(queueOf([1], [8000]), 0, 5000);
    await advance(0);
    expect(r.last().positionMs).toBe(5000);
    await advance(3000 + TICK_MS);
    expect(r.last().phase).toBe('ended');
  });

  it('プレイリストから再生したら、報告にプレイリストを付ける', async () => {
    const h = setup([item(1, 'image'), item(2, 'image')]);
    const queue = queueOf([1, 2], [1000, 1000]);
    queue.playlist = {
      id: 9,
      name: '夜の作業',
      kind: 'manual',
      shuffle: false,
      seedLocked: false,
      repeatMode: 'off',
      bgmThroughImages: true,
      maxSameKindRun: 2,
    };
    h.controller.start(queue);
    await advance(1000 + TICK_MS);
    expect(h.reports[0]).toMatchObject({ itemId: 1, playlistId: 9 });
  });
});
