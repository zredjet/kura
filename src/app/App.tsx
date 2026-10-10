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
import { SearchPalette } from '../features/search/SearchPalette';
import { TagDialog } from '../features/tags/TagDialog';
import { Welcome } from '../features/welcome/Welcome';
import { closePlayer, loadPlayerPrefs } from '../features/player/playerActions';
import { reportPlaybackSupport } from '../features/player/support';
import { Theater } from '../features/player/Theater';
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

  // 再生できる形式の表を Rust に渡す（プロキシの要否の判定に使う。docs/architecture.md §8）
  useEffect(() => {
    if (isTauri()) {
      reportPlaybackSupport().catch((error: unknown) =>
        console.warn('再生可否の表を渡せませんでした', error),
      );
    }
  }, []);

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
    void loadPlayerPrefs();
  }, []);
  // ライブラリを替えたら再生をやめる
  const libraryPath = useLibraryStore((s) => s.info?.path);
  useEffect(() => closePlayer, [libraryPath]);

  return (
    <>
      <AppShell sidebar={<Sidebar />}>
        <LibraryView />
      </AppShell>
      <Theater />
      <ImportSheet />
      <DropOverlay />
      <TagDialog />
      <SearchPalette />
      <Overlays />
      <Menu />
      <Notice />
      <DragGhost />
    </>
  );
}
