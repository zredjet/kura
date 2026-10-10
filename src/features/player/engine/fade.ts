import type { Clock } from './types';

/** BGM のフェードの長さ（docs/playlists.md §6.2）。 */
export const FADE_MS = 300;
/** フェードで音量を変える間隔。 */
const STEP_MS = 16;

/**
 * 音量の倍率（0〜1）を時間をかけて変える。新しいフェードを始めると前のものは止まる。
 * `apply` で要素の音量（全体の音量 × 倍率）に反映する。
 */
export class Fader {
  gain = 1;
  private timer: number | null = null;

  constructor(
    private readonly clock: Clock,
    private readonly apply: (gain: number) => void,
  ) {}

  /** すぐに倍率を決める（進行中のフェードは止める）。 */
  set(gain: number): void {
    this.cancel();
    this.gain = gain;
    this.apply(gain);
  }

  /** `ms` かけて `to` まで変え、終わったら `done` を呼ぶ。 */
  fade(to: number, ms: number, done?: () => void): void {
    this.cancel();
    const from = this.gain;
    const start = this.clock.now();
    const step = () => {
      const t = ms <= 0 ? 1 : Math.min(1, (this.clock.now() - start) / ms);
      this.gain = from + (to - from) * t;
      this.apply(this.gain);
      if (t >= 1) {
        this.timer = null;
        done?.();
      } else {
        this.timer = this.clock.setTimeout(step, STEP_MS);
      }
    };
    step();
  }

  /** 進行中のフェードを止める（倍率はその時点のまま）。 */
  cancel(): void {
    if (this.timer !== null) {
      this.clock.clearTimeout(this.timer);
      this.timer = null;
    }
  }
}
