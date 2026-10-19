/**
 * スマートプレイリストのルールの 1 行と、検索の文字列（docs/search-query.md）の相互変換。
 * 行は決まった形の文字列にして保存し（`rule_json` の `clauses`）、読むときに同じ形なら行に戻す。
 * 形の決まっていない条件は「検索式」の行として文字のまま扱う。
 */

import { kindWord, PLAYABLE_KINDS, type PlayableKind } from '../../ipc/kinds';

/**
 * ルールの行で選べる種類：再生できる種類だけ（スマートの候補にフォント・3D は入らない。docs/playlists.md §1.1）。
 */
export type KindValue = PlayableKind;

export type RuleRow =
  | { field: 'rating'; op: 'ge' | 'le' | 'eq'; value: number }
  | { field: 'kind'; op: 'has' | 'only'; value: KindValue[] }
  | { field: 'tags'; op: 'all' | 'any' | 'none'; value: string[] }
  | { field: 'duration'; op: 'gt' | 'lt'; minutes: number }
  | { field: 'played'; op: 'before' | 'within' | 'never'; days: number }
  | { field: 'added'; op: 'within' | 'before'; days: number }
  | { field: 'folder'; op: 'in' | 'out'; path: string }
  | { field: 'name'; op: 'has' | 'not'; text: string }
  | { field: 'query'; text: string };

export type RuleField = RuleRow['field'];

/** 行で選べる種類（並びは `ITEM_KINDS` の順）。 */
export const RULE_KINDS: readonly KindValue[] = PLAYABLE_KINDS;

