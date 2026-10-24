import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import { EDIT_CATALOG, type EditNodeDto, type ParamSpec } from '../../ipc/edit';
import {
  addableOps,
  controlFor,
  defaultParams,
  hasLaterPaint,
  isParamVisible,
  makeColor,
  makeLength,
  opCategory,
  opSpec,
  readColor,
  readLength,
  summarizeNode,
  visibleParams,
} from './opsCatalog';

const t = ja.editor;

function spec(id: string): NonNullable<ReturnType<typeof opSpec>> {
  const d = opSpec(id);
  if (!d) throw new Error(`カタログに ${id} がない`);
  return d;
}

function param(id: string, key: string): ParamSpec {
  const p = spec(id).schema.params.find((x) => x.key === key);
  if (!p) throw new Error(`${id} に ${key} がない`);
  return p;
}

function node(
  id: number,
  op: string,
  params: Record<string, unknown>,
  over: Partial<EditNodeDto> = {},
) {
  return { id, op, v: 1, enabled: true, params, log: null, ...over } as EditNodeDto;
}

describe('引数の欄の種類', () => {
  it('列挙は 4 個以下ならセグメント、多ければ選択。数・真偽・矩形・長さ・色・格子・描いたもの', () => {
    expect(controlFor(param('image.flip', 'axis'))).toBe('segmented');
    expect(controlFor(param('image.resize', 'filter'))).toBe('segmented');
    expect(controlFor(param('image.convert', 'format'))).toBe('select');
    expect(controlFor(param('image.convert', 'colors'))).toBe('number');
    expect(controlFor(param('sprite.split_grid', 'skipEmpty'))).toBe('switch');
    expect(controlFor(param('image.crop', 'rect'))).toBe('rect');
    expect(controlFor(param('image.resize', 'width'))).toBe('length');
    expect(controlFor(param('image.convert', 'matte'))).toBe('color');
    expect(controlFor(param('sprite.split_grid', 'grid'))).toBe('grid');
    expect(controlFor(param('meta.set_pivot', 'pivot'))).toBe('point');
    expect(controlFor(param('paint.actions', 'actions'))).toBe('count');
    expect(controlFor(param('palette.replace', 'entries'))).toBe('count');
  });
});

describe('表示の条件', () => {
  const convert = spec('image.convert');
  const shown = (format: string) =>
    visibleParams(convert, {
      format,
      colors: 256,
      dither: 'none',
      matte: [255, 255, 255],
    }).basic.map((p) => p.key);

  it('色の数・ディザはパレット（indexed8）のときだけ、下地の色はアルファを外す形式のときだけ', () => {
    expect(shown('indexed8')).toEqual(['format', 'colors', 'dither']);
    expect(shown('rgba8')).toEqual(['format']);
    expect(shown('rgb8')).toEqual(['format', 'matte']);
    expect(shown('gray16')).toEqual(['format', 'matte']);
  });

  it('条件の引数がノードになければ既定値で決める（既定は rgba8）', () => {
    expect(isParamVisible(param('image.convert', 'colors'), {}, convert.schema.params)).toBe(false);
    expect(isParamVisible(param('image.convert', 'format'), {}, convert.schema.params)).toBe(true);
  });

  it('カタログの表示の条件は、どれも同じ操作の列挙の引数を指す', () => {
    for (const op of EDIT_CATALOG) {
      for (const p of op.schema.params) {
        if (!p.visible) continue;
        const target = op.schema.params.find((x) => x.key === p.visible?.key);
        expect(target?.kind.type, `${op.id}.${p.key}`).toBe('enum');
      }
    }
  });
});

describe('足す操作', () => {
  it('分類ごとに並べ、音声の操作は出さない', () => {
    const groups = addableOps();
    const ids = groups.flatMap((g) => g.ops.map((o) => o.id));
    expect(ids).toContain('image.crop');
    expect(ids).toContain('sprite.split_grid');
    expect(ids.some((id) => id.startsWith('audio.'))).toBe(false);
    expect(groups.map((g) => g.category)).toEqual([
      'geometry',
      'convert',
      'sprite',
      'paint',
      'meta',
    ]);
    expect(opCategory('palette.replace')).toBe('convert');
    expect(opCategory('image.rotate90')).toBe('geometry');
  });

  it('既定の引数：切り抜き・キャンバスは今の出力の全体、大きさを変えるは今の幅', () => {
    const size = { w: 64, h: 32 };
    expect(defaultParams(spec('image.crop'), size)).toEqual({ rect: { x: 0, y: 0, w: 64, h: 32 } });
    expect(defaultParams(spec('image.canvas'), size)).toEqual({
      rect: { x: 0, y: 0, w: 64, h: 32 },
      fill: null,
    });
    expect(defaultParams(spec('image.resize'), size)).toEqual({
      width: { px: 64 },
      height: 'auto',
      filter: 'nearest',
    });
    // 大きさが分からなければカタログの既定値
    expect(defaultParams(spec('image.flip'), null)).toEqual({ axis: 'horizontal' });
  });
});

describe('値の形', () => {
  it('長さは px・%・自動', () => {
    expect(readLength({ px: 32 })).toEqual({ mode: 'px', n: 32 });
    expect(readLength({ scale: { num: 1, den: 2 } })).toEqual({ mode: 'percent', n: 50 });
    expect(readLength('auto')).toEqual({ mode: 'auto', n: 0 });
    expect(makeLength('px', 12.4)).toEqual({ px: 12 });
    expect(makeLength('percent', 50)).toEqual({ scale: { num: 50, den: 100 } });
    expect(makeLength('percent', 33.33)).toEqual({ scale: { num: 3333, den: 10_000 } });
    expect(makeLength('auto', 1)).toBe('auto');
  });

  it('色は 3 つ（アルファなし）か 4 つ', () => {
    expect(readColor([255, 0, 0])).toEqual([255, 0, 0, 255]);
    expect(readColor([1, 2, 3, 4])).toEqual([1, 2, 3, 4]);
    expect(readColor(null)).toBeNull();
    expect(makeColor([1, 2, 3, 4], false)).toEqual([1, 2, 3]);
    expect(makeColor([1, 2, 3, 4], true)).toEqual([1, 2, 3, 4]);
  });
});

describe('スタックの行の要約', () => {
  it('描画は本数、ほかはふだんの引数を 2 つまで', () => {
    expect(
      summarizeNode(
        node(1, 'paint.actions', { actions: [] }, { log: { field: 'actions', count: 12 } }),
      ),
    ).toBe(t.strokeCount('12'));
    expect(summarizeNode(node(2, 'image.crop', { rect: { x: 1, y: 2, w: 30, h: 20 } }))).toBe(
      t.size('30', '20'),
    );
    expect(summarizeNode(node(3, 'image.flip', { axis: 'vertical' }))).toBe('上下');
    expect(
      summarizeNode(
        node(4, 'image.convert', {
          format: 'indexed8',
          colors: 16,
          dither: 'ordered',
          matte: [0, 0, 0],
        }),
      ),
    ).toBe('パレット（256 色まで） · 16');
  });
});

describe('後ろの描画', () => {
  it('選んだ操作より後ろに描画があれば真', () => {
    const doc = {
      order: [1, 2, 3],
      nodes: {
        1: node(1, 'image.crop', {}),
        2: node(2, 'paint.actions', {}),
        3: node(3, 'image.flip', {}),
      },
      outputs: [],
    };
    expect(hasLaterPaint(doc, 1)).toBe(true);
    expect(hasLaterPaint(doc, 3)).toBe(false);
    expect(hasLaterPaint(doc, 9)).toBe(false);
  });
});
