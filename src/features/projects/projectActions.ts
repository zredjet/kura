import type { MenuEntry } from '../../components/Menu/menuStore';
import { askConfirm } from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import { pickProjectFolder } from '../../ipc/dialogs';
import { setDragOutTarget } from '../../ipc/dragout';
import { revealPath } from '../../ipc/library';
import {
  addProject,
  type ProjectDto,
  relocateProject,
  removeProject,
  renameProject,
  scanProjects,
} from '../../ipc/projects';
import { notify } from '../../stores/noticeStore';
import { useViewStore } from '../../stores/viewStore';
import { openCreditsSheet } from '../credits/creditsSheetStore';
import { openPresetSheet } from './presetSheetStore';
import { useProjectStore } from './projectStore';

const t = ja.projects;

function fail(error: unknown): void {
  notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
}

async function reload(): Promise<void> {
  await useProjectStore.getState().load();
}

/** フォルダを選んでプロジェクトとして登録し、その一覧を開く（照らし直しは Rust が積む）。 */
export async function addProjectAction(): Promise<void> {
  const path = await pickProjectFolder(t.pickTitle);
  if (!path) return;
  try {
    const project = await addProject(path);
    await reload();
    notify(t.added(project.name));
    useViewStore.getState().setView({ kind: 'project', id: project.id });
  } catch (error) {
    fail(error);
  }
}

/** 名前を変える（空なら何もしない）。 */
export async function renameProjectTo(id: number, name: string): Promise<void> {
  const trimmed = name.trim();
  if (!trimmed) return;
  try {
    await renameProject(id, trimmed);
    await reload();
  } catch (error) {
    fail(error);
  }
}

/** 場所を選び直す（使った記録は残し、照らし直す）。 */
export async function relocateProjectAction(project: ProjectDto): Promise<void> {
  const path = await pickProjectFolder(t.relocateTitle(project.name));
  if (!path) return;
  try {
    await relocateProject(project.id, path);
    await reload();
  } catch (error) {
    fail(error);
  }
}

/** 確かめてから登録を外す（開いていた一覧は「すべて」へ）。 */
export async function removeProjectWithConfirm(project: ProjectDto): Promise<void> {
  const ok = await askConfirm({
    title: t.removeTitle,
    message: t.removeMessage(project.name),
    confirm: t.removeConfirm,
  });
  if (!ok) return;
  try {
    await removeProject(project.id);
    const view = useViewStore.getState().view;
    if (view.kind === 'project' && view.id === project.id) {
      useViewStore.getState().setView({ kind: 'all' });
    }
    await reload();
  } catch (error) {
    fail(error);
  }
}

/** 登録したプロジェクト全部の照らし直しを積む。 */
export async function scanProjectsAction(): Promise<void> {
  try {
    await scanProjects();
    notify(t.scanQueued);
  } catch (error) {
    fail(error);
  }
}

/** 持ち出し先にする（選んでいれば外す）。 */
export async function toggleDragOutTarget(project: ProjectDto): Promise<void> {
  const chosen = useProjectStore.getState().target?.chosen ?? null;
  const next = chosen === project.id ? null : project.id;
  try {
    await setDragOutTarget(next);
    await reload();
    notify(next === null ? t.targetUnset : t.targetSet(project.name));
  } catch (error) {
    fail(error);
  }
}

/** Finder でプロジェクトのフォルダを見せる。 */
export function revealProject(project: ProjectDto): void {
  revealPath(project.root).catch(fail);
}

/** プロジェクトの行・一覧の見出しの「…」の項目。 */
export function projectMenu(project: ProjectDto, rename: () => void): MenuEntry[] {
  const chosen = useProjectStore.getState().target?.chosen === project.id;
  return [
    {
      label: chosen ? t.unsetTarget : t.setTarget,
      checked: chosen,
      onSelect: () => void toggleDragOutTarget(project),
    },
    { label: t.settings, onSelect: () => openPresetSheet(project.id) },
    {
      label: t.credits,
      onSelect: () => openCreditsSheet({ type: 'project', id: project.id }, project.name),
    },
    'separator',
    { label: t.scan, onSelect: () => void scanProjectsAction() },
    { label: t.relocate, onSelect: () => void relocateProjectAction(project) },
    { label: t.reveal, disabled: project.missing, onSelect: () => revealProject(project) },
    'separator',
    { label: t.rename, shortcut: '↩', onSelect: rename },
    {
      label: t.remove,
      shortcut: '⌘⌫',
      danger: true,
      onSelect: () => void removeProjectWithConfirm(project),
    },
  ];
}
