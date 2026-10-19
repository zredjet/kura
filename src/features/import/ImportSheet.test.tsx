import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ImportPreviewDto } from '../../ipc/import';
import { startImport } from '../../ipc/import';
import { useImportStore } from '../../stores/importStore';
import { ImportSheet } from './ImportSheet';

vi.mock('../../ipc/folders', () => ({
  foldersTree: vi.fn(async () => [
    { id: 1, parentId: null, name: '写真', color: null, pos: 1 },
    { id: 2, parentId: 1, name: '2026 旅行', color: null, pos: 1 },
  ]),
}));
vi.mock('../../ipc/import', () => ({
  previewImport: vi.fn(),
  startImport: vi.fn(async () => 7),
}));

const PREVIEW: ImportPreviewDto = {
  total: 128,
  images: 96,
  videos: 24,
  audios: 8,
  fonts: 0,
  models: 0,
  totalBytes: 18_400_000_000,
  duplicates: 6,
  skipped: 0,
  roots: [
    { name: '旅行2026', isDir: true },
    { name: '花火', isDir: true },
    { name: '家族', isDir: true },
  ],
  subfolders: 6,
  sampleDirs: ['旅行2026', '鎌倉'],
  errors: 0,
};

function open(preview: ImportPreviewDto | null = PREVIEW, error: string | null = null) {
  useImportStore.setState({
    dragging: false,
    phase: 'sheet',
    paths: ['/src/旅行2026'],
    preview,
    error,
  });
}

beforeEach(() => {
  vi.mocked(startImport).mockClear();
});

describe('ImportSheet', () => {
  it('モックと同じ内容を出す', async () => {
    open();
    render(<ImportSheet />);
    const dialog = screen.getByRole('dialog', { name: '128 項目を取り込み' });
    expect(dialog).toHaveTextContent('旅行2026 ほか 2 フォルダ · サブフォルダ 6');
    expect(dialog).toHaveTextContent('18.4 GB');
    expect(dialog).toHaveTextContent('6 項目はすでにライブラリにあります（内容が同一）');
    expect(screen.getByRole('button', { name: '122 項目を取り込む' })).toBeInTheDocument();
    expect(
      screen.getByRole('switch', { name: /フォルダ構成を仮想フォルダとして再現/ }),
    ).toBeChecked();
    expect(screen.getByRole('switch', { name: /フォルダ名をタグとして付ける/ })).not.toBeChecked();
    expect(await screen.findByRole('option', { name: '写真 / 2026 旅行' })).toBeInTheDocument();
  });

  it('選んだオプションで取り込みを始めて閉じる', async () => {
    open();
    render(<ImportSheet />);
    await screen.findByRole('option', { name: '写真 / 2026 旅行' });

    fireEvent.change(screen.getByRole('combobox', { name: '取り込み先のフォルダ' }), {
      target: { value: '2' },
    });
    expect(screen.getByText('旅行2026 / 鎌倉 → 写真 / 2026 旅行 / 鎌倉')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('switch', { name: /フォルダ名をタグとして付ける/ }));
    fireEvent.change(screen.getByRole('combobox', { name: '重複の扱い' }), {
      target: { value: 'ignore' },
    });
    const tagInput = screen.getByRole('textbox', { name: 'すべてに付けるタグを追加' });
    fireEvent.change(tagInput, { target: { value: '旅行' } });
    fireEvent.keyDown(tagInput, { key: 'Enter' });

    fireEvent.click(screen.getByRole('button', { name: '122 項目を取り込む' }));
    await waitFor(() => expect(useImportStore.getState().phase).toBe('idle'));
    expect(startImport).toHaveBeenCalledWith(['/src/旅行2026'], {
      destFolderId: 2,
      reproduceFolders: true,
      folderNamesAsTags: true,
      unitsPerFolder: false,
      fileInfoTags: true,
      tags: ['旅行'],
      duplicates: 'ignore',
    });
  });

  it('ファイルだけのときはフォルダのオプションを出さない', () => {
    open({ ...PREVIEW, roots: [{ name: 'a.jpg', isDir: false }], subfolders: 0, sampleDirs: [] });
    render(<ImportSheet />);
    // フォルダのスイッチは出さず、ファイルの情報からタグだけ出す
    expect(screen.getAllByRole('switch')).toHaveLength(1);
    expect(screen.getByRole('switch', { name: /ファイルの情報からタグを付ける/ })).toBeChecked();
  });

  it('取り込めるものがない・確認に失敗した・esc で閉じる', () => {
    open({ ...PREVIEW, total: 0 });
    const { unmount } = render(<ImportSheet />);
    expect(
      screen.getByRole('dialog', { name: '取り込めるファイルがありません' }),
    ).toBeInTheDocument();
    unmount();

    open(null, '読めません');
    render(<ImportSheet />);
    expect(screen.getByRole('dialog')).toHaveTextContent('読めません');
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(useImportStore.getState().phase).toBe('idle');
  });
});
