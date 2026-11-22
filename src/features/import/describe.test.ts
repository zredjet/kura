import { describe, expect, it } from 'vitest';

import type { ImportPreviewDto } from '../../ipc/import';
import {
  confirmLabel,
  describeSources,
  kindStats,
  folderChoices,
  folderTagExample,
  reproduceExample,
  unitExample,
} from './describe';

function preview(over: Partial<ImportPreviewDto> = {}): ImportPreviewDto {
  return {
    total: 128,
    images: 96,
    videos: 24,
    audios: 8,
    fonts: 0,
    models: 0,
    totalBytes: 18_400_000_000,
    duplicates: 6,
    skipped: 0,
    roots: [{ name: '旅行2026', isDir: true }],
    subfolders: 0,
    sampleDirs: ['旅行2026', '鎌倉'],
    errors: 0,
    ...over,
  };
}

describe('kindStats', () => {
  it('画像・動画・音楽は 0 件でも出し、フォント・3D は 1 件以上のときだけ出す', () => {
    expect(kindStats(preview({ videos: 0 }))).toEqual([
      { kind: 'image', label: '画像', count: 96 },
      { kind: 'video', label: '動画', count: 0 },
      { kind: 'audio', label: '音楽', count: 8 },
    ]);
    expect(kindStats(preview({ fonts: 3, models: 1 })).map((s) => [s.label, s.count])).toEqual([
      ['画像', 96],
      ['動画', 24],
      ['音楽', 8],
      ['フォント', 3],
      ['3D', 1],
    ]);
  });
});

describe('describeSources', () => {
  it('モックと同じ形にする', () => {
    const p = preview({
      roots: [
        { name: '旅行2026', isDir: true },
        { name: '花火', isDir: true },
        { name: '家族', isDir: true },
      ],
      subfolders: 6,
    });
    expect(describeSources(p)).toBe('旅行2026 ほか 2 フォルダ · サブフォルダ 6');
  });

  it('ファイルだけ・混在・対象外', () => {
    expect(
      describeSources(
        preview({
          roots: [
            { name: 'a.jpg', isDir: false },
            { name: 'b.jpg', isDir: false },
          ],
        }),
      ),
    ).toBe('a.jpg ほか 1 ファイル');
    expect(
      describeSources(
        preview({
          roots: [
            { name: 'a.jpg', isDir: false },
            { name: 'x', isDir: true },
            { name: 'b.jpg', isDir: false },
          ],
          skipped: 3,
        }),
      ),
    ).toBe('a.jpg ほか 2 項目 · 対象外 3 件');
    expect(describeSources(preview())).toBe('旅行2026');
  });
});

describe('confirmLabel', () => {
  it('重複の見込みを除いた数を出す', () => {
    expect(confirmLabel(preview())).toBe('122 項目を取り込む');
    expect(confirmLabel(preview({ total: 6, duplicates: 6 }))).toBe('取り込む');
  });
});

describe('例の文', () => {
  it('フォルダ構成の再現（取り込み先あり・なし）', () => {
    expect(reproduceExample(['旅行2026', '鎌倉'], ['写真', '2026 旅行'])).toBe(
      '旅行2026 / 鎌倉 → 写真 / 2026 旅行 / 鎌倉',
    );
    expect(reproduceExample(['旅行2026', '鎌倉'], null)).toBe('旅行2026 / 鎌倉 → 旅行2026 / 鎌倉');
    expect(reproduceExample([], null)).toBe('');
  });

  it('フォルダ名のタグとユニット', () => {
    expect(folderTagExample(['旅行2026', '鎌倉'])).toBe(
      '旅行2026 / 鎌倉 → タグ「旅行2026」「鎌倉」',
    );
    expect(unitExample(['旅行2026', '鎌倉'])).toBe('鎌倉 → ユニット「鎌倉」');
  });
});

describe('folderChoices', () => {
  it('木の順に並べてパスを付ける', () => {
    const choices = folderChoices([
      { id: 3, parentId: null, name: 'イラスト', color: null, pos: 2, count: null, unitId: null },
      { id: 2, parentId: 1, name: '2026 旅行', color: null, pos: 1, count: null, unitId: null },
      { id: 1, parentId: null, name: '写真', color: null, pos: 1, count: null, unitId: null },
      { id: 4, parentId: 1, name: '家族', color: null, pos: 2, count: null, unitId: null },
    ]);
    expect(choices.map((c) => c.path.join(' / '))).toEqual([
      '写真',
      '写真 / 2026 旅行',
      '写真 / 家族',
      'イラスト',
    ]);
  });
});