/** 空白・記号を含む値は引用符で囲む（パレットの候補と同じ。ADR-52）。 */
function quote(value: string): string {
  return /[\s()|"]/.test(value) ? `"${value.replace(/"/g, '')}"` : value;
}

/** タグ（「名前空間:名前」なら名前の部分だけを囲む）。 */
function quoteTag(tag: string): string {
  const at = tag.indexOf(':');
  if (at > 0) {
    return `${tag.slice(0, at)}:${quote(tag.slice(at + 1))}`;
  }
  return quote(tag);
}

function unquote(value: string): string {
  return value.startsWith('"') && value.endsWith('"') && value.length >= 2
    ? value.slice(1, -1)
    : value;
}

/** 新しく足す行の初期値。 */
export function newRow(field: RuleField): RuleRow {
  switch (field) {
    case 'rating':
      return { field, op: 'ge', value: 4 };
    case 'kind':
      return { field, op: 'has', value: [...RULE_KINDS] };
    case 'tags':
      return { field, op: 'all', value: [] };
    case 'duration':
      return { field, op: 'gt', minutes: 3 };
    case 'played':
      return { field, op: 'before', days: 7 };
    case 'added':
      return { field, op: 'within', days: 30 };
    case 'folder':
      return { field, op: 'in', path: '' };
    case 'name':
      return { field, op: 'has', text: '' };
    case 'query':
      return { field, text: '' };
  }
}

/** 行を検索の文字列にする（中身が空なら空文字）。 */
export function toClause(row: RuleRow): string {
  switch (row.field) {
    case 'rating':
      return `評価${{ ge: '>=', le: '<=', eq: '=' }[row.op]}${row.value}`;
    case 'kind': {
      const kinds = RULE_KINDS.filter((k) => row.value.includes(k)).map(kindWord);
      return kinds.length === 0 ? '' : `種類${row.op === 'only' ? '=' : ':'}${kinds.join(',')}`;
    }
    case 'tags': {
      const tags = row.value
        .map((t) => t.trim())
        .filter(Boolean)
        .map(quoteTag);
      if (tags.length === 0) {
        return '';
      }
      if (row.op === 'none') {
        return tags.map((t) => `-${t}`).join(' ');
      }
      if (row.op === 'any' && tags.length > 1) {
        return `(${tags.join(' | ')})`;
      }
      return tags.join(' ');
    }
    case 'duration':
      return `長さ${row.op === 'gt' ? '>' : '<'}${row.minutes}分`;
    case 'played':
      if (row.op === 'never') {
        return '再生:未';
      }
      return row.op === 'within'
        ? `再生:${row.days}日以内`
        : `(再生:${row.days}日以上前 | 再生:未)`;
    case 'added':
      return `追加:${row.days}日${row.op === 'within' ? '以内' : '以上前'}`;
    case 'folder':
      return row.path.trim()
        ? `${row.op === 'out' ? '-' : ''}フォルダ:${quote(row.path.trim())}`
        : '';
    case 'name':
      return row.text.trim() ? `${row.op === 'not' ? '-' : ''}名前:${quote(row.text.trim())}` : '';
    case 'query':
      return row.text.trim();
  }
}

/** 文字列のタグの並び（空白で区切る。引用符の中は区切らない）。 */
function splitTerms(text: string): string[] | null {
  const terms = text.match(/(?:[^\s"]+(?::"[^"]*")?|"[^"]*")/g);
  return terms && terms.join(' ') === text.trim().replace(/\s+/g, ' ') ? terms : null;
}

/** タグとして読める語か（フィールドの条件・記号を含まない）。 */
function isTagTerm(term: string): boolean {
  const field =
    /^(種類|評価|長さ|追加|撮影|再生|再生回数|フォルダ|ユニット|取得元|名前|url|メモ|タグ数|幅|高さ|向き|type|rating|dur|added|taken|played|plays|folder|unit|source|name|note|tags|w|h|orient)[:=<>!]/i;
  return !field.test(term) && !/[()|]/.test(term) && !term.startsWith('-');
}

function tagValue(term: string): string {
  const at = term.indexOf(':');
  return at > 0 ? `${term.slice(0, at)}:${unquote(term.slice(at + 1))}` : unquote(term);
}

/** 検索の文字列を行に戻す（決まった形でなければ「検索式」の行）。 */
export function fromClause(clause: string): RuleRow {
  const text = clause.trim();
  let m: RegExpMatchArray | null;
  if ((m = text.match(/^評価(>=|<=|=)([0-5])$/))) {
    const op = m[1] === '>=' ? 'ge' : m[1] === '<=' ? 'le' : 'eq';
    return { field: 'rating', op, value: Number(m[2]) };
  }
  if ((m = text.match(/^種類([:=])([^,\s]+(?:,[^,\s]+)*)$/))) {
    // 行に戻すのは、行で書いた形（再生できる種類の語だけ）のとき。ほかは「検索式」の行のまま
    const words = (m[2] ?? '').split(',');
    if (words.every((w) => RULE_KINDS.some((k) => kindWord(k) === w))) {
      const value = RULE_KINDS.filter((k) => words.includes(kindWord(k)));
      return { field: 'kind', op: m[1] === '=' ? 'only' : 'has', value };
    }
  }
  if ((m = text.match(/^長さ([<>])(\d+)分$/))) {
    return { field: 'duration', op: m[1] === '>' ? 'gt' : 'lt', minutes: Number(m[2]) };
  }
  if (text === '再生:未') {
    return { field: 'played', op: 'never', days: 7 };
  }
  if ((m = text.match(/^再生:(\d+)日以内$/))) {
    return { field: 'played', op: 'within', days: Number(m[1]) };
  }
  if ((m = text.match(/^\(再生:(\d+)日以上前 \| 再生:未\)$/))) {
    return { field: 'played', op: 'before', days: Number(m[1]) };
  }
  if ((m = text.match(/^追加:(\d+)日(以内|以上前)$/))) {
    return { field: 'added', op: m[2] === '以内' ? 'within' : 'before', days: Number(m[1]) };
  }
  if ((m = text.match(/^(-?)フォルダ:(.+)$/))) {
    return { field: 'folder', op: m[1] ? 'out' : 'in', path: unquote(m[2] ?? '') };
  }
  if ((m = text.match(/^(-?)名前:(.+)$/))) {
    return { field: 'name', op: m[1] ? 'not' : 'has', text: unquote(m[2] ?? '') };
  }
  // タグ：「a b」はすべて、「(a | b)」はいずれか、「-a -b」は含まない
  if ((m = text.match(/^\((.+)\)$/))) {
    const parts = (m[1] ?? '').split(' | ');
    if (parts.length > 1 && parts.every(isTagTerm)) {
      return { field: 'tags', op: 'any', value: parts.map(tagValue) };
    }
  }
  const terms = splitTerms(text);
  if (terms && terms.length > 0) {
    if (terms.every((t) => t.startsWith('-') && isTagTerm(t.slice(1)))) {
      return { field: 'tags', op: 'none', value: terms.map((t) => tagValue(t.slice(1))) };
    }
    if (terms.every(isTagTerm)) {
      return { field: 'tags', op: 'all', value: terms.map(tagValue) };
    }
  }
  return { field: 'query', text };
}
