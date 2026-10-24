/**
 * 画像エディタの保存・閉じる・下書きの流れ（M12 の段 5b。docs/editing.md §12.12・§14、design.md §3.1「画像エディタ」）。
 *
 * - 保存（⌘S。上書き）：警告があるときだけ確認のシートに警告を並べる。保存のジョブを積み（`edit_save`）、進捗はボタンの中、
 *   終わったら知らせる。保存が待っている間に積むと Busy。
 * - ▾：別名で保存…（⌘⇧S。名前のシート）・元に戻す…（確認のシート）・版の一覧…（文字だけの一覧から途中の版に戻す）。
 * - 保存のジョブのイベント（`job://progress` の `edit`）で進捗を出し、終わったら `edit_view` で状態を取り直す（Rust が結果の
 *   版・中身で土台を更新して保存の印を付ける）。版に戻したら `needsReopen` なので開き直す。結果は saved・unchanged・
 *   existing（同じ中身のアイテムを示し、そこへ移れる）、失敗は Conflict・Busy・Mismatch・TooLarge などの分かる言葉のシート。
 * - 保存が最初の段（予算の確保を含む）から進まないときは「エディタを閉じると進みます」（予算を待って駐車している保存は、
 *   エディタの取り置きを外すと進む。Rust の Usage.parked は IPC に出ていないので、時間で見る）。
 * - 未保存で閉じるとき（‹・esc・ライブラリの切り替え・窓を閉じる）は［保存しない］［キャンセル］［保存］のシート。⌘Q（アプリの
 *   終了）は止められないが、Rust が閉じるときに下書きを置くので、次に開くと下書きのシートで戻せる。
 */

import { getCurrentWindow } from '@tauri-apps/api/window';
import { isTauri } from '@tauri-apps/api/core';

import { ja } from '../../i18n/ja';
import {
  discardEditDraft,
  type EditFormatChoice,
  type EditSaveModeDto,
  listEditVersions,
  saveEdit,
} from '../../ipc/edit';
import { getItems } from '../../ipc/items';
import { listJobs, type JobProgress } from '../../ipc/jobs';
import { isIpcError } from '../../ipc/result';
import { invalidateItems } from '../../stores/itemCache';
import { onAnyJob } from '../../stores/jobsStore';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { useViewStore } from '../../stores/viewStore';
import { selectOne } from '../library/selection';
import {
  type AfterClose,
  applyEditorStatus,
  closeEditor,
  editorSession,
  type EditorSave,
  hasUnsavedChanges,
  onEditorResult,
  openEditor,
  refreshEditorView,
  type SaveMode,
  useEditorStore,
} from './editorStore';
import { effectiveFormat, outputChoice } from './formats';

const t = ja.editor;

/** 最初の段のまま、この時間（ms）進まなければ「エディタを閉じると進みます」。 */
export const SLOW_SAVE_MS = 3000;
/** 保存のジョブの最初の段の進み（「元の大きさを確かめています」。予算の確保はこのあと）。 */
export const FIRST_STAGE = 0.05;

/** 保存のジョブの結果（`jobs.result_json`。docs/editing.md §14「保存のジョブ」）。 */
export type SaveOutcome =
  | { outcome: 'saved' | 'unchanged'; itemId: number | null }
  | { outcome: 'existing'; itemId: number; role: string; trashed: boolean };

/** 結果の JSON を読む（読めなければ null）。 */
export function parseSaveResult(json: string | null): SaveOutcome | null {
  if (!json) return null;
  let raw: unknown;
  try {
    raw = JSON.parse(json);
  } catch {
    return null;
  }
  if (!raw || typeof raw !== 'object') return null;
  const r = raw as Record<string, unknown>;
  const itemId = typeof r.itemId === 'number' ? r.itemId : null;
  if (r.outcome === 'existing' && itemId !== null) {
    return {
      outcome: 'existing',
      itemId,
      role: typeof r.role === 'string' ? r.role : 'current',
      trashed: r.trashed === true,
    };
  }
  if (r.outcome === 'saved' || r.outcome === 'unchanged') return { outcome: r.outcome, itemId };
  return null;
}

/** 保存できなかった理由の言葉（code ごと。知らない code は Rust の言葉だけ）。 */
export function saveFailureText(
  mode: SaveMode,
  code: string | null,
  message: string,
): { lead: string | null; detail: string; hint: string | null } {
  const lead = code ? (t.saveFailures[code] ?? null) : null;
  const hint = mode === 'revert' && code === 'Conflict' ? t.revertConflictHint : null;
  return { lead, detail: message, hint };
}

