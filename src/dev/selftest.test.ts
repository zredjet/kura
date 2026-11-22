import { describe, expect, it } from 'vitest';

import { type Check, type Violation, MANIFEST_HASH, isExpected, judge } from './selftest';

const v = (during: string, directive = 'frame-src'): Violation => ({
  directive,
  blocked: 'kura://localhost/file/x',
  source: '',
  during,
});

describe('selftest の判定', () => {
  it('目録の名前は kura-devtools と同じ 64 桁の 16 進', () => {
    expect(MANIFEST_HASH).toMatch(/^[0-9a-f]{64}$/);
    expect(MANIFEST_HASH.startsWith('5e1f7e57')).toBe(true);
  });

  it('止まるべきもの（F）の最中の違反だけを予期したものとする', () => {
    expect(isExpected(v('F1'))).toBe(true);
    expect(isExpected(v('F4'))).toBe(true);
    expect(isExpected(v('F4-nav'))).toBe(true);
    expect(isExpected(v('P7', 'img-src'))).toBe(false);
    expect(isExpected(v('early'))).toBe(false);
    expect(isExpected(v('early-report'))).toBe(false);
    expect(isExpected(v('after'))).toBe(false);
  });

  it('外れた試しか予期しない違反があれば不合格、見送りは数えない', () => {
    const ok: Check = { ok: true, detail: null };
    const skipped: Check = { ok: null, detail: 'F2 は手で' };
    const failed: Check = { ok: false, detail: 'img error' };

    const pass = judge({ P1: ok, F2: skipped }, [v('F4')]);
    expect(pass.pass).toBe(true);
    expect(pass.skipped).toEqual(['F2']);
    expect(pass.expected).toHaveLength(1);

    const broken = judge({ P1: ok, P4: failed }, []);
    expect(broken.pass).toBe(false);
    expect(broken.failed).toEqual(['P4']);

    const noisy = judge({ P1: ok }, [v('P2', 'connect-src'), v('F1')]);
    expect(noisy.pass).toBe(false);
    expect(noisy.unexpected.map((x) => x.during)).toEqual(['P2']);
  });
});
