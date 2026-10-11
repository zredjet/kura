import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { ToolInfoDto } from '../../ipc/downloads';
import { ToolsPanel } from './ToolsPanel';

const tool = (name: string, path: string | null): ToolInfoDto => ({
  name,
  path,
  version: path ? '1.0' : null,
  homebrew: path !== null,
  overridden: false,
});

vi.mock('../../ipc/downloads', () => ({
  toolsStatus: vi.fn(async () => [
    tool('yt-dlp', '/opt/homebrew/bin/yt-dlp'),
    tool('gallery-dl', null),
    tool('deno', null),
    tool('ffmpeg', '/opt/homebrew/bin/ffmpeg'),
    tool('ffprobe', '/opt/homebrew/bin/ffprobe'),
    tool('mpv', null),
  ]),
  getDownloadPrefs: vi.fn(async () => ({ cookies: 'firefox', rules: [] })),
  setDownloadPrefs: vi.fn(),
  setToolPath: vi.fn(),
  updateTools: vi.fn(),
}));

describe('ToolsPanel', () => {
  it('shows how to install missing tools (mpv is optional)', async () => {
    render(<ToolsPanel />);
    const t = ja.downloads;
    expect(await screen.findByText('gallery-dl')).toBeTruthy();
    expect(screen.getAllByText(t.tools.missing)).toHaveLength(3);
    expect(screen.getByText(t.cookies.firefox)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: t.tools.install }));
    expect(screen.getByText(t.tools.installHint)).toBeTruthy();
    // 要るものだけ（任意の mpv は入れない）
    expect(screen.getByText('brew install gallery-dl deno')).toBeTruthy();
  });
});
