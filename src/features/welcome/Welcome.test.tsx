import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { checkLocation, createLibrary, openLibrary } from '../../ipc/library';
import { IpcError } from '../../ipc/result';
import { useLibraryStore } from '../../stores/libraryStore';
import { Welcome } from './Welcome';

vi.mock('@tauri-apps/api/path', () => ({
  homeDir: async () => '/Users/me',
  pictureDir: async () => '/Users/me/Pictures',
  join: async (...parts: string[]) => parts.join('/'),
}));
vi.mock('../../ipc/dialogs', () => ({
  pickNewLibraryLocation: vi.fn(
    async () => '/Users/me/Library/Mobile Documents/com~apple~CloudDocs/K',
  ),
  pickExistingLibrary: vi.fn(async () => null),
}));
vi.mock('../../ipc/library', () => ({
  checkLocation: vi.fn(),
  createLibrary: vi.fn(),
  openLibrary: vi.fn(),
  restoreLibrary: vi.fn(),
  libraryInfo: vi.fn(),
  onLibraryUpgrade: vi.fn(async () => () => {}),
}));

const DEFAULT = '/Users/me/Pictures/Kura Library.kuralib';

beforeEach(() => {
  useLibraryStore.setState({ status: 'none', info: null, missingPath: null, restoreError: null });
  vi.mocked(checkLocation).mockImplementation(async (path) => ({
    path: path.endsWith('.kuralib') ? path : `${path}.kuralib`,
    status: 'new',
    syncService: path.includes('Mobile Documents') ? 'iCloud Drive' : null,
  }));
  vi.mocked(createLibrary).mockResolvedValue({
    name: 'Kura Library',
    path: DEFAULT,
    itemCount: 0,
    integrity: null,
  });
});

describe('Welcome', () => {
  it('既定の場所に新しいライブラリを作る', async () => {
    render(<Welcome />);
    expect(await screen.findByText('~/Pictures/Kura Library.kuralib')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: '新しいライブラリを作る' }));
    await waitFor(() => expect(useLibraryStore.getState().status).toBe('open'));
    expect(createLibrary).toHaveBeenCalledWith(DEFAULT);
  });

  it('同期フォルダを選んだら警告する', async () => {
    render(<Welcome />);
    await screen.findByText('~/Pictures/Kura Library.kuralib');
    fireEvent.click(screen.getByRole('button', { name: '場所を変更…' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('iCloud Drive の中です');
  });

  it('既存のライブラリがある場所なら「開く」にする', async () => {
    vi.mocked(checkLocation).mockResolvedValue({
      path: DEFAULT,
      status: 'existingLibrary',
      syncService: null,
    });
    render(<Welcome />);
    expect(await screen.findByRole('button', { name: 'このライブラリを開く' })).toBeEnabled();
  });
});

describe('Welcome（ライブラリを開けなかったとき。M11）', () => {
  it('前回のライブラリを開けなかった理由を、見出し・説明・場所で出す（見つからないとは言わない）', async () => {
    const message =
      'ディスクの空き容量が足りないため、ライブラリを新しい版にできません。あと約 120 MB の空きを作ってから開き直してください（ライブラリは変えていません）';
    useLibraryStore.setState({
      restoreError: { path: '/Users/me/Pictures/L.kuralib', error: { code: 'NoSpace', message } },
    });
    render(<Welcome />);
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(ja.libraryOpen.failure.NoSpace);
    expect(alert).toHaveTextContent(message);
    expect(alert).toHaveTextContent('~/Pictures/L.kuralib');
    expect(screen.queryByText(/前回のライブラリが見つかりません/)).toBeNull();
  });

  it('既存のライブラリを開いて、新しい版の Kura のものだったら理由を出す', async () => {
    vi.mocked(checkLocation).mockResolvedValue({
      path: DEFAULT,
      status: 'existingLibrary',
      syncService: null,
    });
    vi.mocked(openLibrary).mockRejectedValue(
      new IpcError({
        code: 'LibraryTooNew',
        message: 'このライブラリは新しい版の Kura で作られています（スキーマ 12）',
      }),
    );
    render(<Welcome />);
    fireEvent.click(await screen.findByRole('button', { name: 'このライブラリを開く' }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(ja.libraryOpen.failure.LibraryTooNew);
    expect(alert).toHaveTextContent('スキーマ 12');
  });
});
