import { beforeEach, describe, expect, it, vi } from 'vitest';

import {
  createLibrary,
  libraryInfo,
  onLibraryUpgrade,
  openLibrary,
  restoreLibrary,
} from '../ipc/library';
import { IpcError } from '../ipc/result';
import { useLibraryStore } from './libraryStore';
import { receiveUpgrade, useUpgradeStore } from './upgradeStore';

const order: string[] = [];

vi.mock('../ipc/library', () => ({
  onLibraryUpgrade: vi.fn(async () => {
    // 受け取りの登録は IPC を 1 往復する（待たずに開くと、最初の知らせを落とす）
    await new Promise((resolve) => setTimeout(resolve, 5));
    order.push('listen');
    return () => {};
  }),
  restoreLibrary: vi.fn(),
  openLibrary: vi.fn(),
  createLibrary: vi.fn(),
  libraryInfo: vi.fn(),
}));

const info = { name: 'L', path: '/L.kuralib', itemCount: 3, integrity: null };

beforeEach(() => {
  useLibraryStore.setState({
    status: 'loading',
    info: null,
    missingPath: null,
    restoreError: null,
  });
  vi.mocked(restoreLibrary).mockReset();
  vi.mocked(openLibrary).mockReset();
  vi.mocked(createLibrary).mockReset();
  vi.mocked(libraryInfo).mockReset();
});

describe('起動のときに前回のライブラリを開く', () => {
  it('更新の知らせを受け取り始めてから開く', async () => {
    vi.mocked(restoreLibrary).mockImplementation(async () => {
      order.push('restore');
      return { info, lastPath: info.path };
    });
    await useLibraryStore.getState().restore();
    expect(order).toEqual(['listen', 'restore']);
    expect(onLibraryUpgrade).toHaveBeenCalledTimes(1);
    expect(useLibraryStore.getState().status).toBe('open');
  });

  it('続けて呼ばれても（開発時の StrictMode の 2 回の effect）、開くのは 1 回だけ', async () => {
    // 同じライブラリを 2 つ並べて開くと、新しい版にする前のバックアップがぶつかって両方失敗する（実測）
    vi.mocked(restoreLibrary).mockResolvedValue({ info, lastPath: info.path });
    await Promise.all([useLibraryStore.getState().restore(), useLibraryStore.getState().restore()]);
    await useLibraryStore.getState().restore();
    expect(restoreLibrary).toHaveBeenCalledTimes(1);
    expect(useLibraryStore.getState().status).toBe('open');
  });

  it('開けなかった理由があれば、見つからないとは言わずに理由と場所を持つ', async () => {
    const error = {
      code: 'NoSpace',
      message: 'あと約 120 MB の空きを作ってから開き直してください',
    } as const;
    vi.mocked(restoreLibrary).mockResolvedValue({ info: null, lastPath: '/L.kuralib', error });
    await useLibraryStore.getState().restore();
    expect(useLibraryStore.getState()).toMatchObject({
      status: 'none',
      missingPath: null,
      restoreError: { path: '/L.kuralib', error },
    });
  });

  it('理由がなければ前回の場所が見つからなかった', async () => {
    vi.mocked(restoreLibrary).mockResolvedValue({ info: null, lastPath: '/Volumes/SSD/L.kuralib' });
    await useLibraryStore.getState().restore();
    expect(useLibraryStore.getState()).toMatchObject({
      status: 'none',
      missingPath: '/Volumes/SSD/L.kuralib',
      restoreError: null,
    });
  });

  it('開き終えたら「更新しています」を残さない（知らせが欠けても）', async () => {
    vi.mocked(restoreLibrary).mockImplementation(async () => {
      receiveUpgrade({
        phase: 'started',
        path: info.path,
        fromVersion: 6,
        toVersion: 9,
        dbBytes: 1,
        elapsedMs: null,
        error: null,
      });
      useUpgradeStore.setState({ visible: true });
      return { info, lastPath: info.path };
    });
    await useLibraryStore.getState().restore();
    expect(useUpgradeStore.getState()).toMatchObject({ running: null, visible: false });
  });
});

