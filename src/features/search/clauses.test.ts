import { describe, expect, it } from 'vitest';

import { QUERY_FIELD_NAMES } from '../../ipc/bindings';
import {
  clauseParts,
  currentWord,
  fieldTexts,
  findField,
  isComplete,
  joinClauses,
  replaceField,
  singleKind,
} from './clauses';

describe('clauses', () => {
  it('reads fields, exclusions and plain words', () => {
    expect(clauseParts('種類:画像,動画')).toEqual({
      negated: false,
      field: 'type',
      name: '種類',
      op: ':',
      value: '画像,動画',
    });
    expect(clauseParts('-評価＞＝3')).toMatchObject({
      negated: true,
      field: 'rating',
      op: '>=',
      value: '3',
    });
    expect(clauseParts('TYPE=video')).toMatchObject({ field: 'type', op: '=' });
    expect(clauseParts('フォルダ:"写真/2026 旅行"')).toMatchObject({
      field: 'folder',
      value: '写真/2026 旅行',
    });
    // 名前空間付きタグ・語・かっこはフィールドではない
    expect(clauseParts('作者:もりの').field).toBeNull();
    expect(clauseParts('夕景')).toMatchObject({ field: null, value: '夕景', negated: false });
    expect(clauseParts('(海 | 港)').field).toBeNull();
  });

  it('replaces one field and keeps the rest', () => {
    const clauses = ['夕景', '種類:画像', '-種類:音楽', '評価>=3'];
    expect(replaceField(clauses, 'type', '種類:動画')).toEqual([
      '夕景',
      '-種類:音楽',
      '評価>=3',
      '種類:動画',
    ]);
    expect(replaceField(clauses, 'rating', null)).toEqual(['夕景', '種類:画像', '-種類:音楽']);
    expect(findField(clauses, 'rating')?.value).toBe('3');
    expect(replaceField(['長さ<1分', '夕景'], 'dur', ['長さ>=1分', '長さ<5分'])).toEqual([
      '夕景',
      '長さ>=1分',
      '長さ<5分',
    ]);
    expect(fieldTexts(['長さ>=1分', '-長さ<5分', '夕景'], 'dur')).toEqual(['長さ>=1分']);
    expect(joinClauses([' 夕景 ', '', '評価>=3'])).toBe('夕景 評価>=3');
  });

  it('finds a single kind for the segment', () => {
    expect(singleKind(['種類:動画'])).toBe('video');
    expect(singleKind(['type=image'])).toBe('image');
    expect(singleKind(['種類:フォント'])).toBe('font');
    expect(singleKind(['種類:3d'])).toBe('model');
    expect(singleKind(['type:model'])).toBe('model');
    expect(singleKind(['種類:写真'])).toBeNull();
    expect(singleKind(['種類:画像,動画'])).toBeNull();
    expect(singleKind(['-種類:動画'])).toBeNull();
    expect(singleKind(['種類!=動画'])).toBeNull();
    expect(singleKind([])).toBeNull();
  });

  it('reads the asset fields from the Rust table (M11)', () => {
    expect(clauseParts('透過:あり')).toMatchObject({ field: 'alpha', op: ':', value: 'あり' });
    expect(clauseParts('ラウドネス>-14')).toMatchObject({ field: 'lufs', op: '>', value: '-14' });
    expect(clauseParts('２のべき乗:あり').field).toBe('pot');
    expect(clauseParts('LOOP=yes').field).toBe('loop');
    // 記号は Rust の字句解析と同じ表で直す（全角の「：」、U+2212 の「−」の除外）
    expect(clauseParts('透過：あり').field).toBe('alpha');
    expect(clauseParts('−透過:あり')).toMatchObject({ negated: true, field: 'alpha' });
    // 引用符の中は直さない（Rust の字句解析と同じ）
    expect(clauseParts('フォルダ:"写真、旅行"').value).toBe('写真、旅行');
    expect(clauseParts('名前:“Ａ：Ｂ”').value).toBe('Ａ：Ｂ');
    // 表のすべての名前がフィールドとして読める
    for (const { field, names } of QUERY_FIELD_NAMES) {
      for (const name of names) {
        expect(clauseParts(`${name}:x`).field, name).toBe(field);
        expect(clauseParts(`-${name.toUpperCase()}>1`).field, name).toBe(field);
      }
    }
  });

  it('splits kinds at commas the same way as Rust', () => {
    // 読点「、」・全角の「，」も「,」と同じ（Rust の字句解析の表）
    expect(singleKind(['種類:画像、'])).toBe('image');
    expect(singleKind(['種類:画像、動画'])).toBeNull();
    expect(singleKind(['種類:音楽，フォント'])).toBeNull();
    expect(singleKind(['種類：フォント'])).toBe('font');
  });

  it('knows when typing can become a chip', () => {
    expect(isComplete('夕景')).toBe(true);
    expect(isComplete('"駅前 夜')).toBe(false);
    expect(isComplete('"駅前 夜"')).toBe(true);
    expect(isComplete('(海 | 港')).toBe(false);
    expect(isComplete('作者:')).toBe(false);
    expect(isComplete('海 |')).toBe(false);
    expect(isComplete('')).toBe(false);
  });

  it('finds the word being typed', () => {
    expect(currentWord('夕景 作者:も')).toEqual({ start: 3, word: '作者:も' });
    expect(currentWord('名前:"IMG 0')).toEqual({ start: 0, word: '名前:"IMG 0' });
    expect(currentWord('(海|港')).toEqual({ start: 3, word: '港' });
    expect(currentWord('')).toEqual({ start: 0, word: '' });
  });
});