/** 保存のジョブの終わりの知らせ（終わった保存と、Rust が取り直しを返したか）。 */
interface Finished {
  job: number;
  mode: SaveMode;
  title: string | null;
  outcome: SaveOutcome | null;
}

/** 終わったが、まだ知らせていない保存（Rust が取り直しで状態を返したら知らせる）。 */
let finished: Finished | null = null;
/** 保存が終わったら閉じる（未保存で閉じるときのシートの「保存」）。 */
let closeAfterSave: AfterClose | null = null;
/** 閉じるのを待っている呼び手（閉じたら true、やめたら false）。 */
let pendingClose: ((closed: boolean) => void) | null = null;
let slowTimer: ReturnType<typeof setTimeout> | null = null;
let reopening = false;

function setSave(patch: Partial<EditorSave> | null): void {
  const save = useEditorStore.getState().save;
  if (patch === null) {
    if (save) useEditorStore.setState({ save: null });
    return;
  }
  if (save) useEditorStore.setState({ save: { ...save, ...patch } });
}

function settleClose(closed: boolean): void {
  const resolve = pendingClose;
  pendingClose = null;
  closeAfterSave = null;
  resolve?.(closed);
}

function clearSlow(): void {
  if (slowTimer !== null) clearTimeout(slowTimer);
  slowTimer = null;
}

/** 最初の段から進まなければ「エディタを閉じると進みます」を出し、Rust に状態を聞く（駐車ならキャッシュを空にする）。 */
function armSlow(job: number): void {
  clearSlow();
  slowTimer = setTimeout(() => {
    slowTimer = null;
    const save = useEditorStore.getState().save;
    if (!save || save.job !== job) return;
    if (
      (save.progress ?? 0) <= FIRST_STAGE &&
      (save.state === 'queued' || save.state === 'running')
    ) {
      setSave({ slow: true });
      refreshEditorView();
    }
  }, SLOW_SAVE_MS);
}

// ===== 保存を積む =====

/** 出力の形式が「元と同じ」で元の形式に書けなければ、書ける形式を渡す（PSD・HEIC などを開いて保存するとき）。 */
function forcedFormat(): EditFormatChoice {
  const s = useEditorStore.getState();
  const { format, forced } = effectiveFormat(outputChoice(s.doc.outputs), s.source?.format ?? null);
  return forced ? { type: 'to', format } : null;
}

/** 保存のジョブを積む（積めたら true）。 */
async function startSave(
  mode: EditSaveModeDto,
  kind: SaveMode,
  title: string | null,
  closeAfter: AfterClose | null,
): Promise<boolean> {
  const session = editorSession();
  const s = useEditorStore.getState();
  if (!session || s.phase !== 'open') return false;
  if (s.save) {
    notify(t.saveBusy);
    return false;
  }
  try {
    const started = await saveEdit(session, mode, kind === 'revert' ? null : forcedFormat());
    if (editorSession() !== session) return false;
    applyEditorStatus(started.status, started.doc);
    useEditorStore.setState({
      save: {
        job: started.job,
        mode: kind,
        rev: started.status.head,
        state: 'queued',
        progress: null,
        slow: false,
        title,
      },
    });
    closeAfterSave = closeAfter;
    finished = null;
    armSlow(started.job);
    return true;
  } catch (error) {
    if (editorSession() !== session) return false;
    if (isIpcError(error, 'Busy')) {
      notify(t.saveBusy);
    } else {
      useEditorStore.setState({
        sheet: {
          kind: 'saveFailed',
          mode: kind,
          code: isIpcError(error) ? error.code : null,
          message: error instanceof Error ? error.message : String(error),
        },
      });
    }
    return false;
  }
}

/** 保存（⌘S。上書き）。警告があるときだけ確認のシートを出す。 */
export function requestSave(closeAfter: AfterClose | null = null): void {
  const s = useEditorStore.getState();
  if (s.phase !== 'open' || s.sheet || s.save) {
    if (s.save) notify(t.saveBusy);
    if (closeAfter !== null) settleClose(false);
    return;
  }
  if (s.warnings.length > 0) {
    useEditorStore.setState({ sheet: { kind: 'saveWarnings', closeAfter } });
    return;
  }
  void startSave({ type: 'overwrite' }, 'overwrite', null, closeAfter).then((ok) => {
    if (!ok && closeAfter !== null) settleClose(false);
  });
}

