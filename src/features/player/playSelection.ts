import { useLibraryStore } from '../../stores/libraryStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { viewTitle } from '../library/viewTitle';
import { playEntries } from './playerActions';

/** 選んでいるもの（一覧の並び順）を再生キューにして、シアターで再生する。 */
export function playSelection(
  entries: readonly number[] = useViewStore.getState().selection.ids,
): void {
  const view = useViewStore.getState();
  const ids = useLibraryStore.getState().list?.ids;
  const selected = new Set(entries);
  const inList = ids ? Array.from(ids).filter((id) => selected.has(id)) : [];
  // 一覧にないもの（読み直す前など）は選んだ順で後ろに
  const placed = new Set(inList);
  const ordered = [...inList, ...entries.filter((id) => !placed.has(id))];
  const { folders, smartFolders } = useSidebarStore.getState();
  void playEntries(ordered, viewTitle(view.view, folders, smartFolders));
}
