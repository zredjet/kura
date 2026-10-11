import type { ReactNode } from 'react';

import styles from './AppShell.module.css';

interface AppShellProps {
  sidebar: ReactNode;
  /** サイドバーを出しているか（⌘⌥S で隠せる）。 */
  sidebarVisible: boolean;
  children: ReactNode;
}

/**
 * ウィンドウ全体の枠。左にサイドバー、右に画面の本体を置く（Main.dc.html の外枠）。
 * サイドバーを隠したときは、信号機の分だけ本体の見出しをずらし（CSS 変数）、信号機のまわりをドラッグ領域にする。
 */
export function AppShell({ sidebar, sidebarVisible, children }: AppShellProps) {
  return (
    <div className={styles.shell} data-sidebar={sidebarVisible ? undefined : 'hidden'}>
      {sidebarVisible && sidebar}
      {!sidebarVisible && <div className={styles.dragStrip} data-tauri-drag-region />}
      <main className={styles.main}>{children}</main>
    </div>
  );
}
