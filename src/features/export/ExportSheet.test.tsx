import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { pickExportFolder } from '../../ipc/dialogs';
import { type ExportPresetDto, previewExportNames, startExport } from '../../ipc/export';
import { type ProjectDto, projectForDest, projectSettings } from '../../ipc/projects';
import { ExportSheet } from './ExportSheet';
import { targetOf, targetOptions, targetValue } from './ExportOptionsFields';
import {
  closeExportSheet,
  isPreset,
  lastPreset,
  M9_PRESET,
  openExportSheet,
  rememberPreset,
  useExportSheetStore,
} from './exportSheetStore';

const godot: ExportPresetDto = {
  name: { template: null, style: 'snake', ascii: false },
  image: { type: 'playable' },
  audio: { type: 'playable' },
  manifest: false,
};

const project: ProjectDto = {
  id: 5,
  name: 'My Game',
  root: '/Users/me/game',
  engine: 'godot',
  missing: false,
  scannedAt: 1,
  files: 3,
  items: 2,
  createdAt: 0,
  updatedAt: 0,
};

vi.mock('../../ipc/dialogs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/dialogs')>()),
  pickExportFolder: vi.fn(),
}));
vi.mock('../../ipc/export', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/export')>()),
  exportFormats: vi.fn(async () => ({ image: ['png', 'webp'], audio: ['wav', 'ogg'] })),
  previewExportNames: vi.fn(async (entries: number[]) => ({
    names: entries.map((id) => ({ itemId: id, name: `item_${id}.png`, convert: null })),
    total: entries.length,
  })),
  startExport: vi.fn(async () => 1),
}));
vi.mock('../../ipc/projects', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/projects')>()),
  projectForDest: vi.fn(async () => null),
  projectSettings: vi.fn(async () => godot),
}));

beforeEach(() => {
  localStorage.clear();
  closeExportSheet();
  vi.mocked(startExport).mockClear();
  vi.mocked(projectForDest).mockResolvedValue(null);
});

describe('形式の選択の値', () => {
  it('行き来でき、表にない形式は選択肢に足す', () => {
    for (const target of [
      { type: 'original' } as const,
      { type: 'playable' } as const,
      { type: 'to', format: 'webp' } as const,
    ]) {
      expect(targetOf(targetValue(target))).toEqual(target);
    }
    const options = targetOptions(['png'], { type: 'to', format: 'gif' }).map((o) => o.value);
    expect(options).toEqual(['original', 'playable', 'to:png', 'to:gif']);
  });
});

describe('前回の名前と形式', () => {
  it('覚えたものを読み、読めなければ M9', () => {
    expect(lastPreset()).toEqual(M9_PRESET);
    rememberPreset(godot);
    expect(lastPreset()).toEqual(godot);
    localStorage.setItem('kura.export.lastPreset', '{"name":1}');
    expect(lastPreset()).toEqual(M9_PRESET);
    // 項目ごとに確かめる（型の違う値・知らない書き方・形式の形の違い）
    expect(isPreset({ ...godot, name: { ...godot.name, style: 'upper' } })).toBe(false);
    expect(isPreset({ ...godot, image: { type: 'to' } })).toBe(false);
    expect(isPreset({ ...godot, name: { ...godot.name, template: 3 } })).toBe(false);
    expect(isPreset(godot)).toBe(true);
  });
});

