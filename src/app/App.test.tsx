import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../i18n/ja';
import { restoreLibrary } from '../ipc/library';
import { useLibraryStore } from '../stores/libraryStore';
import { App } from './App';

vi.mock('../ipc/library', () => ({
  restoreLibrary: vi.fn(),
  libraryInfo: vi.fn(),
  createLibrary: vi.fn(),
  openLibrary: vi.fn(),
  checkLocation: vi.fn(async (path: string) => ({ path, status: 'new', syncService: null })),
}));
vi.mock('../ipc/jobs', () => ({
  listJobs: vi.fn(async () => []),
  onJobProgress: vi.fn(async () => () => {}),
  progressFromJob: vi.fn(),
  cancelJob: vi.fn(),
  pauseJob: vi.fn(),
  resumeJob: vi.fn(),
}));
vi.mock(import('../ipc/items'), async (importOriginal) => ({
  ...(await importOriginal()),
  queryIds: vi.fn(async () => ({ ids: new Uint32Array(), aspects: new Uint16Array(), groups: [] })),
  getItems: vi.fn(async () => []),
  getItemDetail: vi.fn(),
  libraryCounts: vi.fn(async () => ({
    all: 12480,
    recent: 0,
    unsorted: 0,
    untagged: 0,
    trash: 0,
    captures: 0,
  })),
}));
vi.mock('../ipc/folders', () => ({
  foldersTree: vi.fn(async () => []),
}));
vi.mock('../ipc/search', () => ({
  smartFolders: vi.fn(async () => []),
  queryParse: vi.fn(),
  querySuggest: vi.fn(async () => []),
}));
vi.mock('@tauri-apps/api/path', () => ({
  homeDir: async () => '/Users/me',
  pictureDir: async () => '/Users/me/Pictures',
  join: async (...parts: string[]) => parts.join('/'),
}));

const info = {
  name: 'Kura Library',
  path: '/Users/me/Pictures/Kura Library.kuralib',
  itemCount: 12480,
  integrity: null,
};

beforeEach(() => {
  useLibraryStore.setState({ status: 'loading', info: null, missingPath: null });
  vi.mocked(restoreLibrary).mockReset();
});

describe('App（前回のライブラリを開けたとき）', () => {
  beforeEach(() => {
    vi.mocked(restoreLibrary).mockResolvedValue({ info, lastPath: info.path });
  });

  it('サイドバー・ツールバー・グリッド領域・インスペクタがある', async () => {
    render(<App />);

    expect(await screen.findByRole('navigation', { name: ja.sidebar.label })).toBeInTheDocument();
    expect(screen.getByRole('main')).toBeInTheDocument();
    expect(
      screen.getByRole('heading', { level: 1, name: ja.library.allItems }),
    ).toBeInTheDocument();
    expect(screen.getByText('12,480 項目 · 追加日の新しい順')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: ja.library.importFiles })).toBeInTheDocument();
    expect(screen.getByRole('region', { name: ja.library.gridLabel })).toBeInTheDocument();
    expect(
      screen.getByRole('complementary', { name: ja.library.inspectorLabel }),
    ).toBeInTheDocument();
  });

  it('サイドバーの先頭行とツールバーがウィンドウのドラッグ領域になっている', async () => {
    render(<App />);

    const nav = await screen.findByRole('navigation', { name: ja.sidebar.label });
    expect(nav.firstElementChild).toHaveAttribute('data-tauri-drag-region');

    const heading = screen.getByRole('heading', { level: 1 });
    expect(heading).toHaveAttribute('data-tauri-drag-region');
    expect(heading.closest('header')).toHaveAttribute('data-tauri-drag-region');
  });
});

describe('App（ライブラリがないとき）', () => {
  it('初回起動の画面を出し、前回の場所が見つからなければ知らせる', async () => {
    vi.mocked(restoreLibrary).mockResolvedValue({
      info: null,
      lastPath: '/Volumes/SSD/Kura Library.kuralib',
    });
    render(<App />);

    expect(
      await screen.findByRole('heading', { level: 1, name: ja.welcome.title }),
    ).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('/Volumes/SSD/Kura Library.kuralib');
  });
});
