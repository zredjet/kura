import { describe, expect, it } from 'vitest';

import { effectiveFormat, exportName, formatOfExt, savedExt, splitName } from './formats';

describe('書き出しの名前（Rust の export_name と同じ規則）', () => {
  it('形式が同じなら元の名前のまま（jpg と jpeg は同じ）', () => {
    expect(exportName('IMG_0001.JPG', 'jpg')).toBe('IMG_0001.JPG');
    expect(exportName('photo.jpeg', 'jpg')).toBe('photo.jpeg');
    expect(exportName('anim.apng', 'png')).toBe('anim.apng');
    expect(exportName('scan.TIF', 'tiff')).toBe('scan.TIF');
  });

  it('形式が違えば語幹に今の拡張子（拡張子のない名前・知らない拡張子も）', () => {
    expect(exportName('hero.psd', 'png')).toBe('hero.png');
    expect(exportName('hero.png', 'webp')).toBe('hero.webp');
    expect(exportName('archive.v2.gif', 'png')).toBe('archive.v2.png');
    expect(exportName('README', 'png')).toBe('README.png');
    expect(exportName('dump.dat', 'png')).toBe('dump.png');
    expect(exportName('a.xyz', 'XYZ')).toBe('a.xyz');
  });

  it('空の名前は変えない', () => {
    expect(exportName('', 'png')).toBe('');
    expect(exportName('hero.png', '')).toBe('hero.png');
    expect(splitName('.hidden')).toEqual({ stem: '.hidden', ext: null });
  });
});

describe('出力の形式', () => {
  it('「元と同じ」は元の形式。元に書けなければ PNG で保存する', () => {
    expect(effectiveFormat({ type: 'keep' }, 'png')).toEqual({ format: 'png', forced: false });
    expect(effectiveFormat({ type: 'keep' }, 'psd')).toEqual({ format: 'png', forced: true });
    expect(effectiveFormat({ type: 'to', format: 'webp' }, 'psd')).toEqual({
      format: 'webp',
      forced: false,
    });
  });

  it('保存したあとの拡張子：形式が今と同じなら今の拡張子、違えば代表', () => {
    expect(savedExt('jpeg', 'jpeg')).toBe('jpeg');
    expect(savedExt('jpeg', 'png')).toBe('jpg');
    expect(savedExt('png', 'psd')).toBe('png');
    expect(formatOfExt('JPG')).toBe('jpeg');
    expect(formatOfExt('mp4')).toBeNull();
  });
});