/** 警告の確認のシートで「保存」。 */
export function confirmWarnings(): void {
  const sheet = useEditorStore.getState().sheet;
  const closeAfter = sheet?.kind === 'saveWarnings' ? sheet.closeAfter : null;
  useEditorStore.setState({ sheet: null });
  void startSave({ type: 'overwrite' }, 'overwrite', null, closeAfter).then((ok) => {
    if (!ok && closeAfter !== null) settleClose(false);
  });
}

/** 別名で保存…（⌘⇧S）：名前のシート。 */
export function openSaveAs(): void {
  const s = useEditorStore.getState();
  if (s.phase !== 'open' || s.sheet) return;
  if (s.save) {
    notify(t.saveBusy);
    return;
  }
  useEditorStore.setState({ sheet: { kind: 'saveAs' } });
}

/** 名前に使えるか（Rust と同じ：空でなく `/` を含まない）。 */
export function validSaveAsName(name: string): boolean {
  const n = name.trim();
  return n.length > 0 && !n.includes('/') && !n.includes('\0');
}

/** 別名で保存のシートで「保存」（名前は表示名と、元のファイル名の語幹の両方に使う）。 */
export function submitSaveAs(name: string): void {
  const n = name.trim();
  if (!validSaveAsName(n)) return;
  useEditorStore.setState({ sheet: null });
  void startSave({ type: 'saveAs', title: n, stem: n }, 'saveAs', n, null);
}

/** 元に戻す…：確認のシート。 */
export function openRevert(): void {
  const s = useEditorStore.getState();
  if (s.phase !== 'open' || s.sheet) return;
  useEditorStore.setState({ sheet: { kind: 'revert' } });
}

/** 版の一覧…。 */
export function openVersions(): void {
  const s = useEditorStore.getState();
  if (s.phase !== 'open' || s.sheet) return;
  useEditorStore.setState({ sheet: { kind: 'versions' } });
}

/** 版の一覧を読む（シートが呼ぶ）。 */
export function loadVersions(): ReturnType<typeof listEditVersions> {
  const item = useEditorStore.getState().item;
  return item ? listEditVersions(item.id) : Promise.resolve([]);
}

/** 版の一覧で「この版に戻す」：確認のシート。 */
export function askRestore(version: number): void {
  useEditorStore.setState({ sheet: { kind: 'restore', version } });
}

/** 版 `to` に戻す（0 は元。確認のシートで「元に戻す」）。 */
export function confirmRevert(to: number): void {
  useEditorStore.setState({ sheet: null });
  void startSave({ type: 'revert', to }, 'revert', null, null);
}

/** シートを閉じる（キャンセル・esc）。 */
export function dismissSheet(): void {
  const sheet = useEditorStore.getState().sheet;
  if (!sheet) return;
  if (sheet.kind === 'draft') {
    // 下書きのシートを esc で閉じたら、開くのをやめる（下書きは残る。開いただけでは下書きを替えない）
    closeEditor();
    return;
  }
  useEditorStore.setState({ sheet: null });
  if (sheet.kind === 'unsaved' || (sheet.kind === 'saveWarnings' && sheet.closeAfter !== null)) {
    settleClose(false);
  }
}

// ===== 下書き =====

/** 下書きのシートの答え：捨てる・下書きから開く。 */
export function answerDraft(answer: 'discard' | 'open'): void {
  const s = useEditorStore.getState();
  const item = s.item;
  if (!item || s.sheet?.kind !== 'draft') return;
  useEditorStore.setState({ sheet: null });
  if (answer === 'open' && s.draft?.fresh) {
    void openEditor(item, { fromDraft: true });
    return;
  }
  discardEditDraft(item.id).then(
    () => {
      if (useEditorStore.getState().item?.id === item.id) {
        const draft = useEditorStore.getState().draft;
        if (draft) useEditorStore.setState({ draft: { ...draft, exists: false } });
      }
    },
    (error: unknown) =>
      notify(ja.common.failed(error instanceof Error ? error.message : String(error))),
  );
}

// ===== 閉じる =====

/**
 * エディタを閉じる（‹・esc・ライブラリの切り替え・窓を閉じる）。保存していない変更があれば［保存しない］［キャンセル］
 * ［保存］のシートを出す。閉じたら true、やめたら false（開いていなければ true）。
 */
