import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import { opName } from '../editor/opsCatalog';
import {
  bindingsFrom,
  bindSlots,
  choicesOf,
  defaultKeep,
  defaultRecipeName,
  defaultSeconds,
  formatLabel,
  includedByDefault,
  outputFormat,
  parseSeconds,
  recipeMedia,
  recipeRows,
  type RecipeNodeLike,
  secondsText,
  slotActive,
  sourceLabel,
} from './recipeModel';

const flip: RecipeNodeLike = { id: 1, op: 'image.flip', v: 1, params: { axis: 'vertical' } };
const grid: RecipeNodeLike = {
  id: 2,
  op: 'meta.set_grid',
  v: 1,
  params: { grid: { cellW: 16, cellH: 16, gapX: 0, gapY: 0, offsetX: 0, offsetY: 0 } },
};
const paint: RecipeNodeLike = { id: 3, op: 'paint.actions', v: 1, params: { actions: [] } };
const off: RecipeNodeLike = { id: 4, op: 'image.rotate90', v: 1, params: {}, enabled: false };
const loop: RecipeNodeLike = {
  id: 5,
  op: 'audio.set_loop',
  v: 1,
  params: { loop: { start: 44100, end: 88200 } },
};
const trim: RecipeNodeLike = { id: 6, op: 'audio.trim', v: 1, params: { start: 22050, end: null } };

