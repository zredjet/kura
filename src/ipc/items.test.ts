import { describe, expect, it } from 'vitest';

import { parseIdList, utcOffsetMinutes } from './items';

function bytes(ids: number[], aspects: number[], groups: [number, number][]): ArrayBuffer {
  const size = 4 + ids.length * 6 + 4 + groups.length * 8;
  const view = new DataView(new ArrayBuffer(size));
  let o = 0;
  view.setUint32(o, ids.length, true);
  o += 4;
  ids.forEach((id) => {
    view.setUint32(o, id, true);
    o += 4;
  });
  aspects.forEach((a) => {
    view.setUint16(o, a, true);
    o += 2;
  });
  view.setUint32(o, groups.length, true);
  o += 4;
  groups.forEach(([start]) => {
    view.setUint32(o, start, true);
    o += 4;
  });
  groups.forEach(([, day]) => {
    view.setInt32(o, day, true);
    o += 4;
  });
  return view.buffer;
}

describe('parseIdList', () => {
  it('Rust の IdList::encode と同じ形を読む', () => {
    const list = parseIdList(
      bytes(
        [3, 2, 1],
        [1000, 1778, 563],
        [
          [0, 20555],
          [1, -1],
        ],
      ),
    );
    expect(Array.from(list.ids)).toEqual([3, 2, 1]);
    expect(Array.from(list.aspects)).toEqual([1000, 1778, 563]);
    expect(list.groups).toEqual([
      { start: 0, day: 20555 },
      { start: 1, day: -1 },
    ]);
  });

  it('空の一覧', () => {
    const list = parseIdList(bytes([], [], []));
    expect(list.ids.length).toBe(0);
    expect(list.groups).toEqual([]);
  });

  it('時差は分（東が正）', () => {
    expect(typeof utcOffsetMinutes()).toBe('number');
  });
});
