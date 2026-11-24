import { type PointerEvent, useEffect, useRef, useState } from 'react';

import { clampSeek, moveScrub, type ScrubAnchor, startScrub } from '../../lib/seek';

/**
 * 押してドラッグするシーク（ADR-230。シークバーと波形で共有する）。
 *
 * - 押したら（左ボタン）ポインタを捕まえ、⌥ なしならポインタの下の時刻へすぐ飛ぶ。⌥ ありは今の位置のまま、そこから
 *   1/10 の速さで動く（`src/lib/seek.ts` の錨の方式）。
 * - 動かしている間の `onSeek` は 1 フレームに 1 回にまとめ、離したときはすぐ送る。飛び先は終わりの手前で止める
 *   （`clampSeek`）。
 * - `onScrubStart`・`onScrubEnd` は押したとき・離したとき（再生を止めて戻すのは呼ぶ側）。
 * - `resetKey`（アイテム）が替わったら、そのドラッグは無かったことにする（あとの動き・離すのを無視する）。そのときと、
 *   ドラッグ中に外れたときも `onScrubEnd` は呼ぶ（止めた再生を戻す・ドラッグ中の表示を戻すため）。
 * - ⌃クリック（macOS の副クリック）・左以外のボタンでは始めない。
 * - `trackHover` なら、押していない間もポインタの位置を返す（シークバーの吹き出し）。
 */

export interface ScrubOptions {
  durationMs: number | null;
  /** 今の位置（⌥ で押したときの始まり）。 */
  positionMs: number;
  onSeek: (ms: number) => void;
  onScrubStart?: () => void;
  onScrubEnd?: () => void;
  resetKey?: unknown;
  disabled?: boolean;
  trackHover?: boolean;
}

export interface ScrubPointer {
  /** 要素の左からの x（px）。 */
  x: number;
  /** 要素の幅（px）。 */
  width: number;
}

export interface ScrubHandlers {
  onPointerDown: (e: PointerEvent<HTMLElement>) => void;
  onPointerMove: (e: PointerEvent<HTMLElement>) => void;
  onPointerUp: (e: PointerEvent<HTMLElement>) => void;
  onPointerCancel: (e: PointerEvent<HTMLElement>) => void;
  onLostPointerCapture: (e: PointerEvent<HTMLElement>) => void;
  onPointerLeave: () => void;
}

export interface ScrubState {
  /** ドラッグ中の位置（ms。ドラッグしていなければ null）。 */
  scrubMs: number | null;
  /** ポインタの位置（`trackHover` のときだけ。外にいれば null）。 */
  hover: ScrubPointer | null;
  handlers: ScrubHandlers;
}

interface Drag {
  pointerId: number;
  anchor: ScrubAnchor;
  left: number;
  width: number;
  key: unknown;
  ms: number;
}

/** まとめていたシークを取り消す。 */
function cancelFrame(frame: { current: number | null }): void {
  if (frame.current !== null && typeof cancelAnimationFrame === 'function') {
    cancelAnimationFrame(frame.current);
  }
  frame.current = null;
}

/** 1 フレームに 1 回にまとめる口（rAF がなければすぐ）。 */
function schedule(cb: () => void): number | null {
  if (typeof requestAnimationFrame === 'function') {
    return requestAnimationFrame(cb);
  }
  cb();
  return null;
}

export function useScrub({
  durationMs,
  positionMs,
  onSeek,
  onScrubStart,
  onScrubEnd,
  resetKey = null,
  disabled = false,
  trackHover = false,
}: ScrubOptions): ScrubState {
  const drag = useRef<Drag | null>(null);
  const frame = useRef<number | null>(null);
  const latest = useRef({ onSeek, onScrubStart, onScrubEnd, durationMs, resetKey });
  useEffect(() => {
    latest.current = { onSeek, onScrubStart, onScrubEnd, durationMs, resetKey };
  });
  // 画面に出すドラッグの位置（アイテムが替わったら出さない）
  const [shown, setShown] = useState<{ key: unknown; ms: number } | null>(null);
  const [hover, setHover] = useState<ScrubPointer | null>(null);
  const length = durationMs !== null && durationMs > 0 ? durationMs : null;
  const enabled = !disabled && length !== null;

  // アイテムが替わった：そのドラッグを捨て、終わりだけ知らせる
  useEffect(() => {
    const d = drag.current;
    if (d && d.key !== resetKey) {
      drag.current = null;
      cancelFrame(frame);
      latest.current.onScrubEnd?.();
    }
  }, [resetKey]);

  // 外れた：まとめていたシークを取り消し、ドラッグ中なら終わりを知らせる
  useEffect(
    () => () => {
      cancelFrame(frame);
      if (drag.current) {
        drag.current = null;
        latest.current.onScrubEnd?.();
      }
    },
    [],
  );

  const flush = () => cancelFrame(frame);

  const send = (ms: number, now: boolean) => {
    const target = clampSeek(ms, latest.current.durationMs);
    if (now) {
      flush();
      latest.current.onSeek(target);
      return;
    }
    if (frame.current !== null) {
      return;
    }
    frame.current = schedule(() => {
      frame.current = null;
      const d = drag.current;
      if (d && d.key === latest.current.resetKey) {
        latest.current.onSeek(clampSeek(d.ms, latest.current.durationMs));
      }
    });
  };

  const current = (d: Drag | null): Drag | null =>
    d && d.key === latest.current.resetKey ? d : null;

  const finish = (e: PointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d || d.pointerId !== e.pointerId) {
      return;
    }
    drag.current = null;
    setShown(null);
    if (current(d) === null) {
      flush();
      return;
    }
    send(d.ms, true);
    latest.current.onScrubEnd?.();
  };

  const handlers: ScrubHandlers = {
    onPointerDown: (e) => {
      if (!enabled || length === null || e.button !== 0 || e.ctrlKey || drag.current !== null) {
        return;
      }
      // 文字の選択・フォーカスの移動をしない（← → は前後のアイテムのまま）
      e.preventDefault();
      const el = e.currentTarget;
      if (typeof el.setPointerCapture === 'function') {
        el.setPointerCapture(e.pointerId);
      }
      const rect = el.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const anchor = startScrub(x, rect.width, length, positionMs, e.altKey);
      drag.current = {
        pointerId: e.pointerId,
        anchor,
        left: rect.left,
        width: rect.width,
        key: resetKey,
        ms: anchor.ms,
      };
      setShown({ key: resetKey, ms: anchor.ms });
      if (trackHover) {
        setHover({ x, width: rect.width });
      }
      latest.current.onScrubStart?.();
      send(anchor.ms, true);
    },
    onPointerMove: (e) => {
      const d = drag.current;
      if (d && d.pointerId === e.pointerId) {
        if (current(d) === null || length === null) {
          return;
        }
        const step = moveScrub(d.anchor, e.clientX - d.left, d.width, length, e.altKey);
        d.anchor = step.anchor;
        d.ms = step.ms;
        setShown({ key: d.key, ms: step.ms });
        send(step.ms, false);
        return;
      }
      if (trackHover && enabled) {
        const rect = e.currentTarget.getBoundingClientRect();
        setHover({ x: e.clientX - rect.left, width: rect.width });
      }
    },
    onPointerUp: finish,
    onPointerCancel: finish,
    onLostPointerCapture: finish,
    onPointerLeave: () => {
      if (trackHover) {
        setHover(null);
      }
    },
  };

  return {
    scrubMs: shown !== null && shown.key === resetKey ? shown.ms : null,
    hover: enabled ? hover : null,
    handlers,
  };
}
