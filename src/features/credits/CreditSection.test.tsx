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
import { CreditSection, licenseFlags, licenseOptions } from './CreditSection';
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
  previewItemCredit: vi.fn(async () => []),
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
    vi.mocked(previewItemCredit).mockResolvedValueOnce(['「森」 もりの']);
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