describe('書き出しのシート', () => {
  it('書き出し先を選ぶまで書き出せず、外への書き出しは前回の設定を覚える', async () => {
    vi.mocked(pickExportFolder).mockResolvedValueOnce('/Users/me/out');
    openExportSheet([1, 2]);
    render(<ExportSheet />);
    const start = screen.getByRole('button', { name: ja.exportSheet.start });
    expect(start).toBeDisabled();
    expect(await screen.findByText('item_1.png')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: ja.exportSheet.choose }));
    expect(await screen.findByText('/Users/me/out')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: ja.exportOptions.styles.kebab }));
    fireEvent.click(start);
    await waitFor(() =>
      expect(startExport).toHaveBeenCalledWith('/Users/me/out', [1, 2], {
        ...M9_PRESET,
        name: { ...M9_PRESET.name, style: 'kebab' },
        project: null,
      }),
    );
    expect(useExportSheetStore.getState().entries).toBeNull();
    expect(lastPreset().name.style).toBe('kebab');
  });

  it('プロジェクトの中を選ぶとその既定を入れ、プロジェクトとして書き出す（覚えない）', async () => {
    vi.mocked(pickExportFolder).mockResolvedValueOnce('/Users/me/game/art');
    vi.mocked(projectForDest).mockResolvedValueOnce(project);
    openExportSheet([3]);
    render(<ExportSheet />);
    fireEvent.click(screen.getByRole('button', { name: ja.exportSheet.choose }));
    expect(await screen.findByText(ja.exportSheet.projectHint('My Game'))).toBeInTheDocument();
    expect(projectSettings).toHaveBeenCalledWith(5);
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'snake_case' })).toHaveAttribute(
        'aria-pressed',
        'true',
      ),
    );
    fireEvent.click(screen.getByRole('button', { name: ja.exportSheet.start }));
    await waitFor(() =>
      expect(startExport).toHaveBeenCalledWith('/Users/me/game/art', [3], {
        ...godot,
        project: 5,
      }),
    );
    expect(lastPreset()).toEqual(M9_PRESET);
  });

  it('テンプレートは打ってすぐ書き出しても入る', async () => {
    vi.mocked(pickExportFolder).mockResolvedValueOnce('/Users/me/out');
    openExportSheet([1]);
    render(<ExportSheet />);
    fireEvent.click(screen.getByRole('button', { name: ja.exportSheet.choose }));
    await screen.findByText('/Users/me/out');
    fireEvent.change(screen.getByRole('textbox', { name: ja.exportOptions.template }), {
      target: { value: '{番号}' },
    });
    fireEvent.click(screen.getByRole('button', { name: ja.exportSheet.start }));
    await waitFor(() =>
      expect(startExport).toHaveBeenCalledWith('/Users/me/out', [1], {
        ...M9_PRESET,
        name: { ...M9_PRESET.name, template: '{番号}' },
        project: null,
      }),
    );
  });

  it('同じプロジェクトの中で選び直しても、シートで変えた値は残す', async () => {
    vi.mocked(pickExportFolder)
      .mockResolvedValueOnce('/Users/me/game/art')
      .mockResolvedValueOnce('/Users/me/game/sfx');
    vi.mocked(projectForDest).mockResolvedValue(project);
    vi.mocked(projectSettings).mockClear();
    openExportSheet([3]);
    render(<ExportSheet />);
    fireEvent.click(screen.getByRole('button', { name: ja.exportSheet.choose }));
    await screen.findByText(ja.exportSheet.projectHint('My Game'));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'snake_case' })).toHaveAttribute(
        'aria-pressed',
        'true',
      ),
    );
    fireEvent.click(screen.getByRole('button', { name: ja.exportOptions.styles.kebab }));
    fireEvent.click(screen.getByRole('button', { name: ja.exportSheet.choose }));
    await screen.findByText('/Users/me/game/sfx');
    expect(projectSettings).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: ja.exportOptions.styles.kebab })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
  });

  it('見本の件数はジョブと同じ数え方（ユニットのメンバーを含む）', async () => {
    vi.mocked(previewExportNames).mockResolvedValueOnce({ names: [], total: 12 });
    openExportSheet([1, 2]);
    render(<ExportSheet />);
    expect(await screen.findByText(ja.exportSheet.count('12'))).toBeInTheDocument();
  });

  it('テンプレートが読めなければ見本の代わりに理由を出す', async () => {
    vi.mocked(previewExportNames).mockRejectedValue(new Error('知らない語です: {なに}'));
    openExportSheet([1]);
    render(<ExportSheet />);
    expect(await screen.findByText('知らない語です: {なに}')).toBeInTheDocument();
  });
});
