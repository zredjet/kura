import type { SeekTarget } from '../../lib/seekKeys';

/**
 * クイックルックの `<video>`・`<audio>` を動かす小さな関数と、シークのキーの当て先（ADR-230）。
 *
 * - カプセル（`QuickLookControls`）が付いている間だけ当て先を登録し、ライブラリのキー（`handleLibraryKey`）が
 *   `quickLookSeekTarget()` で取り出す（, . < > J K L）。
 * - ドラッグ中は止めて、離したら（ドラッグの前に鳴っていれば）続きから鳴らす。
 */

let current: SeekTarget | null = null;

/** シークのキーの当て先を登録する。戻り値で外す。 */
export function registerQuickLookMedia(target: SeekTarget): () => void {
  current = target;
  return () => {
    if (current === target) {
      current = null;
    }
  };
}

/** 今のクイックルックの動画・音声（なければ null）。 */
export function quickLookSeekTarget(): SeekTarget | null {
  return current;
}

/** 鳴らす（自動再生を断られても何もしない）。 */
export function playMedia(el: HTMLMediaElement): void {
  try {
    const result = el.play() as Promise<void> | undefined;
    result?.catch(() => undefined);
  } catch {
    // 鳴らせない（読み込みに失敗した要素など）
  }
}

/** 再生／一時停止。 */
export function toggleMedia(el: HTMLMediaElement): void {
  if (el.paused || el.ended) {
    playMedia(el);
  } else {
    el.pause();
  }
}

/** 位置を変える（ms。先は呼ぶ側が収める）。 */
export function seekMedia(el: HTMLMediaElement, ms: number): void {
  el.currentTime = Math.max(0, ms) / 1000;
}

/** 音量（0〜100。プレーヤーと同じ設定）を当てる。 */
export function applyVolume(el: HTMLMediaElement, volume: number): void {
  el.volume = Math.min(1, Math.max(0, volume / 100));
}

const resume = new WeakMap<HTMLMediaElement, boolean>();

/** ドラッグを始める：止める（鳴っていたかを覚える）。 */
export function beginMediaScrub(el: HTMLMediaElement): void {
  resume.set(el, !el.paused);
  el.pause();
}

/** ドラッグを終える：鳴っていたなら続きから鳴らす。 */
export function endMediaScrub(el: HTMLMediaElement): void {
  if (resume.get(el) === true) {
    playMedia(el);
  }
  resume.delete(el);
}

/** 要素の長さ（ms。分からなければ null）。 */
export function mediaDurationMs(el: HTMLMediaElement): number | null {
  return Number.isFinite(el.duration) && el.duration > 0 ? el.duration * 1000 : null;
}

/** 要素をシークのキーの当て先にする（1 コマは `frameMs`。動画でなければ null を返す関数）。 */
export function mediaSeekTarget(el: HTMLMediaElement, frameMs: () => number | null): SeekTarget {
  return {
    positionMs: () => (Number.isFinite(el.currentTime) ? el.currentTime * 1000 : 0),
    endMs: () => mediaDurationMs(el),
    seek: (ms) => seekMedia(el, ms),
    pause: () => el.pause(),
    togglePlay: () => toggleMedia(el),
    frameMs,
  };
}
