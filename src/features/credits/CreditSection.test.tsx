import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import {
  type CreditSummaryDto,
  type LicenseDto,
  previewItemCredit,
  setCredits,
  summarizeCredits,
} from '../../ipc/credits';
import { itemProjects } from '../../ipc/projects';
import { useSettingsStore } from '../../stores/settingsStore';
import { CreditSection, licenseFlags, licenseOptions, previewWarnings } from './CreditSection';
import { useLicenseStore } from './licenseStore';

const t = ja.credit;

const ccBy: LicenseDto = {
  id: 2,
  builtin: 'cc-by-4.0',
  name: 'CC BY 4.0',
  spdx: 'CC-BY-4.0',
  url: 'https://creativecommons.org/licenses/by/4.0/',
  attribution: 'required',
  commercial: 'yes',
  modify: 'yes',
  shareAlike: false,
  note: null,
  items: 1,
  createdAt: 0,
  updatedAt: 0,
};

const none = { mixed: false, value: null };

function summary(patch: Partial<CreditSummaryDto> = {}): CreditSummaryDto {
  return {
    count: 1,
    licenseId: none,
    author: none,
    work: none,
    url: none,
    credit: none,
    ...patch,
  };
}

vi.mock('../../ipc/credits', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/credits')>()),
  summarizeCredits: vi.fn(),
  setCredits: vi.fn(async () => ({ items: 1, changed: 1 })),
  previewItemCredit: vi.fn(async () => ({ lines: [], unset: true, unsetSources: 0, lost: 0 })),
  listLicenses: vi.fn(async () => [ccBy]),
}));
vi.mock('../../ipc/projects', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/projects')>()),
  itemProjects: vi.fn(async () => []),
}));

beforeEach(() => {
  vi.mocked(summarizeCredits).mockReset();
  vi.mocked(setCredits).mockClear();
  useLicenseStore.setState({ list: [ccBy] });
  useSettingsStore.setState({ open: false, tab: 'general' });
});

describe('licenseFlags・licenseOptions', () => {
  it('ライセンスの印と選択肢（値がばらばらなら先頭に「複数の値」、末尾に管理）', () => {
    expect(licenseFlags({ ...ccBy, shareAlike: true })).toEqual([
      '表記が必要',
      '商用可',
      '改変可',
      '継承',
    ]);
    expect(licenseOptions([ccBy], false).map((o) => o.value)).toEqual(['unset', '2', 'manage']);
    const mixed = licenseOptions([ccBy], true);
    expect(mixed[0]).toMatchObject({ value: 'mixed', disabled: true });
  });
});

describe('CreditSection', () => {
  it('1 件：ライセンスを選ぶと付け、印と見本と使っているプロジェクトを出す', async () => {
    vi.mocked(summarizeCredits).mockResolvedValue(
      summary({ licenseId: { mixed: false, value: 2 } }),
    );
    vi.mocked(previewItemCredit).mockResolvedValueOnce({
      lines: ['「森」 もりの'],
      unset: false,
      unsetSources: 0,
      lost: 0,
    });
    vi.mocked(itemProjects).mockResolvedValueOnce([
      { projectId: 5, name: 'My Game', missing: false, paths: ['art/hero.png'] },
    ]);
    render(<CreditSection itemIds={[7]} unitIds={[]} source="https://example.com/hero" />);
    expect(await screen.findByText('表記が必要')).toBeInTheDocument();
    expect(await screen.findByText('「森」 もりの')).toBeInTheDocument();
    expect(await screen.findByRole('button', { name: 'My Game' })).toBeInTheDocument();
    fireEvent.change(screen.getByRole('combobox', { name: t.license }), {
      target: { value: 'unset' },
    });
    await waitFor(() =>
      expect(setCredits).toHaveBeenCalledWith([7], [], { license: { type: 'unset' } }),
    );
  });

  it('1 件・未設定：見本の代わりに注意を出し、文の欄は Enter で付ける', async () => {
    vi.mocked(summarizeCredits).mockResolvedValue(summary());
    render(<CreditSection itemIds={[7]} unitIds={[]} />);
    expect(await screen.findByText(t.previewUnset)).toBeInTheDocument();
    const author = screen.getByRole('textbox', { name: t.author });
    author.focus();
    fireEvent.change(author, { target: { value: '  もりの ' } });
    fireEvent.keyDown(author, { key: 'Enter' });
    await waitFor(() => expect(setCredits).toHaveBeenCalledWith([7], [], { author: 'もりの' }));
  });

  it('複数：値がばらばらの欄は「複数の値」で、空のまま離れても消さない。管理を選ぶと設定のタブ', async () => {
    vi.mocked(summarizeCredits).mockResolvedValue(
      summary({
        count: 3,
        licenseId: { mixed: true, value: null },
        author: { mixed: true, value: null },
      }),
    );
    render(<CreditSection itemIds={[1, 2]} unitIds={[9]} />);
    const author = await screen.findByRole('textbox', { name: t.author });
    expect(author).toHaveAttribute('placeholder', t.mixed);
    fireEvent.blur(author);
    expect(setCredits).not.toHaveBeenCalled();
    fireEvent.change(screen.getByRole('combobox', { name: t.license }), {
      target: { value: 'manage' },
    });
    expect(useSettingsStore.getState()).toMatchObject({ open: true, tab: 'licenses' });
    expect(setCredits).not.toHaveBeenCalled();
    fireEvent.change(screen.getByRole('combobox', { name: t.license }), { target: { value: '2' } });
    await waitFor(() =>
      expect(setCredits).toHaveBeenCalledWith([1, 2], [9], { license: { type: 'set', id: 2 } }),
    );
    // 複数では見本と使っているプロジェクトを出さない
    expect(screen.queryByText(t.preview)).toBeNull();
  });
});