describe('ライブラリを開く', () => {
  it('開けたら前の理由を消す。開けなければ例外のまま（画面が理由を出す）', async () => {
    useLibraryStore.setState({
      status: 'none',
      restoreError: { path: '/old', error: { code: 'Db', message: 'x' } },
    });
    vi.mocked(openLibrary).mockRejectedValueOnce(
      new IpcError({ code: 'LibraryTooNew', message: '新しい版' }),
    );
    await expect(useLibraryStore.getState().open('/new.kuralib')).rejects.toMatchObject({
      code: 'LibraryTooNew',
    });
    vi.mocked(openLibrary).mockResolvedValueOnce(info);
    await useLibraryStore.getState().open(info.path);
    expect(useLibraryStore.getState()).toMatchObject({ status: 'open', restoreError: null });
  });

  /** 開く IPC の中で「新しい版にし始めた」が届き、表示が出たことにする。 */
  const startUpgrade = () => {
    receiveUpgrade({
      phase: 'started',
      path: '/new.kuralib',
      fromVersion: 6,
      toVersion: 9,
      dbBytes: 1,
      elapsedMs: null,
      error: null,
    });
    useUpgradeStore.setState({ visible: true });
  };

  it('開き終えたら・開けなくても「更新しています」を残さない（finished・failed の知らせが欠けても）', async () => {
    useLibraryStore.setState({ status: 'none' });
    vi.mocked(openLibrary).mockImplementationOnce(async () => {
      startUpgrade();
      return info;
    });
    await useLibraryStore.getState().open(info.path);
    expect(useUpgradeStore.getState()).toMatchObject({ running: null, visible: false });

    useLibraryStore.setState({ status: 'none' });
    vi.mocked(openLibrary).mockImplementationOnce(async () => {
      startUpgrade();
      throw new IpcError({ code: 'UpgradeFailed', message: '戻しました' });
    });
    await expect(useLibraryStore.getState().open('/new.kuralib')).rejects.toMatchObject({
      code: 'UpgradeFailed',
    });
    expect(useUpgradeStore.getState()).toMatchObject({ running: null, visible: false });
  });

  it('起動のときより先に開いても（ほかの道から）、更新の知らせを受け取り始めてから開く', async () => {
    // 受け取りの登録は一度だけ（upgradeStore の watching）なので、まだ登録していないモジュールで確かめる
    vi.resetModules();
    const fresh = await import('./libraryStore');
    const ipc = await import('../ipc/library');
    order.length = 0;
    vi.mocked(ipc.openLibrary).mockImplementationOnce(async () => {
      order.push('open');
      return info;
    });
    await fresh.useLibraryStore.getState().open(info.path);
    expect(order).toEqual(['listen', 'open']);
  });
});

// Rust は前のライブラリを閉じてから開く（src-tauri の AppState::replace_session）。開けなければ、どのライブラリも開いていない
describe('開いているライブラリから別のライブラリを開く・作るのに失敗した', () => {
  const list = { ids: Uint32Array.from([1]), aspects: new Uint16Array(1), groups: [] };
  const closed = new IpcError({ code: 'NotFound', message: 'ライブラリが開かれていません' });

  it('前のライブラリも閉じていれば、初回の画面に戻って理由と場所を出す（開くとき）', async () => {
    useLibraryStore.setState({ status: 'open', info, list });
    vi.mocked(openLibrary).mockRejectedValueOnce(
      new IpcError({ code: 'LibraryTooNew', message: '新しい版の Kura で作られたライブラリです' }),
    );
    vi.mocked(libraryInfo).mockRejectedValueOnce(closed);
    await expect(useLibraryStore.getState().open('/B.kuralib')).rejects.toMatchObject({
      code: 'LibraryTooNew',
    });
    expect(useLibraryStore.getState()).toMatchObject({
      status: 'none',
      info: null,
      list: null,
      missingPath: null,
      restoreError: {
        path: '/B.kuralib',
        error: { code: 'LibraryTooNew', message: '新しい版の Kura で作られたライブラリです' },
      },
    });
  });

  it('作るときも同じ', async () => {
    useLibraryStore.setState({ status: 'open', info, list });
    vi.mocked(createLibrary).mockRejectedValueOnce(
      new IpcError({ code: 'Io', message: '書き込めません' }),
    );
    vi.mocked(libraryInfo).mockRejectedValueOnce(closed);
    await expect(useLibraryStore.getState().create('/C.kuralib')).rejects.toMatchObject({
      code: 'Io',
    });
    expect(useLibraryStore.getState()).toMatchObject({
      status: 'none',
      restoreError: { path: '/C.kuralib', error: { code: 'Io' } },
    });
  });

  it('前のライブラリがまだ開いていれば（閉じる前に失敗した）、そのまま', async () => {
    useLibraryStore.setState({ status: 'open', info, list });
    vi.mocked(openLibrary).mockRejectedValueOnce(new Error('切れた'));
    vi.mocked(libraryInfo).mockResolvedValueOnce(info);
    await expect(useLibraryStore.getState().open('/B.kuralib')).rejects.toThrow('切れた');
    expect(useLibraryStore.getState()).toMatchObject({ status: 'open', info, restoreError: null });
  });

  it('初回の画面から開けなかったときは画面が自分で理由を出す（状態を変えない・確かめない）', async () => {
    useLibraryStore.setState({ status: 'none', info: null });
    vi.mocked(openLibrary).mockRejectedValueOnce(
      new IpcError({ code: 'NoSpace', message: '空きが足りません' }),
    );
    await expect(useLibraryStore.getState().open('/B.kuralib')).rejects.toMatchObject({
      code: 'NoSpace',
    });
    expect(useLibraryStore.getState()).toMatchObject({ status: 'none', restoreError: null });
    expect(libraryInfo).not.toHaveBeenCalled();
  });
});
