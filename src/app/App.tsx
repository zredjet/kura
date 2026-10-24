import { isTauri } from '@tauri-apps/api/core';
import { useEffect } from 'react';

import { Menu } from '../components/Menu/Menu';
import { Notice } from '../components/Notice/Notice';
import { Overlays } from '../components/Overlay/Overlays';
import { startLoginShotEvents } from '../features/downloads/captureShot';
import { DownloadsView } from '../features/downloads/DownloadsView';
import { useDownloadsShortcut } from '../features/downloads/useDownloadsShortcut';
import { DropOverlay } from '../features/import/DropOverlay';
import { ImportSheet } from '../features/import/ImportSheet';
import { Editor } from '../features/editor/Editor';
import { closeEditor } from '../features/editor/editorStore';
import { DragGhost } from '../features/library/DragGhost';
import { useFileDrop, useImportShortcut } from '../features/import/useFileDrop';
import { LibraryView } from '../features/library/LibraryView';
import { PlaylistView } from '../features/playlists/PlaylistView';
import { Sidebar } from '../features/sidebar/Sidebar';
import { SearchPalette } from '../features/search/SearchPalette';
import { SettingsSheet } from '../features/settings/SettingsSheet';
import { TagDialog } from '../features/tags/TagDialog';
import { TagsView } from '../features/tags/TagsView';
import { LibraryUpgrade } from '../features/welcome/LibraryUpgrade';
import { Welcome } from '../features/welcome/Welcome';
import { closePlayer, loadPlayerPrefs, watchFullscreen } from '../features/player/playerActions';
import { reportPlaybackSupport } from '../features/player/support';
import { Theater } from '../features/player/Theater';
import { useDownloadsStore } from '../stores/downloadsStore';
import { startJobEvents, useJobsStore } from '../stores/jobsStore';
import { useLibraryStore } from '../stores/libraryStore';
import { useViewStore } from '../stores/viewStore';
import { AppShell } from './AppShell';
import styles from './App.module.css';

export function App() {
  const status = useLibraryStore((s) => s.status);

  useEffect(() => {
    void useLibraryStore.getState().restore();
  }, []);

  useEffect(() => (isTauri() ? startJobEvents() : undefined), []);
  useEffect(() => (isTauri() ? startLoginShotEvents() : undefined), []);
  useEffect(() => (isTauri() ? watchFullscreen() : undefined), []);

  // 再生できる形式の表を Rust に渡す（プロキシの要否の判定に使う。docs/architecture.md §8）
  useEffect(() => {
    if (isTauri()) {
      reportPlaybackSupport().catch((error: unknown) =>
        console.warn('再生可否の表を渡せませんでした', error),
      );
    }
  }, []);

  return (
    <>
      {status === 'loading' ? (
        <div className={styles.loading} data-tauri-drag-region />
      ) : status === 'none' ? (
        <Welcome />
      ) : (
        <LibraryScreen />
      )}
      {/* 開くときに DB を新しい版にするのが 300ms を超えたら、どの画面の上にも出す */}
      <LibraryUpgrade />
    </>
  );
}

/** ライブラリを開いているときの画面。 */
function LibraryScreen() {
  useFileDrop();
  useImportShortcut();
  useDownloadsShortcut();
  useEffect(() => {
    void useJobsStore.getState().load();
    void useDownloadsStore.getState().load();
    void loadPlayerPrefs();
  }, []);
  const playlist = useViewStore((s) => s.playlist);
  const downloads = useViewStore((s) => s.downloads);
  const tags = useViewStore((s) => s.tags);
  const sidebarVisible = useViewStore((s) => s.sidebar);
  // ライブラリを替えたら再生をやめ、画像エディタを閉じる（Rust もライブラリを閉じる前にエディタを閉じる）
  const libraryPath = useLibraryStore((s) => s.info?.path);
  useEffect(() => closePlayer, [libraryPath]);
  useEffect(() => closeEditor, [libraryPath]);

  return (
    <>
      <AppShell sidebar={<Sidebar />} sidebarVisible={sidebarVisible}>
        {downloads ? (
          <DownloadsView />
        ) : tags ? (
          <TagsView />
        ) : playlist !== null ? (
          <PlaylistView key={playlist} id={playlist} />
        ) : (
          <LibraryView />
        )}
      </AppShell>
      <Theater />
      <Editor />
      <ImportSheet />
      <DropOverlay />
      <TagDialog />
      <SearchPalette />
      <SettingsSheet />
      <Overlays />
      <Menu />
      <Notice />
      <DragGhost />
    </>
  );
}
