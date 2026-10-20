import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { checkLocation } from '../../ipc/library';
import { pickExistingLibrary, pickNewLibraryLocation } from '../../ipc/dialogs';
import { IpcError } from '../../ipc/result';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { createNew, openOther } from './LibrarySwitcher';

vi.mock('../../ipc/dialogs', () => ({
  pickExistingLibrary: vi.fn(),
  pickNewLibraryLocation: vi.fn(),
}));
vi.mock('../../ipc/library', () => ({ checkLocation: vi.fn() }));
vi.mock('../../stores/noticeStore', () => ({ notify: vi.fn() }));

const info = { name: 'A', path: '/A.kuralib', itemCount: 1, integrity: null } as never;
const tooNew = new IpcError({
  code: 'LibraryTooNew',
  message: '新しい版の Kura で作られたライブラリです',
});

describe('サイドバー・設定の「別のライブラリを開く…」「新しいライブラリを作る…」', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useLibraryStore.setState({ status: 'open', info });
  });

  it('前のライブラリも閉じて初回の画面に戻ったら、知らせの一行は出さない（画面のカードが理由を出す）', async () => {
    vi.mocked(pickExistingLibrary).mockResolvedValue('/B.kuralib');
    const open = vi.fn(async () => {
      useLibraryStore.setState({ status: 'none', info: null });
      throw tooNew;
    });
    useLibraryStore.setState({ open });
    await openOther();
    expect(open).toHaveBeenCalledWith('/B.kuralib');
    expect(notify).not.toHaveBeenCalled();

    useLibraryStore.setState({ status: 'open', info });
    vi.mocked(pickNewLibraryLocation).mockResolvedValue('/C.kuralib');
    vi.mocked(checkLocation).mockResolvedValue({
      path: '/C.kuralib',
      status: 'new',
      syncService: null,
    } as never);
    const create = vi.fn(async () => {
      useLibraryStore.setState({ status: 'none', info: null });
      throw tooNew;
    });
    useLibraryStore.setState({ create });
    await createNew();
    expect(create).toHaveBeenCalledWith('/C.kuralib');
    expect(notify).not.toHaveBeenCalled();
  });

  it('前のライブラリが開いたままなら、今までどおり知らせの一行', async () => {
    vi.mocked(pickExistingLibrary).mockResolvedValue('/B.kuralib');
    useLibraryStore.setState({ open: vi.fn(async () => Promise.reject(tooNew)) });
    await openOther();
    expect(notify).toHaveBeenCalledWith(ja.welcome.failed(tooNew.message));
  });
});