describe('recipeModel', () => {
  it('音声の操作があれば音声のレシピ', () => {
    expect(recipeMedia(['image.flip', 'meta.set_grid'])).toBe('image');
    expect(recipeMedia(['audio.trim'])).toBe('audio');
    expect(recipeMedia([])).toBe('image');
  });

  it('含める操作の既定は、作るときは無効・描画・パレットの置き換えを外し、見本で編集では全部含める', () => {
    expect(defaultKeep([flip, grid, paint, off])).toEqual([1, 2]);
    expect(defaultKeep([{ id: 9, op: 'palette.replace', v: 1 }])).toEqual([]);
    expect(defaultKeep([flip, grid, paint, off], true)).toEqual([1, 2, 3, 4]);
    expect(includedByDefault(paint)).toBe(false);
    expect(includedByDefault(paint, true)).toBe(true);
  });

  it('名前の既定は操作の名前を 3 つまで → でつなぐ', () => {
    const name = opName;
    expect(defaultRecipeName(['image.flip', 'meta.set_grid'])).toBe(
      `${name('image.flip')} → ${name('meta.set_grid')}`,
    );
    expect(defaultRecipeName(['image.flip', 'image.flip', 'image.flip', 'image.flip'])).toMatch(
      / …$/,
    );
  });

  it('操作の行は名前・要約・無効の印・アイテムごとに決まる値', () => {
    const rows = recipeRows(
      [flip, grid, off],
      [{ node: 2, param: 'grid', part: null, from: { type: 'setting', key: 'grid' } }],
    );
    expect(rows.map((r) => r.op)).toEqual(['image.flip', 'meta.set_grid', 'image.rotate90']);
    expect(rows[0]?.summary).not.toBe('');
    expect(rows[2]?.enabled).toBe(false);
    expect(rows[1]?.bound).toHaveLength(1);
    expect(rows[1]?.bound[0]).toContain(ja.recipes.from.grid);
    expect(rows[0]?.bound).toEqual([]);
  });

  it('出力の形式は指定したときだけ', () => {
    expect(outputFormat([{ format: { type: 'keep' } }])).toBeNull();
    expect(outputFormat([{ format: { type: 'to', format: 'png' } }])).toBe('png');
    expect(outputFormat([])).toBeNull();
    expect(formatLabel('png')).toBe('PNG');
    expect(formatLabel('zzz')).toBe('ZZZ');
  });

  it('秒の表記と読み取り', () => {
    expect(secondsText(2500)).toBe('2.5');
    expect(secondsText(1000)).toBe('1');
    expect(parseSeconds('1.25')).toBe(1250);
    expect(parseSeconds(' 0 ')).toBe(0);
    expect(parseSeconds('')).toBeNull();
    expect(parseSeconds('-1')).toBeNull();
    expect(parseSeconds('abc')).toBeNull();
    expect(sourceLabel({ type: 'time', ms: 2000, anchor: 'end' })).toBe(
      ja.recipes.from.timeEnd('2'),
    );
    expect(sourceLabel({ type: 'setting', key: 'pivot' })).toBe(ja.recipes.from.pivot);
  });

  it('結び付けの欄：含めた操作の結べる引数だけ。区間は全体と始め・終わり', () => {
    const slots = bindSlots([flip, grid, loop, trim, off], [1, 2, 5, 6]);
    expect(slots.map((s) => `${s.node}:${s.param}:${s.part ?? ''}`)).toEqual([
      '2:grid:',
      '5:loop:',
      '5:loop:start',
      '5:loop:end',
      '6:start:',
      '6:end:',
    ]);
    const loopWhole = slots.find((s) => s.key === '5:loop:');
    expect(loopWhole?.settings).toEqual(['loop']);
    expect(loopWhole?.time).toBe(false);
    expect(slots.find((s) => s.key === '5:loop:start')?.whole).toBe('5:loop:');
    expect(slots.find((s) => s.key === '6:start:')?.time).toBe(true);
    // 外した操作の引数は出さない
    expect(bindSlots([grid], [])).toEqual([]);
  });

  it('欄の選び方と結び付けを往復する。区間の全体を設定に結べば片方は使わない', () => {
    const slots = bindSlots([grid, loop, trim], [2, 5, 6]);
    const bindings = [
      {
        node: 2,
        param: 'grid',
        part: null,
        from: { type: 'setting' as const, key: 'grid' as const },
      },
      {
        node: 6,
        param: 'start',
        part: null,
        from: { type: 'time' as const, ms: 1500, anchor: 'end' as const },
      },
    ];
    const choices = choicesOf(slots, bindings);
    expect(choices['2:grid:']).toEqual({ type: 'setting', key: 'grid' });
    expect(choices['6:start:']).toEqual({ type: 'time', anchor: 'end', seconds: '1.5' });
    expect(choices['5:loop:']).toEqual({ type: 'keep' });
    expect(bindingsFrom(slots, choices)).toEqual({ bindings, invalid: null });

    const withLoop = {
      ...choices,
      '5:loop:': { type: 'setting' as const, key: 'loop' as const },
      '5:loop:start': { type: 'time' as const, anchor: 'start' as const, seconds: '1' },
    };
    const start = slots.find((s) => s.key === '5:loop:start');
    expect(start && slotActive(start, withLoop)).toBe(false);
    const out = bindingsFrom(slots, withLoop);
    expect(out.bindings.map((b) => `${b.node}:${b.param}:${b.part ?? ''}`)).toEqual([
      '2:grid:',
      '5:loop:',
      '6:start:',
    ]);
  });

  it('秒を読めない欄は見出しを返して結び付けを作らない', () => {
    const slots = bindSlots([trim], [6]);
    const out = bindingsFrom(slots, {
      '6:start:': { type: 'time', anchor: 'start', seconds: 'x' },
    });
    expect(out.bindings).toEqual([]);
    expect(out.invalid).toBe(slots[0]?.label);
  });

  it('秒の既定は見本の値をレートで割ったもの。終わりからは長さから引く（区間は片方の値）', () => {
    const slots = bindSlots([loop, trim], [5, 6]);
    const start = slots.find((s) => s.key === '6:start:');
    const loopEnd = slots.find((s) => s.key === '5:loop:end');
    expect(start && defaultSeconds(trim, start, 'start', 44100, null)).toBe('0.5');
    expect(loopEnd && defaultSeconds(loop, loopEnd, 'start', 44100, null)).toBe('2');
    expect(start && defaultSeconds(trim, start, 'start', null, null)).toBe('0');
    // 終わりから：(長さ − 値) / レート。長さが分からなければ 0
    expect(start && defaultSeconds(trim, start, 'end', 44100, 88200)).toBe('1.5');
    expect(loopEnd && defaultSeconds(loop, loopEnd, 'end', 44100, 132300)).toBe('1');
    expect(start && defaultSeconds(trim, start, 'end', 44100, null)).toBe('0');
  });

  it('見本の区間がない（ループを消す null）なら、始め・終わりの秒の欄は出さない', () => {
    const cleared: RecipeNodeLike = { ...loop, params: { loop: null } };
    const slots = bindSlots([cleared], [5]);
    expect(slots.map((s) => s.key)).toEqual(['5:loop:']);
  });
});
