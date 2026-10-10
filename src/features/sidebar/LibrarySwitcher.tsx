import { openMenuBelow } from '../../components/Menu/menuStore';
import { LibraryIcon } from '../../components/icons/LibraryIcon';
import { UpDownIcon } from '../../components/icons/UpDownIcon';
import { ja } from '../../i18n/ja';
import { pickExistingLibrary, pickNewLibraryLocation } from '../../ipc/dialogs';
import { checkLocation } from '../../ipc/library';
import { isIpcError } from '../../ipc/result';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import styles from './Sidebar.module.css';

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function openOther(): Promise<void> {
  const picked = await pickExistingLibrary(ja.welcome.openTitle);
  if (!picked) {
    return;
  }
  try {
    await useLibraryStore.getState().open(picked);
  } catch (error) {
    notify(
      isIpcError(error, 'NotFound') ? ja.welcome.notALibrary : ja.welcome.failed(messageOf(error)),
    );
  }
}

async function createNew(): Promise<void> {
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
    const store = useLibraryStore.getState();
    await (location.status === 'existingLibrary'
      ? store.open(location.path)
      : store.create(location.path));
  } catch (error) {
    notify(ja.welcome.failed(messageOf(error)));
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
