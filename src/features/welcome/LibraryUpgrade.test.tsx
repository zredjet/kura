import { act, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { receiveUpgrade, settleUpgrade, UPGRADE_NOTICE_MS } from '../../stores/upgradeStore';
import { LibraryUpgrade } from './LibraryUpgrade';

vi.mock('../../ipc/library', () => ({
  onLibraryUpgrade: vi.fn(async () => () => {}),
}));

const started = {
  phase: 'started',
  path: '/L.kuralib',
  fromVersion: 6,
  toVersion: 9,
  dbBytes: 51_834_880,
  elapsedMs: null,
  error: null,
} as const;

beforeEach(() => {
  vi.useFakeTimers();
  settleUpgrade();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('ライブラリの更新の表示', () => {
  it('始まってすぐは出さず、300ms を超えたら窓の中央に出し、終わったら消す', () => {
    render(<LibraryUpgrade />);
    act(() => receiveUpgrade(started));
    expect(screen.queryByRole('status')).toBeNull();

    act(() => vi.advanceTimersByTime(UPGRADE_NOTICE_MS));
    expect(screen.getByRole('status')).toHaveTextContent(ja.libraryOpen.upgrading);
    // 割合は分からないので、値のある進捗バーにしない（溝だけ）
    expect(screen.queryByRole('progressbar')).toBeNull();

    act(() => receiveUpgrade({ ...started, phase: 'finished', dbBytes: null, elapsedMs: 1961 }));
    expect(screen.queryByRole('status')).toBeNull();
  });

  it('窓を動かせる（地をドラッグの領域にする）', () => {
    render(<LibraryUpgrade />);
    act(() => {
      receiveUpgrade(started);
      vi.advanceTimersByTime(UPGRADE_NOTICE_MS);
    });
    expect(screen.getByRole('status').parentElement).toHaveAttribute('data-tauri-drag-region');
  });
});
