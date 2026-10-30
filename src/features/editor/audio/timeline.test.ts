import { describe, expect, it } from 'vitest';

import type { EditSourcePieceDto } from '../../../ipc/edit';
import { fromSource, toSource } from './timeline';

describe('出力と元のフレームの写し', () => {
  // 元の 1000 から 5000 フレームを切り出し（trim）、48k → 24k に変えた出力（2500 フレーム）
  const map: EditSourcePieceDto[] = [
    { outStart: 0, outEnd: 2500, inStart: 1000, inRate: 48_000, outRate: 24_000 },
  ];

  it('区分の中は元の頭からレートの比で写す（丸める）', () => {
    expect(toSource(0, map)).toBe(1000);
    expect(toSource(1, map)).toBe(1002);
    expect(toSource(2499, map)).toBe(1000 + 4998);
    expect(toSource(2500, map)).toBeNull();
    expect(toSource(-1, map)).toBeNull();
  });

  it('元から出力へ戻す（区分の外・写しがなければ null）', () => {
    expect(fromSource(1000, map)).toBe(0);
    expect(fromSource(1002, map)).toBe(1);
    expect(fromSource(1003, map)).toBe(2);
    expect(fromSource(999, map)).toBeNull();
    expect(fromSource(6000, map)).toBeNull();
    expect(fromSource(10, null)).toBeNull();
    expect(toSource(10, null)).toBeNull();
  });

  it('区分が 2 つ：それぞれの区分で写す', () => {
    const two: EditSourcePieceDto[] = [
      { outStart: 0, outEnd: 100, inStart: 0, inRate: 8000, outRate: 8000 },
      { outStart: 200, outEnd: 300, inStart: 100, inRate: 8000, outRate: 8000 },
    ];
    expect(toSource(150, two)).toBeNull();
    expect(toSource(250, two)).toBe(150);
    expect(fromSource(150, two)).toBe(250);
    expect(fromSource(50, two)).toBe(50);
  });
});
