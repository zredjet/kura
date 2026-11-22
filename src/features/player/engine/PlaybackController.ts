import type { ItemSummaryDto } from '../../../ipc/items';
import { hasDuration, isPlayable, needsProxy, type TimedKind } from '../../../ipc/kinds';
import { type BgmAction, type BgmStatus, bgmOnEnter, bgmOnTrackEnd } from './bgm';
import { FADE_MS, Fader } from './fade';
import { nextIndex, playedDelta, prevTarget, stepImageMs } from './nav';
import type {
  BgmInfo,
  ControllerDeps,
  MediaPort,
  PlayerSnapshot,
  Queue,
  RepeatMode,
  SlotKind,
  SlotPhase,
} from './types';

/** 再生位置・画像の表示時間を進める間隔。 */
export const TICK_MS = 200;
/** プロキシの作成を待つあいだ、状態を確かめる間隔。 */
export const POLL_MS = 1500;
/** 先に概要を取っておくスロット数（今のスロットの後ろ）。 */
const LOOKAHEAD = 3;
/** 先に decode しておく画像の数（docs/playlists.md §6.1）。 */
const PRELOAD_IMAGES = 2;

/** BGM 用の `<audio>`（クロスフェードのため 2 つ）。 */
interface Track {
  el: MediaPort;
  fader: Fader;
}

/** 再生の設定のうち、コントローラが使うもの。 */
export interface ControllerPrefs {
  /** 画像の間も直前の音楽を流す。 */
  throughImages: boolean;
  /** 画像の間に音楽が終わったらループする。 */
  loop: boolean;
  /** 画像の表示時間の既定値（キューに表示時間がないとき）。 */
  imageMs: number;
}

/**
 * 再生キューを順に再生する（docs/playlists.md §6）。並び替えのロジックは持たない。
 * 要素は `<video>`（画面に置くものを `attachVideo` で受け取る）、BGM 用の `<audio>` 2 つ、画像レイヤー（画面側）。
 */
export class PlaybackController {
  private queue: Queue | null = null;
  private index = 0;
  private playing = false;
  private phase: SlotPhase = 'loading';
  private readonly items = new Map<number, ItemSummaryDto>();
  private video: MediaPort | null = null;
  private readonly tracks: [Track, Track];
  private active: 0 | 1 = 0;
  private bgmStatus: BgmStatus = 'none';
  private bgmItem: number | null = null;
  /** BGM の曲が自分のスロット（音楽のスロット）として流れているときのスロット番号。 */
  private bgmSlot: number | null = null;
  private readonly preloaders: Record<TimedKind, MediaPort>;
  private readonly preloadedImages: string[] = [];
  private volume = 0.8;
  private prefs: ControllerPrefs = { throughImages: true, loop: false, imageMs: 8000 };
  private imageOverride: number | null = null;
  private repeat: RepeatMode = 'off';
  /** キューの終わりで「全体を繰り返す」とき、作り直しを頼む先（なければ先頭へ戻る）。 */
  private wrapHandler: (() => void) | null = null;
  /** 始める位置（再開・トリム）。 */
  private pendingStartMs = 0;

  // 今のスロットの数え方
  private positionMs = 0;
  private durationMs: number | null = null;
  private playedMs = 0;
  private completed = false;
  private lastTick = 0;
  private lastMediaSec = 0;

  private readonly attempts = new Map<number, number>();
  private readonly forced = new Set<number>();
  private ticker: number | null = null;
  private pollTimer: number | null = null;
  private enterToken = 0;
  private readonly listeners = new Set<(snapshot: PlayerSnapshot) => void>();
  private readonly recordedListeners = new Set<(itemId: number) => void>();

  constructor(private readonly deps: ControllerDeps) {
    const makeTrack = (i: number): Track => {
      const el = deps.createMedia('audio');
      el.preload = 'auto';
      const fader = new Fader(deps.clock, (gain) => {
        el.volume = clampVolume(this.volume * gain);
      });
      el.addEventListener('ended', () => this.onTrackEnded(i));
      el.addEventListener('error', () => this.onTrackError(i));
      el.addEventListener('loadeddata', () => this.onLoaded(el));
      return { el, fader };
    };
    this.tracks = [makeTrack(0), makeTrack(1)];
    this.preloaders = { audio: deps.createMedia('audio'), video: deps.createMedia('video') };
    this.preloaders.audio.preload = 'metadata';
    this.preloaders.video.preload = 'metadata';
  }