export function requestCloseEditor(then: AfterClose = 'none'): Promise<boolean> {
  const s = useEditorStore.getState();
  if (s.phase === 'closed') return Promise.resolve(true);
  if (s.phase !== 'open' || !hasUnsavedChanges(s)) {
    closeEditor();
    return Promise.resolve(true);
  }
  return new Promise((resolve) => {
    // 前に待っていた呼び手はやめたことにする
    settleClose(false);
    pendingClose = resolve;
    useEditorStore.setState({ sheet: { kind: 'unsaved', then } });
  });
}

/** 未保存で閉じるときのシートの答え。 */
export function answerUnsaved(answer: 'discard' | 'cancel' | 'save'): void {
  const sheet = useEditorStore.getState().sheet;
  if (sheet?.kind !== 'unsaved') return;
  if (answer === 'cancel') {
    useEditorStore.setState({ sheet: null });
    settleClose(false);
    return;
  }
  if (answer === 'discard') {
    closeEditor({ discardDraft: true });
    settleClose(true);
    return;
  }
  useEditorStore.setState({ sheet: null });
  requestSave(sheet.then);
}

// ===== 保存のジョブ =====

function revealItem(itemId: number, trashed: boolean): void {
  void requestCloseEditor().then((closed) => {
    if (!closed) return;
    const view = useViewStore.getState();
    view.setView({ kind: trashed ? 'trash' : 'all' });
    useViewStore.getState().setSelection(selectOne(itemId));
  });
}

/** 一覧・概要を読み直す（保存で中身・拡張子・大きさが変わった、新しいアイテムができた）。 */
function refreshLibrary(itemIds: readonly number[]): void {
  if (itemIds.length > 0) invalidateItems(itemIds);
  void useLibraryStore.getState().loadList();
  void useLibraryStore.getState().refresh();
}

/** 版に戻したあと：下書きを捨てて、新しい今で開き直す。 */
async function reopenAfterRevert(): Promise<void> {
  const item = useEditorStore.getState().item;
  if (!item || reopening) return;
  reopening = true;
  try {
    refreshLibrary([item.id]);
    const [fresh] = await getItems([item.id]);
    await closeEditor({ discardDraft: true });
    await openEditor(fresh ?? item);
    notify(t.reverted);
  } catch (error) {
    notify(ja.common.failed(error instanceof Error ? error.message : String(error)));
  } finally {
    reopening = false;
  }
}

/** 終わった保存を知らせる（Rust が取り直しの結果を返したあと：Rust の「保存しました」のあとに出す）。 */
function deliver(f: Finished): void {
  finished = null;
  clearSlow();
  setSave(null);
  const s = useEditorStore.getState();
  const itemId = s.item?.id ?? null;
  const o = f.outcome;
  if (o?.outcome === 'existing') {
    const self = o.itemId === itemId;
    const text = self
      ? t.existingSelf
      : `${t.existing(t.existingRoles[o.role] ?? o.role)}${o.trashed ? t.existingTrashed : ''}`;
    notify(
      text,
      self ? undefined : { label: t.showItem, run: () => revealItem(o.itemId, o.trashed) },
    );
  } else if (f.mode === 'saveAs') {
    const newId = o?.itemId ?? null;
    refreshLibrary([]);
    notify(
      t.savedAs(f.title ?? ''),
      newId !== null ? { label: t.showItem, run: () => revealItem(newId, false) } : undefined,
    );
  } else if (f.mode === 'overwrite') {
    if (itemId !== null) refreshLibrary([itemId]);
    if (o?.outcome === 'unchanged') notify(t.unchanged);
  }
  if (closeAfterSave !== null) {
    closeEditor();
    settleClose(true);
  }
}

/** 失敗した保存の code と言葉をジョブの記録から読み、シートを出す。 */
async function showFailure(job: number, mode: SaveMode, fallback: string | null): Promise<void> {
  let code: string | null = null;
  let message = fallback ?? '';
  try {
    const jobs = await listJobs({ kinds: ['edit'], limit: 20 });
    const record = jobs.find((j) => j.id === job);
    code = record?.errorCode ?? null;
    message = record?.message ?? message;
  } catch (error) {
    console.warn('保存のジョブを読めませんでした', error);
  }
  if (useEditorStore.getState().phase !== 'closed') {
    useEditorStore.setState({ sheet: { kind: 'saveFailed', mode, code, message } });
  }
}

