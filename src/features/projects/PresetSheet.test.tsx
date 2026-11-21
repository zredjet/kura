import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { ExportPresetDto } from '../../ipc/export';
import { previewExportNames } from '../../ipc/export';
import { setProjectSettings } from '../../ipc/projects';
import { PresetSheet } from './PresetSheet';
import { openPresetSheet, usePresetSheetStore } from './presetSheetStore';
import { useProjectStore } from './projectStore';

const godot: ExportPresetDto = {
  name: { template: null, style: 'snake', ascii: false },
  image: { type: 'playable' },
  audio: { type: 'playable' },
  manifest: false,
};

vi.mock('../../ipc/export', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/export')>()),
  exportFormats: vi.fn(async () => ({ image: ['png'], audio: ['ogg'] })),
  previewExportNames: vi.fn(async (entries: number[]) => ({
    names: entries.slice(0, 50).map((id) => ({ itemId: id, name: `a_${id}.png`, convert: null })),
    total: entries.length,
  })),
}));
vi.mock('../../ipc/projects', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/projects')>()),
  projectSettings: vi.fn(async () => godot),
  setProjectSettings: vi.fn(async () => null),
  projectItems: vi.fn(async () => Array.from({ length: 80 }, (_, i) => i + 1)),
}));

beforeEach(() => {
  usePresetSheetStore.setState({ project: null });
  useProjectStore.setState({
    list: [
      {
        id: 5,
        name: 'My Game',
        root: '/g',
        engine: 'godot',
        missing: false,
        scannedAt: 1,
        files: 80,
        items: 80,
        createdAt: 0,
        updatedAt: 0,
      },
    ],
  });
});

describe('持ち出しの設定のシート', () => {
  it('使っている素材すべてで見本を作り（「ほか N 件」は全体から）、保存する', async () => {
    openPresetSheet(5);
    render(<PresetSheet />);
    expect(await screen.findByText(ja.presetSheet.title('My Game'))).toBeInTheDocument();
    expect(await screen.findByText(ja.exportOptions.previewMore('74'))).toBeInTheDocument();
    expect(vi.mocked(previewExportNames).mock.calls[0]?.[0]).toHaveLength(80);
    fireEvent.click(screen.getByRole('button', { name: ja.exportOptions.styles.pascal }));
    fireEvent.click(screen.getByRole('button', { name: ja.presetSheet.save }));
    await waitFor(() =>
      expect(setProjectSettings).toHaveBeenCalledWith(5, {
        ...godot,
        name: { ...godot.name, style: 'pascal' },
      }),
    );
    expect(usePresetSheetStore.getState().project).toBeNull();
  });
});