describe('CreditSection（段 6 の検証の直し）', () => {
  it('IME の確定の Enter（keyCode 229）では決めず、変換中の Esc でも入力を戻さない', async () => {
    vi.mocked(summarizeCredits).mockResolvedValue(
      summary({ author: { mixed: false, value: 'A' } }),
    );
    render(<CreditSection itemIds={[7]} unitIds={[]} />);
    const author = await screen.findByRole('textbox', { name: t.author });
    author.focus();
    fireEvent.change(author, { target: { value: 'A 森野' } });
    fireEvent.keyDown(author, { key: 'Enter', keyCode: 229 });
    expect(setCredits).not.toHaveBeenCalled();
    expect(author).toHaveFocus();
    fireEvent.keyDown(author, { key: 'Escape', keyCode: 229, isComposing: true });
    expect(author).toHaveValue('A 森野');
  });

  it('値がばらばらの欄は空白だけで離れても消さない', async () => {
    vi.mocked(summarizeCredits).mockResolvedValue(
      summary({ count: 3, author: { mixed: true, value: null } }),
    );
    render(<CreditSection itemIds={[1, 2, 3]} unitIds={[]} />);
    const author = await screen.findByRole('textbox', { name: t.author });
    fireEvent.change(author, { target: { value: '\u3000 ' } });
    fireEvent.blur(author);
    expect(setCredits).not.toHaveBeenCalled();
  });

  it('対象が 0 件（メンバーのいないユニット）なら欄を押せなくして案内する', async () => {
    vi.mocked(summarizeCredits).mockResolvedValue(
      summary({ count: 0, licenseId: { mixed: true, value: null } }),
    );
    render(<CreditSection itemIds={[]} unitIds={[9]} />);
    expect(await screen.findByText(t.empty)).toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: t.license })).toBeDisabled();
    expect(screen.getByRole('textbox', { name: t.author })).toBeDisabled();
  });

  it('エディタの中（navigable が偽）では使っているプロジェクトを押せない名前で出す', async () => {
    vi.mocked(summarizeCredits).mockResolvedValue(summary());
    vi.mocked(itemProjects).mockResolvedValueOnce([
      { projectId: 5, name: 'My Game', missing: false, paths: ['a.png'] },
    ]);
    render(<CreditSection itemIds={[7]} unitIds={[]} navigable={false} />);
    expect(await screen.findByText('My Game')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'My Game' })).toBeNull();
  });

  it('見本の注意：自身の未設定・来歴の元の未設定・消えた元', () => {
    expect(previewWarnings({ lines: [], unset: true, unsetSources: 0, lost: 0 })).toEqual([
      t.previewUnset,
    ]);
    expect(previewWarnings({ lines: ['a'], unset: false, unsetSources: 2, lost: 1 })).toEqual([
      t.unsetSources('2'),
      t.lost('1'),
    ]);
    expect(previewWarnings({ lines: ['a'], unset: false, unsetSources: 0, lost: 0 })).toEqual([]);
  });
});