/** 保存のジョブのイベント（`job://progress`）。今のエディタの保存のジョブだけを見る。 */
export function onEditorJob(event: JobProgress): void {
  if (event.kind !== 'edit') return;
  const save = useEditorStore.getState().save;
  if (!save || event.id !== save.job) return;
  const progress = event.progress ?? save.progress;
  const early = (progress ?? 0) <= FIRST_STAGE;
  setSave({ state: event.state, progress, slow: early ? save.slow : false });
  if (!early) clearSlow();
  else if (event.state === 'running' && slowTimer === null && !save.slow) armSlow(save.job);
  if (event.state === 'done') {
    finished = {
      job: save.job,
      mode: save.mode,
      title: save.title,
      outcome: parseSaveResult(event.resultJson),
    };
    // Rust がもう取り直していれば（ほかの要求で）すぐ知らせる。でなければ取り直しを頼み、その結果のあとに知らせる
    if (useEditorStore.getState().status?.saving !== save.job) deliver(finished);
    else refreshEditorView();
  } else if (event.state === 'failed' || event.state === 'canceled') {
    clearSlow();
    setSave(null);
    finished = null;
    refreshEditorView();
    if (closeAfterSave !== null) settleClose(false);
    if (event.state === 'failed') void showFailure(save.job, save.mode, event.message);
  }
}

/** 取り直しの結果：終わった保存を知らせ、版に戻したなら開き直す。保存のイベントを取りこぼしたら記録を読む。 */
onEditorResult((result) => {
  const s = useEditorStore.getState();
  const save = s.save;
  if (finished && result.status.saving !== finished.job) deliver(finished);
  else if (save && result.status.saving !== save.job && !finished) {
    // Rust はもう終わりを見た（イベントを取りこぼした）：記録から結果を読む
    void listJobs({ kinds: ['edit'], limit: 20 }).then(
      (jobs) => {
        const record = jobs.find((j) => j.id === save.job);
        if (record && useEditorStore.getState().save?.job === save.job) {
          onEditorJob({
            id: record.id,
            kind: record.kind,
            state: record.state,
            progress: record.progress,
            message: record.message,
            resultJson: record.resultJson,
          });
        }
      },
      () => undefined,
    );
  }
  if (result.status.needsReopen) void reopenAfterRevert();
});

// 進捗イベントは jobsStore が受けて、ここへ回す（jobsStore からエディタを読み込まないための口）
onAnyJob(onEditorJob);

// ===== 窓を閉じる =====

let windowGuard: (() => void) | null = null;
let windowGuardPending = false;

/** 保存していない変更がある間だけ、窓を閉じる操作（⌘W・閉じるボタン）を止めてシートを出す。 */
function syncWindowGuard(): void {
  const want = hasUnsavedChanges(useEditorStore.getState());
  if (want && !windowGuard && !windowGuardPending) {
    windowGuardPending = true;
    const win = getCurrentWindow();
    win
      .onCloseRequested((event) => {
        event.preventDefault();
        void requestCloseEditor('window').then((closed) => {
          if (!closed) return;
          // 閉じる操作の受け口があると Rust は窓を閉じないので、受け口を外して壊す（アプリは窓がなくなると終わる）
          windowGuard?.();
          windowGuard = null;
          win.destroy().catch((error: unknown) => console.warn('窓を閉じられませんでした', error));
        });
      })
      .then(
        (unlisten) => {
          windowGuardPending = false;
          windowGuard = unlisten;
          if (!hasUnsavedChanges(useEditorStore.getState())) syncWindowGuard();
        },
        (error: unknown) => {
          windowGuardPending = false;
          console.warn('窓を閉じる操作を受けられません', error);
        },
      );
  } else if (!want && windowGuard) {
    windowGuard();
    windowGuard = null;
  }
}

/** 窓を閉じる操作の見張りを始める（Tauri の中だけ。App が 1 回呼ぶ）。戻り値で止める。 */
export function watchWindowClose(): () => void {
  if (!isTauri()) return () => undefined;
  const stop = useEditorStore.subscribe((now, before) => {
    if (now.status !== before.status || now.save !== before.save || now.phase !== before.phase) {
      syncWindowGuard();
    }
  });
  return () => {
    stop();
    windowGuard?.();
    windowGuard = null;
  };
}
