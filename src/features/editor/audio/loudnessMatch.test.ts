import { describe, expect, it } from 'vitest';

import { dbToGain, matchGains } from './loudnessMatch';

describe('音量を合わせて聞く（M17 段 6）', () => {
  it('いちばん小さい音に合わせて大きいほうだけを下げる（上げない）', () => {
    // 編集後が 6 dB 大きい：編集後を下げる
    expect(matchGains({ input: -20, output: -14, audition: null }, false)).toEqual({
      source: 0,
      edited: -6,
    });
    // 元のほうが大きい：元を下げる
    expect(matchGains({ input: -10, output: -16, audition: null }, false)).toEqual({
      source: -6,
      edited: 0,
    });
  });

  it('外して聞いている間は外した音も比べ、鳴らす編集後の音は外した音', () => {
    expect(matchGains({ input: -20, output: -14, audition: -23 }, true)).toEqual({
      source: -3,
      edited: 0,
    });
    // 外していなければ外した音は比べない
    expect(matchGains({ input: -20, output: -14, audition: -23 }, false)).toEqual({
      source: 0,
      edited: -6,
    });
  });

  it('比べる音が 2 つそろうまでは合わせない・計れない音は外して下げない', () => {
    expect(matchGains({ input: null, output: -14, audition: null }, false)).toEqual({
      source: 0,
      edited: 0,
    });
    expect(matchGains({ input: -20, output: null, audition: -12 }, true)).toEqual({
      source: 0,
      edited: -8,
    });
    expect(dbToGain(0)).toBe(1);
    expect(dbToGain(-6)).toBeCloseTo(0.501_187, 5);
  });
});
