import { isTauri } from '@tauri-apps/api/core';
import { useEffect } from 'react';

import { Menu } from '../components/Menu/Menu';
import { Notice } from '../components/Notice/Notice';
import { Overlays } from '../components/Overlay/Overlays';
import { DropOverlay } from '../features/import/DropOverlay';
import { ImportSheet } from '../features/import/ImportSheet';
import { DragGhost } from '../features/library/DragGhost';
import { useFileDrop, useImportShortcut } from '../features/import/useFileDrop';
import { LibraryView } from '../features/library/LibraryView';
import { Sidebar } from '../features/sidebar/Sidebar';
import { TagDialog } from '../features/tags/TagDialog';
import { Welcome } from '../features/welcome/Welcome';
import { startJobEvents, useJobsStore } from '../stores/jobsStore';
import { useLibraryStore } from '../stores/libraryStore';
import { AppShell } from './AppShell';
import styles from './App.module.css';

export function App() {
  const status = useLibraryStore((s) => s.status);

  useEffect(() => {
    void useLibraryStore.getState().restore();
  }, []);

  useEffect(() => (isTauri() ? startJobEvents() : undefined), []);

  if (status === 'loading') {
    return <div className={styles.loading} data-tauri-drag-region />;
  }
  if (status === 'none') {
    return <Welcome />;
  }
  return <LibraryScreen />;
}

/** ライブラリを開いているときの画面。 */
function LibraryScreen() {
  useFileDrop();
  useImportShortcut();
  useEffect(() => {
    void useJobsStore.getState().load();
  }, []);

  return (
    <>
      <AppShell sidebar={<Sidebar />}>
        <LibraryView />
      </AppShell>
      <ImportSheet />
      <DropOverlay />
      <TagDialog />
      <Overlays />
      <Menu />
      <Notice />
      <DragGhost />
    </>
  );
}
