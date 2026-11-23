import { useCallback, useSyncExternalStore } from 'react';

/**
 * `<video>`・`<audio>` の状態を React に写す（クイックルックのカプセル。ADR-230）。
 *
 * 要素のイベント（再生・一時停止・終わり・長さ・音量・シーク）と、鳴っている間は描くたびに位置を読み、変わったときだけ
 * 新しい値にする（`useSyncExternalStore` の getSnapshot は同じ値なら同じオブジェクトを返す必要があるので、値は購読の
 * 中でだけ作り直し、getSnapshot はそれを返す）。
 */

export interface MediaState {
  paused: boolean;
  ended: boolean;
  /** 今の位置（ms）。 */
  currentMs: number;
  /** 長さ（ms。分からなければ null）。 */
  durationMs: number | null;
  /** 音量（0〜1）。 */
  volume: number;
}

type MediaLike = Pick<HTMLMediaElement, 'paused' | 'ended' | 'currentTime' | 'duration' | 'volume'>;

const EMPTY: MediaState = { paused: true, ended: false, currentMs: 0, durationMs: null, volume: 1 };

/** 要素から読む。 */
export function readMediaState(el: MediaLike): MediaState {
  return {
    paused: el.paused,
    ended: el.ended,
    currentMs: Number.isFinite(el.currentTime) ? el.currentTime * 1000 : 0,
    durationMs: Number.isFinite(el.duration) && el.duration > 0 ? el.duration * 1000 : null,
    volume: el.volume,
  };
}

/** 同じ状態か。 */
export function sameMediaState(a: MediaState, b: MediaState): boolean {
  return (
    a.paused === b.paused &&
    a.ended === b.ended &&
    a.currentMs === b.currentMs &&
    a.durationMs === b.durationMs &&
    a.volume === b.volume
  );
}

const EVENTS = [
  'play',
  'playing',
  'pause',
  'ended',
  'durationchange',
  'loadedmetadata',
  'volumechange',
  'seeking',
  'seeked',
  'emptied',
  'timeupdate',
] as const;

const states = new WeakMap<HTMLMediaElement, MediaState>();

/** 要素の今の状態（変わっていなければ前と同じオブジェクト）。 */
function refresh(el: HTMLMediaElement): MediaState {
  const next = readMediaState(el);
  const prev = states.get(el);
  if (prev && sameMediaState(prev, next)) {
    return prev;
  }
  states.set(el, next);
  return next;
}

function subscribe(el: HTMLMediaElement, notify: () => void): () => void {
  let frame: number | null = null;
  const update = () => {
    const before = states.get(el);
    if (refresh(el) !== before) {
      notify();
    }
  };
  const loop = () => {
    frame = null;
    update();
    if (!el.paused) {
      frame = requestAnimationFrame(loop);
    }
  };
  const onEvent = () => {
    update();
    if (!el.paused && frame === null && typeof requestAnimationFrame === 'function') {
      frame = requestAnimationFrame(loop);
    }
  };
  EVENTS.forEach((name) => el.addEventListener(name, onEvent));
  onEvent();
  return () => {
    EVENTS.forEach((name) => el.removeEventListener(name, onEvent));
    if (frame !== null) {
      cancelAnimationFrame(frame);
    }
  };
}

/** 要素の状態（要素がなければ止まった空の状態）。 */
export function useMediaElement(el: HTMLMediaElement | null): MediaState {
  const sub = useCallback(
    (notify: () => void) => (el ? subscribe(el, notify) : () => undefined),
    [el],
  );
  const get = useCallback(() => {
    if (!el) {
      return EMPTY;
    }
    return states.get(el) ?? refresh(el);
  }, [el]);
  return useSyncExternalStore(sub, get, get);
}
