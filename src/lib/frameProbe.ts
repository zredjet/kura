import { estimateFrameMs } from './frameRate';

/**
 * 再生中の動画のコマの時刻を `requestVideoFrameCallback` で集め、1 コマの長さを推定する（ADR-230。コマ送りのキー）。
 *
 * - `presentedFrames` がちょうど 1 つ進んだ組の `mediaTime` の差だけを使う（落ちたコマ・シークをまたいだ組は捨てる）。
 * - シーク（`seeking`）で組を切り、別のファイルを読み始めたら（`loadstart`。シアターは 1 つの `<video>` を使い回す）
 *   集め直す。`MAX_SAMPLES` 個集まったら止める。
 * - `requestVideoFrameCallback` がない（jsdom・古い WebKit）ときは何もせず、`frameMs()` は null。
 */

export interface FrameWatch {
  /** 推定した 1 コマの長さ（ms。分からなければ null）。 */
  frameMs(): number | null;
  /** 集めるのをやめ、イベントを外す。 */
  stop(): void;
}

/** 集める差の数の上限。 */
const MAX_SAMPLES = 30;

const NONE: FrameWatch = { frameMs: () => null, stop: () => undefined };

export function watchFrames(video: HTMLVideoElement): FrameWatch {
  if (typeof video.requestVideoFrameCallback !== 'function') {
    return NONE;
  }
  let deltas: number[] = [];
  let last: { frames: number; time: number } | null = null;
  let estimate: number | null = null;
  let pending: number | null = null;
  let stopped = false;

  const request = () => {
    if (!stopped && pending === null) {
      pending = video.requestVideoFrameCallback(onFrame);
    }
  };
  const onFrame = (_now: number, meta: VideoFrameCallbackMetadata) => {
    pending = null;
    if (last !== null && meta.presentedFrames === last.frames + 1) {
      const delta = (meta.mediaTime - last.time) * 1000;
      if (delta > 0) {
        deltas.push(delta);
      }
    }
    last = { frames: meta.presentedFrames, time: meta.mediaTime };
    if (deltas.length >= MAX_SAMPLES) {
      estimate = estimateFrameMs(deltas);
      return;
    }
    request();
  };
  const cut = () => {
    last = null;
  };
  const restart = () => {
    deltas = [];
    last = null;
    estimate = null;
    request();
  };
  video.addEventListener('seeking', cut);
  video.addEventListener('loadstart', restart);
  request();

  return {
    frameMs: () => estimate ?? estimateFrameMs(deltas),
    stop: () => {
      stopped = true;
      video.removeEventListener('seeking', cut);
      video.removeEventListener('loadstart', restart);
      if (pending !== null) {
        video.cancelVideoFrameCallback(pending);
        pending = null;
      }
    },
  };
}
