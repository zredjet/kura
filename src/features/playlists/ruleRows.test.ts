import { describe, expect, it } from 'vitest';

import { fromClause, newRow, type RuleRow, toClause } from './ruleRows';

const rows: [RuleRow, string][] = [
  [{ field: 'rating', op: 'ge', value: 4 }, '評価>=4'],
  [{ field: 'rating', op: 'eq', value: 0 }, '評価=0'],
  [{ field: 'kind', op: 'has', value: ['image', 'video'] }, '種類:画像,動画'],
  [{ field: 'kind', op: 'only', value: ['video'] }, '種類=動画'],
  [{ field: 'tags', op: 'all', value: ['夕景', '作者:もりの'] }, '夕景 作者:もりの'],
  [{ field: 'tags', op: 'any', value: ['海', '夜 景'] }, '(海 | "夜 景")'],
  [{ field: 'tags', op: 'none', value: ['未整理', 'ラフ'] }, '-未整理 -ラフ'],
  [{ field: 'duration', op: 'gt', minutes: 20 }, '長さ>20分'],
  [{ field: 'played', op: 'before', days: 7 }, '(再生:7日以上前 | 再生:未)'],
  [{ field: 'played', op: 'within', days: 3 }, '再生:3日以内'],
  [{ field: 'added', op: 'within', days: 30 }, '追加:30日以内'],
  [{ field: 'folder', op: 'out', path: '写真/2026 旅行' }, '-フォルダ:"写真/2026 旅行"'],
  [{ field: 'name', op: 'has', text: 'IMG_' }, '名前:IMG_'],
  [{ field: 'query', text: '撮影:2026-10 向き:縦' }, '撮影:2026-10 向き:縦'],
];

describe('ルールの行と検索の文字列', () => {
  it.each(rows)('%o ⇄ %s', (row, clause) => {
    expect(toClause(row)).toBe(clause);
    expect(fromClause(clause)).toEqual(row);
  });

  it('未再生だけの行', () => {
    expect(toClause({ field: 'played', op: 'never', days: 7 })).toBe('再生:未');
    expect(fromClause('再生:未')).toMatchObject({ field: 'played', op: 'never' });
  });

  it('中身が空なら空の文字列', () => {
    expect(toClause(newRow('tags'))).toBe('');
    expect(toClause({ field: 'kind', op: 'has', value: [] })).toBe('');
    expect(toClause(newRow('folder'))).toBe('');
  });

  it('決まった形でない条件は検索式の行', () => {
    expect(fromClause('評価>=4 海')).toEqual({ field: 'query', text: '評価>=4 海' });
    expect(fromClause('(海 | 評価>=3)')).toEqual({ field: 'query', text: '(海 | 評価>=3)' });
    expect(fromClause('-種類:動画')).toEqual({ field: 'query', text: '-種類:動画' });
  });
});