  // ===== 外から使うもの =====

  /** 状態が変わるたびに呼ぶ。戻り値で受け取りをやめる。 */
  subscribe(listener: (snapshot: PlayerSnapshot) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** 再生済みとして記録されたアイテムを受け取る（履歴のため）。 */
  onRecorded(listener: (itemId: number) => void): () => void {
    this.recordedListeners.add(listener);
    return () => this.recordedListeners.delete(listener);
  }

  /** 画面に置いた `<video>` を受け取る。 */
  attachVideo(el: MediaPort | null): void {
    if (this.video === el) {
      return;
    }
    if (this.video) {
      this.video.removeEventListener('ended', this.onVideoEnded);
      this.video.removeEventListener('error', this.onVideoError);
      this.video.removeEventListener('loadeddata', this.onVideoLoaded);
    }
    this.video = el;
    if (el) {
      el.volume = clampVolume(this.volume);
      el.addEventListener('ended', this.onVideoEnded);
      el.addEventListener('error', this.onVideoError);
      el.addEventListener('loadeddata', this.onVideoLoaded);
    }
  }

  /** 概要（取ってあれば）。 */
  item(id: number): ItemSummaryDto | undefined {
    return this.items.get(id);
  }

  /** キュー。 */
  getQueue(): Queue | null {
    return this.queue;
  }

  /** 今の状態。 */
  snapshot(): PlayerSnapshot {
    const queue = this.queue;
    const itemId = queue ? (queue.itemIds[this.index] ?? null) : null;
    const kind = slotKindOf(itemId !== null ? this.items.get(itemId) : undefined);
    return {
      index: this.index,
      length: queue?.itemIds.length ?? 0,
      playing: this.playing,
      phase: this.phase,
      kind,
      itemId,
      positionMs: this.positionMs,
      durationMs: this.durationMs,
      imageMs: this.imageMs(),
      imageOverridden: this.imageOverride !== null,
      bgm: this.bgmInfo(kind),
      repeat: this.repeat,
      groupIndex: queue?.groups.length ? (queue.groupIndex[this.index] ?? null) : null,
      groupCount: queue?.groups.length ?? 0,
      indexInUnit: queue?.indexInUnit[this.index] ?? 0,
      unitLen: queue?.unitLen[this.index] ?? 1,
    };
  }

  /** キューを読み込んで `index` から（`positionMs` の位置から）再生する。 */
  start(queue: Queue, index = 0, positionMs = 0): void {
    this.leave();
    this.stopAll();
    this.queue = queue;
    this.playing = true;
    this.startTicker();
    this.enter(Math.min(Math.max(0, index), queue.itemIds.length - 1), positionMs);
  }

  /** 画像の表示時間をこのセッションで変えた分を戻す（新しいキューを始めるとき）。 */
  resetImageOverride(): void {
    this.imageOverride = null;
  }

  /** キューの終わりで全体を繰り返すとき、キューの作り直しを頼む先（シャッフルでシードを固定していないとき）。 */
  onWrap(handler: (() => void) | null): void {
    this.wrapHandler = handler;
  }

  /** 再生を終える（今のスロットを報告して、すべて止める）。 */
  close(): void {
    this.leave();
    this.stopAll();
    this.queue = null;
    this.playing = false;
    this.phase = 'loading';
    this.stopTicker();
    this.emit();
  }

  play(): void {
    if (!this.queue) {
      return;
    }
    if (this.phase === 'ended') {
      this.playing = true;
      this.enter(0);
      return;
    }
    this.playing = true;
    this.lastTick = this.deps.clock.now();
    const media = this.slotMedia();
    if (media && this.phase !== 'waiting' && this.phase !== 'failed') {
      playSafely(media);
    }
    const track = this.tracks[this.active];
    if (this.bgmStatus === 'playing' && track.el !== media) {
      playSafely(track.el);
    }
    this.emit();
  }

  pause(): void {
    this.playing = false;
    this.video?.pause();
    this.tracks[this.active].el.pause();
    this.emit();
  }

  togglePlay(): void {
    if (this.playing) {
      this.pause();
    } else {
      this.play();
    }
  }

  /** 次のスロット（終わりならリピートで先頭、でなければ終える）。 */
  next(): void {
    this.completed = false;
    this.advance();
  }

  /** 次のまとまりの先頭（ユニット単位。ファイル単位では次のスロット。§6.3 の ⇧→）。 */
  nextUnit(): void {
    const queue = this.queue;
    const group = this.groupAt(this.index);
    if (!queue || !group) {
      this.next();
      return;
    }
    const target = group.start + group.len;
    if (target < queue.itemIds.length) {
      this.enter(target);
    } else if (this.repeat === 'all') {
      this.completed = false;
      this.advanceFrom(queue.itemIds.length - 1);
    } else {
      this.finish();
    }
  }

  /** 今のまとまりの先頭（すでに先頭なら前のまとまりの先頭。ファイル単位では前のスロット。§6.3 の ⇧←）。 */
  prevUnit(): void {
    const queue = this.queue;
    const group = this.groupAt(this.index);
    if (!queue || !group) {
      this.prev();
      return;
    }
    if (this.index > group.start) {
      this.enter(group.start);
      return;
    }
    const previous = this.groupAt(group.start - 1);
    this.enter(previous ? previous.start : group.start);
  }

  /** 前へ：再生位置が 3 秒以上なら頭に戻し、未満なら前のスロット（docs/playlists.md §6.3）。 */
  prev(): void {
    if (!this.queue) {
      return;
    }
    const target = prevTarget(this.index, this.positionMs);
    if (target.restart) {
      this.seek(0);
    } else {
      this.enter(target.index);
    }
  }

  /** キューの `index` 番目へ。 */
  jump(index: number): void {
    if (this.queue && index >= 0 && index < this.queue.itemIds.length) {
      if (this.phase === 'ended') {
        this.playing = true;
      }
      this.enter(index);
    }
  }

  /** 再生位置を変える（画像は表示した時間）。シークで飛ばした分は再生時間に数えない。 */
  seek(ms: number): void {
    const target = Math.max(0, ms);
    const media = this.slotMedia();
    if (media) {
      media.currentTime = target / 1000;
      this.lastMediaSec = target / 1000;
    }
    this.positionMs = target;
    this.emit();
  }

  setVolume(volume: number): void {
    this.volume = clampVolume(volume);
    if (this.video) {
      this.video.volume = this.volume;
    }
    for (const track of this.tracks) {
      track.el.volume = clampVolume(this.volume * track.fader.gain);
    }
  }

  setPrefs(prefs: ControllerPrefs): void {
    const wasThrough = this.prefs.throughImages;
    this.prefs = prefs;
    // 画像の間に流している曲は、オフにしたら止める
    if (wasThrough && !prefs.throughImages && this.currentKind() === 'image') {
      this.applyBgm(bgmOnEnter('image', this.bgmStatus, prefs));
    }
    this.emit();
  }

  setRepeat(repeat: RepeatMode): void {
    this.repeat = repeat;
    this.emit();
  }

  /** 画像の表示時間を変える（このセッションだけ。docs/playlists.md §6.3 の [ / ]）。 */
  stepImage(deltaMs: number): void {
    this.imageOverride = stepImageMs(this.imageMs(), deltaMs);
    this.emit();
  }

  /** 再生できなかったものをもう一度変換してもらう。 */
  retry(): void {
    const item = this.currentItem();
    if (!item || this.phase !== 'failed') {
      return;
    }
    this.forced.add(item.id);
    this.waitForProxy(item, true);
  }

  // ===== スロットの出入り =====

  private enter(index: number, positionMs = 0): void {
    const queue = this.queue;
    if (!queue) {
      return;
    }
    this.leave();
    const token = ++this.enterToken;
    this.index = index;
    this.pendingStartMs = Math.max(positionMs, queue.startMs[index] ?? 0);
    this.positionMs = 0;
    this.durationMs = null;
    this.playedMs = 0;
    this.completed = false;
    this.lastMediaSec = 0;
    this.lastTick = this.deps.clock.now();
    this.phase = 'loading';
    this.stopPolling();

    const wanted = this.windowIds();
    const missing = wanted.filter((id) => !this.items.has(id));
    if (missing.length === 0) {
      this.begin(token);
      return;
    }
    this.emit();
    void this.deps
      .fetchItems(missing)
      .then((rows) => {
        for (const row of rows) {
          this.items.set(row.id, row);
        }
      })
      .catch((error: unknown) => console.warn('再生する項目の情報を取れませんでした', error))
      .finally(() => {
        if (token === this.enterToken) {
          this.begin(token);
        }
      });
  }

  /** 概要がそろってから、スロットの再生を始める。 */
  private begin(token: number): void {
    const item = this.currentItem();
    if (token !== this.enterToken) {
      return;
    }
    if (!item) {
      // 消えたアイテムは飛ばす
      this.phase = 'failed';
      this.emit();
      return;
    }
    const kind = slotKindOf(item);
    if (kind === null) {
      this.skipUnplayable();
      return;
    }
    if (kind !== 'video' && this.video) {
      stopMedia(this.video);
    }
    if (kind !== 'audio') {
      this.applyBgm(bgmOnEnter(kind, this.bgmStatus, this.prefs));
    }
    if (kind === 'image') {
      this.durationMs = this.imageMs();
      this.positionMs = Math.min(this.pendingStartMs, this.durationMs);
      this.phase = 'playing';
      this.lastTick = this.deps.clock.now();
    } else if (item.proxyState === 'pending') {
      this.waitForProxy(item, false);
    } else {
      this.startMedia(item);
    }
    this.preloadAhead();
    this.requestAhead();
    this.emit();
  }

  /**
   * 再生しない種類（フォント・3D）のスロットを飛ばす。キューは Rust が除いて作るので、ここに来るのは万一のときだけ。
   * 鳴らさず・報告せずに次のスロットへ進み、最後なら終わりにする（全体の繰り返しでも折り返さない。
   * 飛ばすものだけのキューで回り続けないため）。
   */
  private skipUnplayable(): void {
    const length = this.queue?.itemIds.length ?? 0;
    if (this.index + 1 < length) {
      this.enter(this.index + 1);
    } else {
      this.finish();
    }
  }

  /** 今のスロットを離れる：再生した時間を報告する（判定は Rust）。 */
  private leave(): void {
    const queue = this.queue;
    const itemId = queue?.itemIds[this.index];
    if (!queue || itemId === undefined || (this.playedMs <= 0 && !this.completed)) {
      return;
    }
    const unitId = queue.unitIds[this.index] ?? null;
    const item = this.items.get(itemId);
    const report = {
      itemId,
      unitId,
      playlistId: queue.playlist?.id ?? null,
      unitStart: unitId !== null && queue.indexInUnit[this.index] === 0,
      playedMs: Math.round(this.playedMs),
      completed: this.completed,
      durationMs:
        item?.kind === 'image'
          ? null
          : Math.round(this.durationMs ?? item?.durationMs ?? 0) || null,
    };
    this.playedMs = 0;
    this.completed = false;
    void this.deps
      .report(report)
      .then((recorded) => {
        if (recorded) {
          this.recordedListeners.forEach((fn) => fn(itemId));
        }
      })
      .catch((error: unknown) => console.warn('再生の記録に失敗しました', error));
  }

  /** 今のスロットが終わった・次へ：次のスロットへ。終わりなら止める。 */
  private advance(): void {
    // 1 曲リピートは、最後まで再生したときだけ同じスロットをもう一度
    if (this.repeat === 'one' && this.completed) {
      this.enter(this.index);
      return;
    }
    this.advanceFrom(this.index);
  }

  private advanceFrom(index: number): void {
    const queue = this.queue;
    if (!queue) {
      return;
    }
    const atEnd = index + 1 >= queue.itemIds.length;
    if (atEnd && this.repeat === 'all' && this.wrapHandler) {
      this.leave();
      this.wrapHandler();
      return;
    }
    const next = nextIndex(index, queue.itemIds.length, this.repeat === 'all');
    if (next === null) {
      this.finish();
    } else {
      this.enter(next);
    }
  }

  /** `index` のスロットが入っているまとまり（ユニット単位のときだけ）。 */
  private groupAt(index: number): { start: number; len: number } | null {
    const queue = this.queue;
    if (!queue || queue.groups.length === 0 || index < 0) {
      return null;
    }
    const group = queue.groups[queue.groupIndex[index] ?? -1];
    return group ? { start: group.start, len: group.len } : null;
  }

  /** キューの終わり。 */
  private finish(): void {
    this.leave();
    this.playing = false;
    this.phase = 'ended';
    this.video?.pause();
    if (this.bgmStatus === 'playing') {
      this.applyBgm('fadeOutStop');
    }
    this.emit();
  }

  // ===== 音楽・動画 =====

  private startMedia(item: ItemSummaryDto): void {
    const url = this.deps.playUrl(item.hash, this.attempts.get(item.id) ?? 0);
    if (item.kind === 'video') {
      const video = this.video;
      if (!video) {
        this.phase = 'failed';
        return;
      }
      video.src = url;
      video.volume = clampVolume(this.volume);
      if (this.pendingStartMs > 0) {
        video.currentTime = this.pendingStartMs / 1000;
        this.lastMediaSec = this.pendingStartMs / 1000;
      }
      if (this.playing) {
        playSafely(video);
      }
    } else {
      this.switchTrack(item, url);
    }
  }

  /** 音楽のスロット：BGM をその曲に切り替える（流れていれば 300ms のクロスフェード）。 */
  private switchTrack(item: ItemSummaryDto, url: string): void {
    const old = this.tracks[this.active];
    const target: 0 | 1 = this.active === 0 ? 1 : 0;
    const next = this.tracks[target];
    next.fader.cancel();
    next.el.src = url;
    if (this.pendingStartMs > 0) {
      next.el.currentTime = this.pendingStartMs / 1000;
      this.lastMediaSec = this.pendingStartMs / 1000;
    }
    const audible = this.bgmStatus === 'playing' && this.playing;
    if (audible) {
      next.fader.set(0);
      old.fader.fade(0, FADE_MS, () => old.el.pause());
      next.fader.fade(1, FADE_MS);
    } else {
      old.fader.cancel();
      old.el.pause();
      next.fader.set(1);
    }
    if (this.playing) {
      playSafely(next.el);
    }
    this.active = target;
    this.bgmItem = item.id;
    this.bgmSlot = this.index;
    this.bgmStatus = 'playing';
  }

  private applyBgm(action: BgmAction): void {
    const track = this.tracks[this.active];
    switch (action) {
      case 'keep':
      case 'switch':
        return;
      case 'fadeOutPause':
        this.bgmStatus = 'paused';
        track.fader.fade(0, FADE_MS, () => track.el.pause());
        return;
      case 'fadeInResume':
        this.bgmStatus = 'playing';
        if (this.playing) {
          playSafely(track.el);
        }
        track.fader.fade(1, FADE_MS);
        return;
      case 'restart':
        this.bgmStatus = 'playing';
        track.el.currentTime = 0;
        track.fader.set(1);
        if (this.playing) {
          playSafely(track.el);
        }
        return;
      case 'fadeOutStop':
        this.bgmStatus = 'none';
        this.bgmItem = null;
        track.fader.fade(0, FADE_MS, () => track.el.pause());
        return;
      case 'stop':
        this.bgmStatus = 'none';
        this.bgmItem = null;
        track.fader.cancel();
        track.el.pause();
        return;
    }
  }

  private onTrackEnded(i: number): void {
    if (i !== this.active) {
      return;
    }
    if (this.bgmSlot === this.index && this.currentKind() === 'audio') {
      this.completed = true;
      this.bgmStatus = 'ended';
      this.advance();
      return;
    }
    // 画像の間に流していた曲が終わった
    if (bgmOnTrackEnd(this.prefs) === 'restart' && this.currentKind() === 'image') {
      this.applyBgm('restart');
    } else {
      this.bgmStatus = 'ended';
    }
    this.emit();
  }

  private onTrackError(i: number): void {
    if (i !== this.active) {
      return;
    }
    if (this.bgmSlot === this.index && this.currentKind() === 'audio') {
      this.onMediaError();
    } else {
      this.bgmStatus = 'none';
      this.bgmItem = null;
      this.emit();
    }
  }

  private readonly onVideoEnded = () => {
    if (this.currentKind() === 'video') {
      this.completed = true;
      this.advance();
    }
  };

  private readonly onVideoError = () => {
    // src を外したときの error は数えない
    if (this.currentKind() === 'video' && this.video?.src) {
      this.onMediaError();
    }
  };

  private readonly onVideoLoaded = () => {
    if (this.video) {
      this.onLoaded(this.video);
    }
  };

  private onLoaded(el: MediaPort): void {
    if (el === this.slotMedia() && this.phase === 'loading') {
      this.phase = 'playing';
      this.durationMs = finiteMs(el.duration);
      this.emit();
    }
  }

  /** 再生できなかった：元のまま再生できるはずだったものはプロキシを作ってもらって待つ。 */
  private onMediaError(): void {
    const item = this.currentItem();
    if (!item || this.phase === 'waiting') {
      return;
    }
    if (item.proxyState === 'ready' || this.forced.has(item.id)) {
      this.phase = 'failed';
      this.emit();
      return;
    }
    this.forced.add(item.id);
    this.waitForProxy(item, true);
  }

  /** プロキシができるまで待ち、できたら再生する。 */
  private waitForProxy(item: ItemSummaryDto, force: boolean): void {
    this.phase = 'waiting';
    this.stopPolling();
    const token = this.enterToken;
    void this.deps.requestProxies([item.id], force).catch((error: unknown) => {
      console.warn('プロキシを頼めませんでした', error);
    });
    const poll = () => {
      this.pollTimer = null;
      void this.deps
        .fetchItems([item.id])
        .then(([row]) => {
          if (token !== this.enterToken || !row) {
            return;
          }
          this.items.set(row.id, row);
          if (row.proxyState === 'ready') {
            this.attempts.set(row.id, (this.attempts.get(row.id) ?? 0) + 1);
            this.phase = 'loading';
            this.startMedia(row);
            this.emit();
          } else if (row.proxyState === 'failed' || row.proxyState === 'not_needed') {
            this.phase = 'failed';
            this.emit();
          } else {
            this.pollTimer = this.deps.clock.setTimeout(poll, POLL_MS);
          }
        })
        .catch(() => {
          if (token === this.enterToken) {
            this.pollTimer = this.deps.clock.setTimeout(poll, POLL_MS);
          }
        });
    };
    this.pollTimer = this.deps.clock.setTimeout(poll, POLL_MS);
    this.emit();
  }

  private stopPolling(): void {
    if (this.pollTimer !== null) {
      this.deps.clock.clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  // ===== 時間 =====

  private startTicker(): void {
    if (this.ticker !== null) {
      return;
    }
    const tick = () => {
      this.ticker = this.deps.clock.setTimeout(tick, TICK_MS);
      this.tick();
    };
    this.ticker = this.deps.clock.setTimeout(tick, TICK_MS);
  }

  private stopTicker(): void {
    if (this.ticker !== null) {
      this.deps.clock.clearTimeout(this.ticker);
      this.ticker = null;
    }
  }

  private tick(): void {
    const now = this.deps.clock.now();
    const dt = now - this.lastTick;
    this.lastTick = now;
    if (!this.queue || (this.phase !== 'playing' && this.phase !== 'loading')) {
      this.emit();
      return;
    }
    const kind = this.currentKind();
    if (kind === 'image') {
      if (this.playing) {
        this.positionMs += dt;
        this.playedMs += dt;
      }
      this.durationMs = this.imageMs();
      if (this.positionMs >= this.durationMs) {
        this.positionMs = this.durationMs;
        this.completed = true;
        this.advance();
        return;
      }
      // 終わりが次の刻みより前なら、ちょうどのときにもう一度確かめる（表示時間を延ばしすぎない）
      const remaining = this.durationMs - this.positionMs;
      if (this.playing && remaining < TICK_MS) {
        const token = this.enterToken;
        this.deps.clock.setTimeout(() => {
          if (token === this.enterToken) {
            this.tick();
          }
        }, remaining);
      }
    } else {
      const media = this.slotMedia();
      if (media) {
        const sec = media.currentTime;
        if (this.phase === 'loading' && sec > 0) {
          this.phase = 'playing';
        }
        if (this.playing) {
          this.playedMs += playedDelta(this.lastMediaSec, sec);
        }
        this.lastMediaSec = sec;
        this.positionMs = sec * 1000;
        this.durationMs = finiteMs(media.duration) ?? this.durationMs;
        // トリムの終わり
        const endMs = this.queue.endMs[this.index];
        if (endMs !== null && endMs !== undefined && this.positionMs >= endMs) {
          this.completed = true;
          this.advance();
          return;
        }
      }
    }
    this.emit();
  }

  // ===== 先読み =====

  /** 前 1 件と後ろ `LOOKAHEAD` 件のアイテム。 */
  private windowIds(): number[] {
    const ids = this.queue?.itemIds ?? [];
    const out: number[] = [];
    for (let i = Math.max(0, this.index - 1); i <= this.index + LOOKAHEAD && i < ids.length; i++) {
      const id = ids[i];
      if (id !== undefined && !out.includes(id)) {
        out.push(id);
      }
    }
    return out;
  }

  /** 次の画像 2 件を decode し、次のスロットが動画・音楽なら metadata だけ読んでおく。 */
  private preloadAhead(): void {
    const ids = this.queue?.itemIds ?? [];
    let images = 0;
    for (let i = this.index + 1; i <= this.index + LOOKAHEAD && i < ids.length; i++) {
      const item = this.items.get(ids[i] ?? -1);
      if (!item) {
        continue;
      }
      const url = this.deps.playUrl(item.hash, this.attempts.get(item.id) ?? 0);
      if (item.kind === 'image' && images < PRELOAD_IMAGES) {
        images += 1;
        if (!this.preloadedImages.includes(url)) {
          this.preloadedImages.push(url);
          this.preloadedImages.splice(0, Math.max(0, this.preloadedImages.length - 8));
          this.deps.preloadImage(url);
        }
      } else if (i === this.index + 1 && hasDuration(item.kind) && item.proxyState !== 'pending') {
        const preloader = this.preloaders[item.kind];
        if (preloader.src !== url) {
          preloader.src = url;
        }
      }
    }
  }

  /** 今と次の動画・音楽で、判定・作成待ちのものを先に処理してもらう。 */
  private requestAhead(): void {
    const ids = this.queue?.itemIds ?? [];
    const want: number[] = [];
    for (let i = this.index; i <= this.index + LOOKAHEAD && i < ids.length; i++) {
      const item = this.items.get(ids[i] ?? -1);
      if (item && needsProxy(item.kind) && ['unknown', 'pending'].includes(item.proxyState)) {
        want.push(item.id);
      }
    }
    if (want.length > 0) {
      void this.deps.requestProxies(want, false).catch(() => undefined);
    }
  }

  // ===== その他 =====

  private currentItem(): ItemSummaryDto | undefined {
    const id = this.queue?.itemIds[this.index];
    return id !== undefined ? this.items.get(id) : undefined;
  }

  private currentKind(): SlotKind | null {
    return slotKindOf(this.currentItem());
  }

  /** 今のスロットの音楽・動画を鳴らしている要素。 */
  private slotMedia(): MediaPort | null {
    const kind = this.currentKind();
    if (kind === 'video') {
      return this.video;
    }
    if (kind === 'audio' && this.bgmSlot === this.index) {
      return this.tracks[this.active].el;
    }
    return null;
  }

  private imageMs(): number {
    return this.imageOverride ?? this.queue?.imageDurationMs[this.index] ?? this.prefs.imageMs;
  }

  private bgmInfo(kind: SlotKind | null): BgmInfo | null {
    if (kind !== 'image' || this.bgmStatus !== 'playing' || this.bgmItem === null) {
      return null;
    }
    const el = this.tracks[this.active].el;
    return {
      itemId: this.bgmItem,
      positionMs: el.currentTime * 1000,
      durationMs: finiteMs(el.duration),
    };
  }

  private stopAll(): void {
    this.stopPolling();
    if (this.video) {
      stopMedia(this.video);
    }
    for (const track of this.tracks) {
      track.fader.cancel();
      track.el.pause();
    }
    this.bgmStatus = 'none';
    this.bgmItem = null;
    this.bgmSlot = null;
  }

  private emit(): void {
    const snapshot = this.snapshot();
    this.listeners.forEach((fn) => fn(snapshot));
  }
}

function clampVolume(v: number): number {
  return Math.min(1, Math.max(0, v));
}

/** スロットの種類（再生しない種類・概要がまだないものは null）。 */
function slotKindOf(item: ItemSummaryDto | undefined): SlotKind | null {
  return item && isPlayable(item.kind) ? item.kind : null;
}

/** 秒をミリ秒の整数にする（Rust には整数で渡す）。長さがわからなければ null。 */
function finiteMs(sec: number): number | null {
  return Number.isFinite(sec) && sec > 0 ? Math.round(sec * 1000) : null;
}

/** 再生を始める（自動再生の制限などで断られても例外にしない）。 */
function playSafely(media: MediaPort): void {
  const result = media.play() as Promise<void> | undefined;
  result?.catch((error: unknown) => console.warn('再生を始められませんでした', error));
}

/** 動画を止めて読み込みを外す。 */
function stopMedia(media: MediaPort): void {
  media.pause();
  if (media.src) {
    media.removeAttribute('src');
    media.load();
  }
}
