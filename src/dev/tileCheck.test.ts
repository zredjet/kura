import { describe, expect, it } from 'vitest';

import { type CheckIo, runTileCheck, stats } from './tileCheck';

/** 時計を進める偽の口：RGBA8 は読むのが遅く解くのが速い、PNG はその逆。 */
function fakeIo(): CheckIo & { urls: string[] } {
  let t = 0;
  const urls: string[] = [];
  return {
    urls,
    now: () => t,
    read: async (url, format) => {
      urls.push(url);
      t += format === 'rgba' ? 4 : 1;
      return format === 'png' ? new Blob([new Uint8Array(100)]) : new ArrayBuffer(256 * 256 * 4);
    },
    decode: async (_body, format) => {
      t += format === 'rgba' ? 1 : 3;
    },
  };
}

describe('タイルの形式の自己点検（VITE_KURA_CHECK）', () => {
  it('両方の形式を、順を入れ替えながら 1 枚ずつと同時に測る', async () => {
    const io = fakeIo();
    const report = await runTileCheck(
      {
        session: 'ab12',
        rev: 3,
        level: 1,
        frame: 0,
        dpr: 2,
        tiles: [
          { tx: 0, ty: 0, w: 256, h: 256 },
          { tx: 1, ty: 0, w: 256, h: 256 },
        ],
      },
      io,
      3,
    );
    expect(report.rgba).toMatchObject({ tiles: 2, bytes: 2 * 256 * 256 * 4, errors: 0 });
    expect(report.png).toMatchObject({ tiles: 2, bytes: 200, errors: 0 });
    expect(report.rgba.fetch.p50).toBe(4);
    expect(report.rgba.decode.p50).toBe(1);
    expect(report.png.total.p50).toBe(4);
    expect(report.rgba.parallelMs).toHaveLength(3);
    expect(report.fasterPerTile).toBe('png');
    // URL は media.ts の editTileUrl で、PNG は .png
    expect(io.urls).toContain('kura://localhost/edit/ab12/3/1/0/1/0');
    expect(io.urls).toContain('kura://localhost/edit/ab12/3/1/0/1/0.png');
  });

  it('読めなかったタイルは数える', async () => {
    const io = fakeIo();
    const failing: CheckIo = { ...io, read: async () => Promise.reject(new Error('404')) };
    const report = await runTileCheck(
      { session: 'ab', rev: 1, level: 0, frame: 0, dpr: 1, tiles: [{ tx: 0, ty: 0, w: 1, h: 1 }] },
      failing,
      1,
    );
    expect(report.rgba.errors).toBe(2);
    expect(report.png.errors).toBe(2);
  });

  it('分布の要約', () => {
    expect(stats([])).toEqual({ p50: 0, p95: 0, max: 0, mean: 0 });
    expect(stats([5, 1, 3, 2, 4])).toEqual({ p50: 3, p95: 5, max: 5, mean: 3 });
  });
});

/** src の .ts・.tsx（テストを除く）。 */
const sources = Object.entries(
  import.meta.glob<string>(['/src/**/*.ts', '/src/**/*.tsx'], {
    query: '?raw',
    import: 'default',
    eager: true,
  }),
).filter(([path]) => !/\.test\.tsx?$/.test(path));

describe('タイルの自己点検は通常のビルドに入らない', () => {
  it('読み込むのは editorStore.ts の VITE_KURA_CHECK の枝の動的な import だけ', () => {
    const users = sources
      .filter(([, text]) => /(?:from|import\()\s*'[^']*dev\/tileCheck'/.test(text))
      .map(([path]) => path);
    expect(users).toEqual(['/src/features/editor/editorStore.ts']);
    const text =
      sources.find(([path]) => path === '/src/features/editor/editorStore.ts')?.[1] ?? '';
    expect(text).toMatch(
      /if \(import\.meta\.env\.VITE_KURA_CHECK === '1'\) \{\s*void import\('\.\.\/\.\.\/dev\/tileCheck'\)/,
    );
    expect(text).not.toMatch(/^import .*dev\/tileCheck/m);
  });
});
