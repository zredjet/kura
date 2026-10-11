import { askPick } from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import type { DownloadJobDto } from '../../ipc/downloads';
import { createFolder } from '../../ipc/folders';
import { notify } from '../../stores/noticeStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { folderOptions } from '../library/actions';
import { selectAll } from '../library/selection';
import { pathOf } from '../sidebar/tree';

const t = ja.downloads;

/** エラーを知らせる。 */
export function fail(error: unknown): void {
  notify(error instanceof Error ? error.message : String(error));
}

/** 取得したものをライブラリで見せる：1 つのユニットならその中を、そうでなければ最近の取り込みで選ぶ。 */
export function showInLibrary(job: DownloadJobDto): void {
  const view = useViewStore.getState();
  const [unit] = job.unitIds;
  if (job.unitIds.length === 1 && unit !== undefined) {
    view.openUnit(unit);
    return;
  }
  view.setView({ kind: 'recent' });
  if (job.itemIds.length > 0) {
    useViewStore.getState().setSelection(selectAll(job.itemIds));
  }
}

/** 保存先のフォルダを選んでもらう（新しい名前なら作る）。キャンセルなら undefined。 */
export async function pickDestFolder(): Promise<number | undefined> {
  const folders = useSidebarStore.getState().folders;
  const picked = await askPick<number | string>({
    title: t.destPick,
    placeholder: t.destPlaceholder,
    empty: t.destEmpty,
    search: (q) => Promise.resolve(folderOptions(folders, q, { create: true })),
  });
  if (!picked) {
    return undefined;
  }
  if (typeof picked.value === 'number') {
    return picked.value;
  }
  const id = await createFolder(null, picked.value);
  await useSidebarStore.getState().load();
  return id;
}

/** 保存先の表示（「動画 / ライブ」）。指定がなければ「指定なし」。 */
export function destLabel(folderId: number | null): string {
  if (folderId === null) {
    return t.destNone;
  }
  const path = pathOf(useSidebarStore.getState().folders, folderId);
  return path.length > 0 ? path.join(' / ') : t.destNone;
}

/** 文字をクリップボードに写す。 */
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    notify(t.copied);
  } catch (error) {
    fail(error);
  }
}
