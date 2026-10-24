import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { checkLocation } from '../../ipc/library';
import { pickExistingLibrary, pickNewLibraryLocation } from '../../ipc/dialogs';
import { IpcError } from '../../ipc/result';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { answerUnsaved } from '../editor/editorSave';
import { useEditorStore } from '../editor/editorStore';
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

  // design.md §3.1「画像エディタ」：設定のシート・サイドバーから切り替えるとき、エディタに保存していない変更があれば聞く
  it('画像エディタに保存していない変更があれば先に聞き、［キャンセル］なら切り替えず、［保存しない］なら閉じてから開く', async () => {
    const dirty = () =>
      useEditorStore.setState({
        phase: 'open',
        sheet: null,
        save: null,
        pendingSteps: false,
        status: {
          rev: 2,
          head: 2,
          provisional: false,
          canUndo: true,
          canRedo: false,
          dirty: true,
          saving: null,
          needsReopen: false,
        },
      });
    vi.mocked(pickExistingLibrary).mockResolvedValue('/B.kuralib');
    const open = vi.fn(async () => undefined);
    useLibraryStore.setState({ open });
    dirty();
    const canceled = openOther();
    await vi.waitFor(() =>
      expect(useEditorStore.getState().sheet).toEqual({ kind: 'unsaved', then: 'library' }),
    );
    answerUnsaved('cancel');
    await canceled;
    expect(open).not.toHaveBeenCalled();
    expect(useEditorStore.getState().phase).toBe('open');

    const discarded = openOther();
    await vi.waitFor(() => expect(useEditorStore.getState().sheet?.kind).toBe('unsaved'));
    answerUnsaved('discard');
    await discarded;
    expect(useEditorStore.getState().phase).toBe('closed');
    expect(open).toHaveBeenCalledWith('/B.kuralib');

    // 新しいライブラリを作るときも
    dirty();
    vi.mocked(pickNewLibraryLocation).mockResolvedValue('/C.kuralib');
    vi.mocked(checkLocation).mockResolvedValue({
      path: '/C.kuralib',
      status: 'new',
      syncService: null,
    } as never);
    const create = vi.fn(async () => undefined);
    useLibraryStore.setState({ create });
    const creating = createNew();
    await vi.waitFor(() => expect(useEditorStore.getState().sheet?.kind).toBe('unsaved'));
    answerUnsaved('cancel');
    await creating;
    expect(create).not.toHaveBeenCalled();
    useEditorStore.setState({ phase: 'closed', status: null, sheet: null });
  });

  // M12：ほかのアプリが開いているライブラリへは切り替えず、Rust は今のライブラリを閉じない。知らせの一行に理由を出す
  it('ほかのアプリが開いているライブラリ（LibraryInUse）なら、今のライブラリのまま知らせの一行', async () => {
    const inUse = new IpcError({
      code: 'LibraryInUse',
      message:
        'このライブラリはほかのアプリで開かれています。同じライブラリを 2 つのアプリで同時に開くことはできません。もう一方を終了してから開き直してください',
    });
    vi.mocked(pickExistingLibrary).mockResolvedValue('/B.kuralib');
    useLibraryStore.setState({ open: vi.fn(async () => Promise.reject(inUse)) });
    await openOther();
    expect(useLibraryStore.getState()).toMatchObject({ status: 'open', info });
    expect(notify).toHaveBeenCalledWith(ja.welcome.failed(inUse.message));
  });
});
