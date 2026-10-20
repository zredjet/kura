/**
 * 検索の条件を「チップ（AND で並ぶ条件）」の列として扱う純粋関数。
 * 条件の正本は文字列（チップを空白でつないだもの）で、解析は Rust（`query_parse`）が行う。
 * ここではフィルタバーが自分のフィールドの条件を見つけて差し替えられるよう、先頭のフィールド名だけを読む。
 */

import { QUERY_FIELD_NAMES, QUERY_SYMBOLS } from '../../ipc/bindings';
import { type ItemKind, kindFromWord } from '../../ipc/kinds';

/**
 * フィールドの名前（日本語・英語。英語は小文字で引く）→ 英語名（docs/search-query.md §3）。
 * 表の正本は Rust（`query::FIELDS`。bindings の `QUERY_FIELD_NAMES`）。足し忘れると `透過:あり` がタグとして扱われる。
 */
const FIELDS = new Map<string, string>(
  QUERY_FIELD_NAMES.flatMap(({ field, names }) =>
    names.flatMap((name): [string, string][] => [
      [name, field],
      [name.toLowerCase(), field],
    ]),
  ),
);

/** 名前がフィールドなら英語名（`種類`・`TYPE` → `type`）。名前空間付きのタグの名前なら null。 */
export function fieldOf(name: string): string | null {
  return FIELDS.get(name) ?? FIELDS.get(name.toLowerCase()) ?? null;
}

/** 半角と同じに扱う記号（Rust の字句解析の表 `QUERY_SYMBOLS`：全角の記号・U+2212・読点など）。 */
const SYMBOLS = new Map<string, string>(QUERY_SYMBOLS.map(({ from, to }) => [from, to]));

/** 記号を Rust の字句解析と同じに直す（引用符の中はそのまま。引用符自身の全角は直す）。 */
function normalize(text: string): string {
  let quoted = false;
  return Array.from(text, (c) => {
    const n = SYMBOLS.get(c) ?? c;
    if (n === '"') {
      quoted = !quoted;
      return n;
    }
    return quoted ? c : n;
  }).join('');
}

/** 条件 1 つの中身。 */
export interface ClauseParts {
  /** 先頭の `-` で除外しているか。 */
  negated: boolean;
  /** フィールドの英語名（フィールドの条件でなければ null）。 */
  field: string | null;
  /** 入力どおりのフィールド名・記号・値（フィールドの条件のとき）。 */
  name: string;
  op: string;
  value: string;
}

/** 条件 1 つを読む（かっこで囲んだ式などはフィールドなし）。 */
export function clauseParts(text: string): ClauseParts {
  const t = normalize(text.trim());
  const negated = t.startsWith('-') && t.length > 1;
  const body = negated ? t.slice(1) : t;
  const m = /^([^\s:=<>!()"|]+?)(!=|>=|<=|:|=|>|<)(.*)$/s.exec(body);
  if (m) {
    const [, name = '', op = '', value = ''] = m;
    const field = fieldOf(name);
    if (field) {
      return { negated, field, name, op, value: value.replace(/^"(.*)"$/s, '$1') };
    }
  }
  return { negated, field: null, name: '', op: '', value: body };
}

/** チップをつないで検索の文字列にする。 */
export function joinClauses(clauses: readonly string[]): string {
  return clauses
    .map((c) => c.trim())
    .filter(Boolean)
    .join(' ');
}

/** そのフィールドの（除外でない）条件を差し替える（複数でもよい）。`clause` が null なら外す。 */
export function replaceField(
  clauses: readonly string[],
  field: string,
  clause: string | readonly string[] | null,
): string[] {
  const kept = clauses.filter((c) => {
    const parts = clauseParts(c);
    return parts.field !== field || parts.negated;
  });
  if (clause === null) {
    return kept;
  }
  return [...kept, ...(typeof clause === 'string' ? [clause] : clause)];
}

/** そのフィールドの（除外でない）条件の文字列すべて。 */
export function fieldTexts(clauses: readonly string[], field: string): string[] {
  return clauses.filter((c) => {
    const parts = clauseParts(c);
    return parts.field === field && !parts.negated;
  });
}

/** そのフィールドの（除外でない）条件。 */
export function findField(clauses: readonly string[], field: string): ClauseParts | null {
  for (const c of clauses) {
    const parts = clauseParts(c);
    if (parts.field === field && !parts.negated) {
      return parts;
    }
  }
  return null;
}

/** 種類の条件が 1 種類だけならその種類（フィルタバーのセグメント）。 */
export function singleKind(clauses: readonly string[]): ItemKind | null {
  const parts = findField(clauses, 'type');
  if (!parts || (parts.op !== ':' && parts.op !== '=')) {
    return null;
  }
  // 読点「、」・全角の「，」は normalize が「,」に直している（Rust と同じ）
  const values = parts.value
    .split(',')
    .map((v) => v.trim())
    .filter(Boolean);
  if (values.length !== 1) {
    return null;
  }
  return kindFromWord(values[0] ?? '');
}

/** 入力の途中で、ここで区切ってチップにしてよいか（引用符・かっこが閉じていて、記号で終わっていない）。 */
export function isComplete(text: string): boolean {
  const t = normalize(text.trim());
  if (!t) {
    return false;
  }
  let quotes = 0;
  let depth = 0;
  for (const c of t) {
    if (c === '"' || c === '“' || c === '”') {
      quotes += 1;
    } else if (quotes % 2 === 0 && (c === '(' || c === '（')) {
      depth += 1;
    } else if (quotes % 2 === 0 && (c === ')' || c === '）')) {
      depth -= 1;
    }
  }
  return quotes % 2 === 0 && depth === 0 && !/[|｜\-:=<>]$/.test(t);
}

/** 入力中の語（候補を探す語）と、その始まりの位置。引用符の中なら引用符から。 */
export function currentWord(draft: string): { start: number; word: string } {
  let start = 0;
  let inQuote = false;
  for (let i = 0; i < draft.length; i += 1) {
    const c = draft[i];
    if (c === '"') {
      inQuote = !inQuote;
    } else if (!inQuote && (c === ' ' || c === '　' || c === '(' || c === '|')) {
      start = i + 1;
    }
  }
  return { start, word: draft.slice(start) };
}
