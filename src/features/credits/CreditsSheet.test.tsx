import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { DEFAULT_CREDITS_RENDER, renderCredits, saveCredits } from '../../ipc/credits';
import { pickCreditsPath } from '../../ipc/dialogs';
import { useProjectStore } from '../projects/projectStore';
import { CreditsSheet } from './CreditsSheet';
import { closeCreditsSheet, openCreditsSheet } from './creditsSheetStore';

const t = ja.creditsSheet;

vi.mock('../../ipc/credits', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/credits')>()),
  renderCredits: vi.fn(async (_scope: unknown, options: { lang: string }) => ({
    text: options.lang === 'en' ? '# Credits\n' : '# クレジット\n',
    lines: 2,
    omittedOptional: 1,
    omittedNone: 0,
    warnings: 1,
  })),
  saveCredits: vi.fn(async () => ({
    text: '',
    lines: 2,
    omittedOptional: 0,
    omittedNone: 0,
    warnings: 0,
  })),
}));
vi.mock('../../ipc/dialogs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/dialogs')>()),
  pickCreditsPath: vi.fn(async () => '/Users/me/game/CREDITS.md'),
}));

beforeEach(() => {
  localStorage.clear();
  closeCreditsSheet();
  vi.mocked(renderCredits).mockClear();
  useProjectStore.setState({
    list: [
      {
        id: 5,
        name: 'My Game',
        root: '/Users/me/game',
        engine: 'godot',
        missing: false,
        scannedAt: 1,
        files: 1,
        items: 1,
        createdAt: 0,
        updatedAt: 0,
      },
    ],
  });
});

describe('クレジットの一覧のシート', () => {
  it('既定で作って見本と数を出し、言葉を変えると作り直して覚える', async () => {
    openCreditsSheet({ type: 'project', id: 5 }, 'My Game');
    render(<CreditsSheet />);
    expect(screen.getByText(t.scopeProject('My Game'))).toBeInTheDocument();
    expect(await screen.findByText(/# クレジット/)).toBeInTheDocument();
    expect(renderCredits).toHaveBeenCalledWith({ type: 'project', id: 5 }, DEFAULT_CREDITS_RENDER);
    expect(screen.getByText(t.warnings('1'))).toBeInTheDocument();
    expect(screen.getByText(t.omitted('1'))).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.langs.en }));
    expect(await screen.findByText(/# Credits/)).toBeInTheDocument();
    expect(JSON.parse(localStorage.getItem('kura.credits.options') ?? '{}')).toMatchObject({
      lang: 'en',
    });
  });

  it('保存はプロジェクトのフォルダの CREDITS.md を既定にする', async () => {
    openCreditsSheet({ type: 'project', id: 5 }, 'My Game');
    render(<CreditsSheet />);
    await screen.findByText(/# クレジット/);
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await waitFor(() =>
      expect(pickCreditsPath).toHaveBeenCalledWith(t.saveTitle, '/Users/me/game/CREDITS.md', true),
    );
    await waitFor(() =>
      expect(saveCredits).toHaveBeenCalledWith(
        '/Users/me/game/CREDITS.md',
        { type: 'project', id: 5 },
        DEFAULT_CREDITS_RENDER,
      ),
    );
  });

  it('選んだものの範囲は件数で言う', async () => {
    openCreditsSheet({ type: 'items', itemIds: [1, 2], unitIds: [3] });
    render(<CreditsSheet />);
    expect(screen.getByText(t.scopeItems('3'))).toBeInTheDocument();
    await screen.findByText(/# クレジット/);
  });
});
