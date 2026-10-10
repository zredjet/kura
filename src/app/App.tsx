import { LibraryView } from '../features/library/LibraryView';
import { Sidebar } from '../features/sidebar/Sidebar';
import { AppShell } from './AppShell';

export function App() {
  return (
    <AppShell sidebar={<Sidebar />}>
      <LibraryView />
    </AppShell>
  );
}
