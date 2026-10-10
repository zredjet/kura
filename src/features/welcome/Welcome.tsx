import { homeDir, join, pictureDir } from '@tauri-apps/api/path';
import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { FolderIcon } from '../../components/icons/FolderIcon';
import { LibraryIcon } from '../../components/icons/LibraryIcon';
import { WarningIcon } from '../../components/icons/WarningIcon';
import { ja } from '../../i18n/ja';
import { pickExistingLibrary, pickNewLibraryLocation } from '../../ipc/dialogs';
import { checkLocation, type LocationCheckDto } from '../../ipc/library';
import { isIpcError } from '../../ipc/result';
import { tildePath } from '../../lib/format';
import { useLibraryStore } from '../../stores/libraryStore';
import styles from './Welcome.module.css';

const t = ja.welcome;

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** ライブラリがまだないときの画面（初回起動）。新しく作るか、既存のものを開く。 */
export function Welcome() {
  const missingPath = useLibraryStore((s) => s.missingPath);
  const createLibrary = useLibraryStore((s) => s.create);
  const openLibrary = useLibraryStore((s) => s.open);
  const [home, setHome] = useState<string | null>(null);
  const [location, setLocation] = useState<LocationCheckDto | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const titleId = useId();

  useEffect(() => {
    let alive = true;
    (async () => {
      const [homePath, pictures] = await Promise.all([homeDir(), pictureDir()]);
      const checked = await checkLocation(await join(pictures, t.defaultName));
      if (alive) {
        setHome(homePath);
        setLocation(checked);
      }
    })().catch((err: unknown) => console.warn('既定の場所を決められませんでした', err));
    return () => {
      alive = false;
    };
  }, []);

  const changeLocation = async () => {
    const picked = await pickNewLibraryLocation(
      t.newLocationTitle,
      location?.path ?? t.defaultName,
    );
    if (picked) {
      setLocation(await checkLocation(picked));
    }
  };

  const run = async (action: () => Promise<void>, describe: (err: unknown) => string) => {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  };

  const createOrOpen = () => {
    if (!location) {
      return;
    }
    void run(
      () =>
        location.status === 'existingLibrary'
          ? openLibrary(location.path)
          : createLibrary(location.path),
      (err) => t.failed(messageOf(err)),
    );
  };

  const openExisting = async () => {
    const picked = await pickExistingLibrary(t.openTitle);
    if (picked) {
      await run(
        () => openLibrary(picked),
        (err) => (isIpcError(err, 'NotFound') ? t.notALibrary : t.failed(messageOf(err))),
      );
    }
  };

  return (
    <div className={styles.welcome}>
      <div className={styles.dragRegion} data-tauri-drag-region />
      <section className={styles.card} aria-labelledby={titleId}>
        <span className={styles.logo}>
          <LibraryIcon size={26} strokeWidth={2} />
        </span>
        <div className={styles.heading}>
          <h1 id={titleId} className={styles.title}>
            {t.title}
          </h1>
          <p className={styles.lead}>{t.lead}</p>
        </div>

        {missingPath && (
          <p className={styles.notice} role="status">
            {t.missing(tildePath(missingPath, home))}
          </p>
        )}

        <div className={styles.block}>
          <h2 className={styles.blockTitle}>{t.newLibrary}</h2>
          <div className={styles.location}>
            <span className={styles.folderIcon}>
              <FolderIcon size={16} strokeWidth={1.8} />
            </span>
            <span className={styles.path} title={location?.path}>
              {location ? tildePath(location.path, home) : '…'}
            </span>
            <Button size="sm" onClick={() => void changeLocation()} disabled={busy}>
              {t.changeLocation}
            </Button>
          </div>
          {location?.syncService && (
            <p className={styles.warning} role="alert">
              <WarningIcon size={14} strokeWidth={2} />
              {t.syncWarning(location.syncService)}
            </p>
          )}
          {location?.status === 'existingLibrary' && (
            <p className={styles.hint}>{t.existingHere}</p>
          )}
          {location?.status === 'occupied' && (
            <p className={styles.warning} role="alert">
              <WarningIcon size={14} strokeWidth={2} />
              {t.occupied}
            </p>
          )}
          <Button
            variant="primary"
            className={styles.primary}
            onClick={createOrOpen}
            disabled={busy || !location || location.status === 'occupied'}
          >
            {location?.status === 'existingLibrary' ? t.openThis : t.create}
          </Button>
        </div>

        <div className={styles.divider} />

        <Button onClick={() => void openExisting()} disabled={busy}>
          {t.openExisting}
        </Button>

        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
      </section>
    </div>
  );
}
