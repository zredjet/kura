import { describe, expect, it } from 'vitest';

import { isImeKey } from './ime';

describe('isImeKey', () => {
  it('変換中、または変換を確定する Enter（keyCode 229）なら true', () => {
    expect(isImeKey({ isComposing: true, keyCode: 13 })).toBe(true);
    // WebKit：確定の Enter は compositionend のあとに isComposing = false で届く
    expect(isImeKey({ isComposing: false, keyCode: 229 })).toBe(true);
    expect(isImeKey({ isComposing: false, keyCode: 13 })).toBe(false);
  });

  it('React のイベントは nativeEvent を見る', () => {
    expect(isImeKey({ nativeEvent: { isComposing: false, keyCode: 229 } })).toBe(true);
    expect(isImeKey({ nativeEvent: { isComposing: false, keyCode: 13 } })).toBe(false);
  });
});
