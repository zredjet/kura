import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { askConfirm } from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import { createLicense, deleteLicense, type LicenseDto } from '../../ipc/credits';
import { LicensesTab } from './LicensesTab';
import { useLicenseStore } from './licenseStore';

const t = ja.licenses;

function license(patch: Partial<LicenseDto>): LicenseDto {
  return {
    id: 1,
    builtin: 'cc0-1.0',
    name: 'CC0 1.0',
    spdx: 'CC0-1.0',
    url: null,
    attribution: 'none',
    commercial: 'yes',
    modify: 'yes',
    shareAlike: false,
    note: null,
    items: 0,
    createdAt: 0,
    updatedAt: 0,
    ...patch,
  };
}

const builtin = license({});
const site = license({
  id: 14,
  builtin: null,
  name: 'サイトX の規約',
  attribution: 'required',
  items: 3,
});

vi.mock('../../ipc/credits', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/credits')>()),
  listLicenses: vi.fn(async () => [builtin, site]),
  createLicense: vi.fn(async () => 15),
  deleteLicense: vi.fn(async () => 3),
}));
vi.mock('../../components/Overlay/overlayStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../components/Overlay/overlayStore')>()),
  askConfirm: vi.fn(async () => true),
}));

beforeEach(() => {
  useLicenseStore.setState({ list: [builtin, site] });
  vi.mocked(createLicense).mockClear();
  vi.mocked(deleteLicense).mockClear();
});

describe('設定の「ライセンス」タブ', () => {
  it('既定の行は変えられず、利用者の行の操作は名前で読み上げる。削除は件数を言って確かめる', async () => {
    render(<LicensesTab />);
    expect(screen.queryByRole('button', { name: t.editLabel('CC0 1.0') })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: t.deleteLabel('サイトX の規約') }));
    await waitFor(() =>
      expect(askConfirm).toHaveBeenCalledWith(
        expect.objectContaining({ message: t.deleteMessage('サイトX の規約', '3') }),
      ),
    );
    await waitFor(() => expect(deleteLicense).toHaveBeenCalledWith(14));
  });

  it('足すシートは body に出し（設定のガラスの外）、名前がなければ保存できない', async () => {
    const view = render(<LicensesTab />);
    fireEvent.click(screen.getByRole('button', { name: t.add }));
    const dialog = await screen.findByRole('dialog');
    expect(view.container.contains(dialog)).toBe(false);
    const save = within(dialog).getByRole('button', { name: t.save });
    expect(save).toBeDisabled();
    fireEvent.change(within(dialog).getByRole('textbox', { name: t.name }), {
      target: { value: 'BOOTH の規約' },
    });
    fireEvent.click(within(dialog).getByRole('button', { name: t.attributionShort.optional }));
    fireEvent.click(save);
    await waitFor(() =>
      expect(createLicense).toHaveBeenCalledWith(
        expect.objectContaining({ name: 'BOOTH の規約', attribution: 'optional' }),
      ),
    );
  });
});
