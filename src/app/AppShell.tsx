import type { ReactNode } from 'react';

import styles from './AppShell.module.css';

interface AppShellProps {
  sidebar: ReactNode;
  children: ReactNode;
}

/** ウィンドウ全体の枠。左にサイドバー、右に画面の本体を置く（Main.dc.html の外枠）。 */
export function AppShell({ sidebar, children }: AppShellProps) {
  return (
    <div className={styles.shell}>
      {sidebar}
      <main className={styles.main}>{children}</main>
    </div>
  );
}
