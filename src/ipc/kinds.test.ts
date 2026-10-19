import { render } from '@testing-library/react';
import { createElement } from 'react';
import { describe, expect, it } from 'vitest';

import { KIND_ICONS, KindIcon } from '../components/icons/KindIcon';
import { ja } from '../i18n/ja';
import { ITEM_KINDS, QUERY_KIND_WORDS } from './bindings';
import {
  countsFromPlural,
  hasDuration,
  isItemKind,
  isPlayable,
  KIND_ORDER,
  kindBreakdown,
  kindFromWord,
  kindLabel,
  kindWord,
  needsProxy,
  PLAYABLE_KINDS,
} from './kinds';

describe('種類の表（ITEM_KINDS）', () => {
  it('並びは bindings の ITEM_KINDS のまま（画像・動画・音楽・フォント・3D）', () => {
    expect(KIND_ORDER).toEqual(ITEM_KINDS.map((row) => row.kind));
    expect(KIND_ORDER).toEqual(['image', 'video', 'audio', 'font', 'model']);
  });

  it.each(ITEM_KINDS.map((row) => row.kind))('%s には ja のラベルとアイコンがある', (kind) => {
    expect(kindLabel(kind)).not.toBe('');
    expect(kindLabel(kind)).toBe(ja.kinds[kind]);
    expect(KIND_ICONS[kind]).toBeTypeOf('function');
    const { container } = render(createElement(KindIcon, { kind }));
    expect(container.querySelector('svg')?.childElementCount ?? 0).toBeGreaterThan(0);
  });

  it('アイコンは種類ごとに違う絵', () => {
    const drawn = KIND_ORDER.map(
      (kind) => render(createElement(KindIcon, { kind })).container.innerHTML,
    );
    expect(new Set(drawn).size).toBe(KIND_ORDER.length);
  });

  it('フォントは「フォント」、3D は「3D」（決定 10）', () => {
    expect(kindLabel('font')).toBe('フォント');
    expect(kindLabel('model')).toBe('3D');
  });

  it('isPlayable は画像・動画・音楽の 3 種だけ', () => {
    expect(KIND_ORDER.filter((k) => isPlayable(k))).toEqual(['image', 'video', 'audio']);
    expect(PLAYABLE_KINDS).toEqual(['image', 'video', 'audio']);
    expect(isPlayable('font')).toBe(false);
    expect(isPlayable('model')).toBe(false);
    expect(isPlayable('sticker')).toBe(false);
    expect(isPlayable(undefined)).toBe(false);
  });

  it('長さ・プロキシは動画・音楽だけ', () => {
    expect(KIND_ORDER.filter((k) => hasDuration(k))).toEqual(['video', 'audio']);
    expect(KIND_ORDER.filter((k) => needsProxy(k))).toEqual(['video', 'audio']);
  });

  it('知らない値は種類でなく、ラベルは空', () => {
    expect(isItemKind('image')).toBe(true);
    expect(isItemKind('sticker')).toBe(false);
    expect(isItemKind(null)).toBe(false);
    expect(kindLabel('sticker')).toBe('');
    expect(
      render(createElement(KindIcon, { kind: 'sticker' })).container.querySelector('svg'),
    ).not.toBeNull();
  });
});

describe('検索の語', () => {
  it('語は Rust の表（bindings の QUERY_KIND_WORDS）のまま。どの種類にも語がある', () => {
    expect(QUERY_KIND_WORDS.map((w) => w.kind).sort()).toEqual([...KIND_ORDER].sort());
    for (const { kind, word } of QUERY_KIND_WORDS) {
      expect(kindWord(kind)).toBe(word);
    }
    expect(kindWord('font')).toBe('フォント');
    expect(kindWord('model')).toBe('3D');
  });

  it('語から種類に戻る（日本語の語は大文字・小文字を問わず、英語は種類の名前）', () => {
    for (const kind of KIND_ORDER) {
      expect(kindFromWord(kindWord(kind))).toBe(kind);
      expect(kindFromWord(kind.toUpperCase())).toBe(kind);
    }
    expect(kindFromWord('3d')).toBe('model');
    expect(kindFromWord(' フォント ')).toBe('font');
    expect(kindFromWord('写真')).toBeNull();
  });

  it('語に空白・記号を含まない（`種類:<語>` がそのまま式になる）', () => {
    for (const kind of KIND_ORDER) {
      expect(kindWord(kind)).toMatch(/^[^\s,、:=<>!()"|]+$/);
    }
  });
});

describe('種類ごとの数', () => {
  it('並びの順に、0 件を除いて並べる', () => {
    expect(kindBreakdown({ audio: 2, image: 5, model: 0, font: 1 })).toEqual([
      { kind: 'image', label: '画像', count: 5 },
      { kind: 'audio', label: '音楽', count: 2 },
      { kind: 'font', label: 'フォント', count: 1 },
    ]);
  });

  it('複数形の欄から読む', () => {
    expect(countsFromPlural({ images: 1, videos: 2, audios: 3, fonts: 4, models: 5 })).toEqual({
      image: 1,
      video: 2,
      audio: 3,
      font: 4,
      model: 5,
    });
  });
});

/** src の .ts・.tsx（テスト・自動生成・この表のファイルを除く）。 */
const sources = Object.entries(
  import.meta.glob<string>(['/src/**/*.ts', '/src/**/*.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
  }),
).filter(
  ([path]) =>
    !/\.test\.tsx?$/.test(path) &&
    path !== '/src/ipc/bindings.ts' &&
    path !== '/src/ipc/kinds.ts' &&
    path !== '/src/i18n/ja.ts',
);

/** 種類の並び・型・ラベルを画面ごとに書き直している形。 */
const SCATTERED = [
  // 種類の union の型（`'image' | 'video' | 'audio'` など）
  /'(?:image|video|audio|font|model)'\s*\|\s*'(?:image|video|audio|font|model)'/,
  // 種類の並びの配列
  /\[\s*'(?:image|video|audio|font|model)'\s*,\s*'(?:image|video|audio|font|model)'/,
  // ラベルの表を直接引く
  /ja\.kinds\b/,
];

describe('種類は kinds.ts だけで扱う', () => {
  it('ソースを読み込めている', () => {
    expect(sources.length).toBeGreaterThan(50);
  });

  it.each(sources)('%s に種類の型・並び・ラベルの表がない', (_path, text) => {
    const found = SCATTERED.flatMap((pattern) => text.match(pattern)?.[0] ?? []);
    expect(found).toEqual([]);
  });
});
