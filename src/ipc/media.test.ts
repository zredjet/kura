import { describe, expect, it } from 'vitest';

import {
  animUrl,
  editTileUrl,
  fileUrl,
  hasThumb,
  pcmUrl,
  peaksUrl,
  playUrl,
  thumbUrl,
} from './media';

describe('URL', () => {
  it('サムネイルは rev が 0 より大きいときだけ ?r= を付ける', () => {
    expect(thumbUrl('ab', 's', 0)).toBe('kura://localhost/thumb/ab/s');
    expect(thumbUrl('ab', 'l', 3)).toBe('kura://localhost/thumb/ab/l?r=3');
    expect(thumbUrl('ab', 's', -1)).toBe('kura://localhost/thumb/ab/s');
    expect(thumbUrl('ab', 's', Number.NaN)).toBe('kura://localhost/thumb/ab/s');
    expect(thumbUrl('ab', 's', 1.5)).toBe('kura://localhost/thumb/ab/s');
  });

  it('波形・試聴の PCM・アニメの帯', () => {
    expect(peaksUrl('ab')).toBe('kura://localhost/peaks/ab');
    expect(pcmUrl('ab', 0, 44100)).toBe('kura://localhost/pcm/ab/0-44100');
    expect(animUrl('ab', 0)).toBe('kura://localhost/anim/ab/1');
    expect(animUrl('ab', 7)).toBe('kura://localhost/anim/ab/1?r=7');
    expect(fileUrl('ab')).toBe('kura://localhost/file/ab');
    expect(playUrl('ab')).toBe('kura://localhost/play/ab');
  });

  it.each([
    [10, 10],
    [10, 5],
    [-1, 5],
    [0.5, 5],
    [0, Number.POSITIVE_INFINITY],
  ])('PCM の区間 %s-%s は断る', (start, end) => {
    expect(() => pcmUrl('ab', start, end)).toThrow(RangeError);
  });

  it('エディタのタイル（M12）：拡張子なしは RGBA8、.png は PNG', () => {
    expect(editTileUrl('0a1b', 12, 0, 0, 1, 2, 'rgba')).toBe(
      'kura://localhost/edit/0a1b/12/0/0/1/2',
    );
    expect(editTileUrl('0a1b', 12, 3, 4, 0, 0, 'png')).toBe(
      'kura://localhost/edit/0a1b/12/3/4/0/0.png',
    );
  });

  it.each([
    ['ABC', 1, 0, 0, 0, 0],
    ['a/b', 1, 0, 0, 0, 0],
    ['ab', -1, 0, 0, 0, 0],
    ['ab', 1, 4, 0, 0, 0],
    ['ab', 1, 0.5, 0, 0, 0],
    ['ab', 1, 0, 0, 1.5, 0],
    ['ab', 1, 0, 0, 0, Number.NaN],
  ])('エディタのタイルの指定 %s/%s/%s/%s/%s/%s は断る', (session, rev, level, frame, tx, ty) => {
    expect(() => editTileUrl(session, rev, level, frame, tx, ty, 'rgba')).toThrow(RangeError);
  });

  it('サムネイルがあるのは ready と stale（作り直すまで今の絵を出す）', () => {
    expect(hasThumb('ready')).toBe(true);
    expect(hasThumb('stale')).toBe(true);
    expect(hasThumb('pending')).toBe(false);
    expect(hasThumb('failed')).toBe(false);
    expect(hasThumb(undefined)).toBe(false);
  });
});

/** src の .ts・.tsx（テストと自動生成を除く）。 */
const sources = Object.entries(
  import.meta.glob<string>(['/src/**/*.ts', '/src/**/*.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
  }),
).filter(([path]) => !/\.test\.tsx?$/.test(path) && path !== '/src/ipc/bindings.ts');

/** 注釈を除いた中身（`/* … *\/` と、行頭の `//`。文字列の中の `//` は残す）。 */
function code(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
}

describe('メディアの URL と表示の入口', () => {
  it('ソースを読み込めている', () => {
    expect(sources.length).toBeGreaterThan(50);
  });

  // 自己点検（VITE_KURA_SELFTEST）は、受け口が断るはずの壊れた URL もわざと組み立てるので外す
  const urlSources = sources.filter(
    ([path]) => path !== '/src/ipc/media.ts' && path !== '/src/dev/selftest.ts',
  );
  it.each(urlSources)('%s は kura:// を組み立てない（media.ts の関数を使う）', (_path, text) => {
    expect(code(text).match(/kura:\/\//g) ?? []).toEqual([]);
  });

  // サムネイルの絵は MediaImage だけが出す（市松模様・最近傍・フォントの mask と ?r= の漏れを防ぐ）
  const thumbSources = sources.filter(
    ([path]) =>
      path !== '/src/ipc/media.ts' && path !== '/src/components/MediaImage/MediaImage.tsx',
  );
  it.each(thumbSources)('%s は thumbUrl を呼ばない（MediaImage を使う）', (_path, text) => {
    expect(code(text).match(/\bthumbUrl\s*\(/g) ?? []).toEqual([]);
  });

  // stale（作り直し中）も今の絵を出す。'ready' だけで判定しない（判定の定義の media.ts を除く）
  const stateSources = sources.filter(([path]) => path !== '/src/ipc/media.ts');
  it.each(stateSources)(
    '%s は thumbState を ready だけで判定しない（hasThumb を使う）',
    (_path, text) => {
      expect(code(text).match(/thumbState\s*[!=]==?\s*'ready'/g) ?? []).toEqual([]);
    },
  );
});
