import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { libraryCacheUsage } from '../../ipc/library';
import { useLibraryStore } from '../../stores/libraryStore';
import { useSettingsStore } from '../../stores/settingsStore';
import { useViewStore } from '../../stores/viewStore';
import { updatePlayerPrefs } from '../player/playerActions';
import { SettingsSheet } from './SettingsSheet';

vi.mock('../player/playerActions', () => ({ updatePlayerPrefs: vi.fn() }));
vi.mock('../downloads/ToolsPanel', () => ({ ToolsPanel: () => <div>外部ツールの欄</div> }));
vi.mock('../../ipc/downloads', () => ({
  downloadArchives: vi.fn(async () => [{ tool: 'ytDlp', entries: 12, sizeBytes: 300 }]),
  clearDownloadArchive: vi.fn(async () => null),
  toolCommand: () => 'yt-dlp',
}));
vi.mock('../../ipc/library', () => ({
  listBackups: vi.fn(async () => []),
  backupNow: vi.fn(),
  checkLibrary: vi.fn(),
  libraryUsage: vi.fn(async () => ({
    libraryBytes: 1_200_000,
    volumeTotalBytes: 8_000_000,
    volumeAvailableBytes: 3_000_000,
  })),
  revealPath: vi.fn(),
  libraryInfo: vi.fn(async () => ({ integrity: null })),
  libraryCacheUsage: vi.fn(async () => ({
    thumbsBytes: 2_500_000,
    thumbsFiles: 1_234,
    peaksBytes: 34_000,
    peaksFiles: 3,
    animBytes: 0,
    animFiles: 0,
  })),
}));
vi.mock('@tauri-apps/api/path', () => ({ homeDir: async () => '/Users/me' }));

const t = ja.settings;

describe('SettingsSheet', () => {
  beforeEach(() => {
    useSettingsStore.setState({ open: true, tab: 'general' });
    useViewStore.setState({ collapseUnits: false });
  });

  it('閉じているときは何も出さない', () => {
    useSettingsStore.setState({ open: false });
    const { container } = render(<SettingsSheet />);
    expect(container).toBeEmptyDOMElement();
  });

  it('一般：画像の表示時間を変え、まとめて表示を切り替える', () => {
    render(<SettingsSheet />);
    expect(screen.getByRole('heading', { name: t.title })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(t.imageDuration), { target: { value: '10' } });
    expect(updatePlayerPrefs).toHaveBeenCalledWith({ imageDurationMs: 10_000 });
    fireEvent.click(screen.getByRole('switch', { name: ja.library.collapseUnits }));
    expect(useViewStore.getState().collapseUnits).toBe(true);
  });

  it('ダウンロード：外部ツールの欄と、取得済みの記録の件数', async () => {
    render(<SettingsSheet />);
    fireEvent.click(screen.getByRole('button', { name: t.tabs.downloads }));
    expect(screen.getByText('外部ツールの欄')).toBeInTheDocument();
    expect(await screen.findByText(t.archiveEntries('12'))).toBeInTheDocument();
  });

  it('ライブラリ：容量に、サムネイル・波形・アニメのキャッシュを別の行で出す（数えている間はそう出す）', async () => {
    useLibraryStore.setState({
      info: { name: 'Kura', path: '/Users/me/Kura.kuralib', itemCount: 3, integrity: null },
    });
    useSettingsStore.setState({ open: true, tab: 'library' });
    let finish: () => void = () => undefined;
    vi.mocked(libraryCacheUsage).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = () =>
            resolve({
              thumbsBytes: 2_500_000,
              thumbsFiles: 1_234,
              peaksBytes: 34_000,
              peaksFiles: 3,
              animBytes: 0,
              animFiles: 0,
            });
        }),
    );
    render(<SettingsSheet />);
    const m = ja.mediaView;
    const row = (label: string) => screen.getByText(label).nextElementSibling?.textContent;
    expect(row(m.cacheThumbs)).toBe(m.cacheCounting);
    expect(row(m.cachePeaks)).toBe(m.cacheCounting);
    finish();
    expect(await screen.findByText(m.cacheValue('2.5 MB', '1,234'))).toBeInTheDocument();
    expect(row(m.cachePeaks)).toBe(m.cacheValue('34.0 KB', '3'));
    expect(row(m.cacheAnim)).toBe(m.cacheValue('0 B', '0'));
    // ライブラリのファイルの行は別
    expect(row(t.libraryBytes)).not.toBe(row(m.cacheThumbs));
  });

  it('キャッシュを数えられなければ —', async () => {
    useLibraryStore.setState({
      info: { name: 'Kura', path: '/Users/me/Kura.kuralib', itemCount: 3, integrity: null },
    });
    useSettingsStore.setState({ open: true, tab: 'library' });
    vi.mocked(libraryCacheUsage).mockRejectedValueOnce(new Error('読めません'));
    render(<SettingsSheet />);
    const label = screen.getByText(ja.mediaView.cacheAnim);
    await vi.waitFor(() => expect(label.nextElementSibling?.textContent).toBe('—'));
  });

  it('完了で閉じる', () => {
    render(<SettingsSheet />);
    fireEvent.click(screen.getByRole('button', { name: t.done }));
    expect(useSettingsStore.getState().open).toBe(false);
  });
});
