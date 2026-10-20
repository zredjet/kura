import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { useSettingsStore } from '../../stores/settingsStore';
import { useViewStore } from '../../stores/viewStore';
import { updatePlayerPrefs } from '../player/playerActions';
import { DEFAULT_PREFS, usePlayerStore } from '../player/playerStore';
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

  it('一般：選んだ短い音声をすぐ鳴らす（既定でオン。切ると sfxAudition を保存する。M11）', () => {
    usePlayerStore.setState({ prefs: DEFAULT_PREFS });
    render(<SettingsSheet />);
    const sfx = screen.getByRole('switch', { name: ja.asset.sfx });
    expect(sfx).toBeChecked();
    expect(screen.getByText(ja.asset.sfxHint)).toBeInTheDocument();
    fireEvent.click(sfx);
    expect(updatePlayerPrefs).toHaveBeenCalledWith({ sfxAudition: false });
  });

  it('ダウンロード：外部ツールの欄と、取得済みの記録の件数', async () => {
    render(<SettingsSheet />);
    fireEvent.click(screen.getByRole('button', { name: t.tabs.downloads }));
    expect(screen.getByText('外部ツールの欄')).toBeInTheDocument();
    expect(await screen.findByText(t.archiveEntries('12'))).toBeInTheDocument();
  });

  it('完了で閉じる', () => {
    render(<SettingsSheet />);
    fireEvent.click(screen.getByRole('button', { name: t.done }));
    expect(useSettingsStore.getState().open).toBe(false);
  });
});
