import { openMenuBelow } from '../../components/Menu/menuStore';
import { LibraryIcon } from '../../components/icons/LibraryIcon';
import { UpDownIcon } from '../../components/icons/UpDownIcon';
import { ja } from '../../i18n/ja';
import { pickExistingLibrary, pickNewLibraryLocation } from '../../ipc/dialogs';
import { checkLocation } from '../../ipc/library';
import { isIpcError } from '../../ipc/result';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { requestCloseEditor } from '../editor/editorSave';
import styles from './Sidebar.module.css';

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * 開く・作るのに失敗したことを知らせる。前のライブラリも閉じて初回の画面に戻ったときは、画面のカードが理由と場所を
 * 出すので知らせない（知らせの一行はライブラリの画面にしかない）。
 */
function notifyUnlessClosed(message: string): void {
  if (useLibraryStore.getState().status === 'open') {
    notify(message);
  }
}

/** 別のライブラリを選んで開く（サイドバーの切り替え・設定画面）。 */
export async function openOther(): Promise<void> {
  const picked = await pickExistingLibrary(ja.welcome.openTitle);
  if (!picked) {
    return;
  }
  // 画像エディタに保存していない変更があれば、先に聞く（設定のシートから切り替えるとき）
  if (!(await requestCloseEditor('library'))) {
    return;
  }
  try {
    await useLibraryStore.getState().open(picked);
  } catch (error) {
    notifyUnlessClosed(
      isIpcError(error, 'NotFound') ? ja.welcome.notALibrary : ja.welcome.failed(messageOf(error)),
    );
  }
}

/** 新しいライブラリを作って開く（サイドバーの切り替え・設定画面）。 */
export async function createNew(): Promise<void> {
  const picked = await pickNewLibraryLocation(ja.welcome.newLocationTitle, ja.welcome.defaultName);
  if (!picked) {
    return;
  }
  try {
    const location = await checkLocation(picked);
    if (location.status === 'occupied') {
      notify(ja.welcome.occupied);
      return;
    }
    if (!(await requestCloseEditor('library'))) {
      return;
    }
    const store = useLibraryStore.getState();
    await (location.status === 'existingLibrary'
      ? store.open(location.path)
      : store.create(location.path));
  } catch (error) {
    notifyUnlessClosed(ja.welcome.failed(messageOf(error)));
  }
}

/** サイドバー上のライブラリ切り替え（Sidebar.dc.html の「メインライブラリ」）。 */
export function LibrarySwitcher() {
  const name = useLibraryStore((s) => s.info?.name ?? '');
  return (
    <button
      type="button"
      className={styles.switcher}
      aria-label={ja.sidebar.switchLibrary(name)}
      aria-haspopup="menu"
      onClick={(e) =>
        openMenuBelow(e.currentTarget, ja.sidebar.switchMenu, [
          { label: ja.sidebar.openOther, onSelect: () => void openOther() },
          { label: ja.sidebar.createNew, onSelect: () => void createNew() },
        ])
      }
    >
      <span className={styles.switcherBadge}>
        <LibraryIcon size={14} strokeWidth={2.2} />
      </span>
      <span className={styles.switcherName}>{name}</span>
      <span className={styles.switcherChevron}>
        <UpDownIcon size={14} strokeWidth={2} />
      </span>
    </button>
  );
}
