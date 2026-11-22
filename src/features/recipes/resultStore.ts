/**
 * 一括の結果のシート（M14 段 7。docs/editing.md §14.2「結果」）：状態と理由ごとの群・別名で作ったアイテム・［もう一度…］・
 * ［元に戻す…］。カプセルの［結果を見る］から開く。一括が動いている間は、カプセルが読み直した結果に合わせて替わる。
 */

import { create } from 'zustand';

import { askConfirm } from '../../components/Overlay/overlayStore';
import { ja } from '../../i18n/ja';
import { type BatchResultDto, batchResult, undoBatch } from '../../ipc/batch';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import { openApply } from './applyStore';
import { useBatchJobsStore } from './batchJobsStore';

interface ResultState {
  job: number | null;
  result: BatchResultDto | null;
  error: string | null;
}

export const useResultStore = create<ResultState>(() => ({ job: null, result: null, error: null }));

let opened = 0;

/** 結果を読み直す。 */
async function load(job: number): Promise<void> {
  const mine = opened;
  try {
    const result = await batchResult(job);
    if (mine !== opened) return;
    useResultStore.setState({ result, error: null });
    // カプセルの写しもそろえる（古い写しで、シートの新しい結果を上書きしない）
    useBatchJobsStore.setState((s) => {
      const known = s.jobs[job];
      return known ? { jobs: { ...s.jobs, [job]: { ...known, result } } } : s;
    });
  } catch (error) {
    if (mine !== opened) return;
    useResultStore.setState({ error: error instanceof Error ? error.message : String(error) });
  }
}

/** 結果のシートを開く。 */
export function openResult(job: number): void {
  opened++;
  const known = useBatchJobsStore.getState().jobs[job]?.result ?? null;
  useResultStore.setState({ job, result: known, error: null });
  void load(job);
}

export function closeResult(): void {
  opened++;
  useResultStore.setState({ job: null, result: null, error: null });
}

// カプセルが新しく読み直した結果に合わせる（開いている一括の写しが変わったときだけ。ほかの一括の進みでは替えない）
useBatchJobsStore.subscribe((s, before) => {
  const { job } = useResultStore.getState();
  if (job === null) return;
  const result = s.jobs[job]?.result;
  if (result && result !== before.jobs[job]?.result) {
    useResultStore.setState({ result });
  }
});

/** もう一度当てられる行（飛ばした・失敗した・中止した・待っている。1 件ずつの一括だけ）。 */
export function retryItems(result: BatchResultDto): number[] {
  if (result.action.type !== 'overwrite' && result.action.type !== 'saveAs') return [];
  return result.groups
    .filter(
      (g) =>
        g.state === 'skipped' ||
        g.state === 'failed' ||
        g.state === 'canceled' ||
        g.state === 'pending',
    )
    .flatMap((g) => g.items);
}

/** ［もう一度…］：その一括の写しで、飛ばした・失敗したものに当てるシートを開く（同じ保存のしかたから始める）。 */
export function retryBatch(result: BatchResultDto): void {
  const items = retryItems(result);
  if (items.length === 0) return;
  closeResult();
  void openApply({ type: 'job', job: result.job }, items, {
    retry: true,
    mode: result.action.type === 'saveAs' ? 'saveAs' : 'overwrite',
    nameTemplate: result.action.type === 'saveAs' ? result.action.name : undefined,
  });
}

/** ［元に戻す…］：確かめてから、元に戻す一括を積む。 */
export async function undoFromResult(result: BatchResultDto): Promise<void> {
  const t = ja.batch;
  const n = formatCount(result.undoAvailable);
  const message =
    result.action.type === 'merge'
      ? t.undoConfirmMerge
      : result.action.type === 'saveAs'
        ? t.undoConfirmSaveAs(n)
        : t.undoConfirmOverwrite(n);
  const ok = await askConfirm({ title: t.undoConfirmTitle, message, confirm: t.undoConfirm });
  if (!ok) return;
  try {
    await undoBatch(result.job);
    notify(t.undoStarted);
    closeResult();
  } catch (error) {
    notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
  }
}
