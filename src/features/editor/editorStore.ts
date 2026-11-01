/**
 * エディタの核の状態と操作（M12 の段 5。M13 の段 6a で画像の部分を `image/imageStore.ts` へ分けた。docs/editing.md
 * §12.8・§12.12、design.md §3.1「画像エディタ」）。
 *
 * - 核が持つのは種類によらないもの：セッション・文書・状態・保存・シート・選んだ操作・指摘・結果が確定した手の番号
 *   （`acked`）・応えの来ていない手（`pendingSteps`）・種類（`kind`）。表示（タイル・波形・ズーム・道具）は種類ごとの
 *   `EditorAdapter` が持ち、核はその口だけを呼ぶ。画面に出すものは Zustand に置き、要求は `RequestQueue`（1 つだけ出す）。
 *   開くたびにアダプタを作り、閉じたら捨てる（`dispose`）。
 * - 開く（`openEditorAs`）：前のエディタは閉じる。下書きがあれば下書きのシート（段 5b）。種類からアダプタを選ぶ口は
 *   `editorKinds.ts` の `openEditor`。閉じる（`closeEditor`）：必ず `edit_close` を呼ぶ（開いている途中でセッションがまだ
 *   なければ、Rust の開く途中を `edit_cancel_open` で止める）。未保存で閉じるときのシート・保存の流れは `editorSave.ts`。
 *   ライブラリを切り替える・閉じるときも閉じる（App）。
 * - 右パネル（段 5b）：選んだ操作・操作の並びの手（有効・並べ替え・消す・足す）・引数の手（送り方は `paramSend.ts`）・
 *   出力の形式。引数の欄は、送った手の番号（`sendParam` の戻り値）と、結果が確定した番号（`acked`）を比べて手元の値を
 *   やめる。
 * - 応えの来ていない文書の手（送った・ためた手）があれば `pendingSteps`（未保存に数える。閉じる・保存する前に待つ）。
 */

import { create } from 'zustand';

import {
  cancelOpenEdit,
  closeEdit,
  discardEditDraft,
  type EditDocDeltaDto,
  type EditDraftDto,
  type EditNodeDto,
  type EditOpenedDto,
  type EditProblemDto,
  type EditSaveModeDto,
  type EditSourceDto,
  type EditStatusDto,
  type EditStepDto,
  type EditStepResultDto,
  type EditViewDto,
  type EditViewportDto,
  type EditWritableFormatDto,
  type FormatChoice,
  type Issue,
  type OpDescriptor,
  openEdit,
  type OutputSpec,
  stepEdit,
  viewEdit,
} from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import type { JobProgress } from '../../ipc/jobs';
import { IpcError } from '../../ipc/result';
import { notify } from '../../stores/noticeStore';
import { outputChoice } from './formats';
import { type EditorKind, formatsFor } from './kindTraits';
import { defaultParams, opSpec, type ParamValues } from './opsCatalog';
import {
  addSteps,
  enableSteps,
  moveSteps,
  outputSteps,
  type ParamGesture,
  paramSteps,
  removeSteps,
  sameChoice,
} from './paramSend';
import { type QueuedStep, RequestQueue, type SentRequest } from './requestQueue';

export type EditorPhase = 'closed' | 'opening' | 'open' | 'error';
export type EditorPanel = 'ops' | 'info';

/** 文書の要約（ノードの並びと、各ノードの要約・出力）。 */
export interface EditorDoc {
  order: number[];
  nodes: Record<number, EditNodeDto>;
  outputs: OutputSpec[];
}

/** 保存の向き。 */
export type SaveMode = 'overwrite' | 'saveAs' | 'revert';

/** 閉じたあとにすること（未保存で閉じるときのシートの続き）。 */
export type AfterClose = 'none' | 'library' | 'window';

/** エディタの上に出すシート（`EditorSheets.tsx`）。 */
export type EditorSheet =
  | { kind: 'draft' }
  | { kind: 'unsaved'; then: AfterClose }
  | { kind: 'saveWarnings'; closeAfter: AfterClose | null }
  | { kind: 'saveAs' }
  | { kind: 'revert' }
  | { kind: 'versions' }
  | { kind: 'restore'; version: number }
  | { kind: 'saveFailed'; mode: SaveMode; code: string | null; message: string }
  | {
      /** 保存が FormatUnavailable で断られた：書ける形式を選んで、同じ向きで積み直す（M13）。 */
      kind: 'format';
      /** 断られた保存の向き（上書き・別名。別名は名前つき）。 */
      mode: EditSaveModeDto;
      /** 閉じる前の保存なら、保存が終わってから閉じる。 */
      closeAfter: AfterClose | null;
      /** 書ける形式の一覧（Rust の `edit_formats`。形式の表の順）。 */
      formats: EditWritableFormatDto[];
      /** 代わりの既定（音声は Ogg Vorbis、画像は PNG）。 */
      fallback: string | null;
      /** 書けなかった形式（分からなければ null）。 */
      unavailable: string | null;
    };

/** ジョブの状態。 */
type JobStateDto = JobProgress['state'];

/** 保存する前のアイテムの大きさ（上書きで変わったかを見る。画像は幅と高さ、音声は長さ）。 */
export type SizeBefore =
  { kind: 'image'; w: number; h: number } | { kind: 'audio'; durationMs: number };

/** 動いている保存のジョブ（ボタンの中の進捗・終わったときの知らせ）。 */
export interface EditorSave {
  job: number;
  mode: SaveMode;
  /** 積んだときの確定した頭（この頭のままなら、閉じても保存は続く）。 */
  rev: number;
  state: JobStateDto;
  progress: number | null;
  /** 最初の段（予算の確保を含む）から進まない（「エディタを閉じると進みます」）。 */
  slow: boolean;
  /** 別名で保存の名前。 */
  title: string | null;
  /** 積んだときのアイテム（今）の大きさ（素材の設定が新しい大きさに合うかを知らせる）。 */
  before: SizeBefore | null;
}

/** 失敗（開けなかった・表示できなかった・要求が通らなかった）。 */
export interface EditorFailure {
  kind: 'open' | 'view' | 'request';
  code: string | null;
  message: string;
}

export interface EditorState {
  phase: EditorPhase;
  /** 開くたびに増える（表示の面をつなぎ直す）。 */
  openId: number;
  /** 開いているエディタの種類（アダプタ・画面の部品・キー・表示メニューを選ぶ）。閉じていれば null。 */
  kind: EditorKind | null;
  item: ItemSummaryDto | null;
  session: string | null;
  source: EditSourceDto | null;
  /**
   * 元のファイルの形式（Rust の `originalFormat`。出力の形式の「元と同じ」が書く形式）。元と違う形式で保存したアイテムでは
   * アイテムの今の拡張子と違う。開き終えるまでは null（`originalFormatOf`）。
   */
  originalFormat: string | null;
  status: EditStatusDto | null;
  doc: EditorDoc;
  failure: EditorFailure | null;
  draft: EditDraftDto | null;
  warnings: Issue[];
  panel: EditorPanel;
  /** 右パネルで選んだ操作（ノード ID）。 */
  selected: number | null;
  sheet: EditorSheet | null;
  save: EditorSave | null;
  /** 断られた手の指摘（引数の欄の下に出す）。 */
  issues: Issue[];
  /** 応えの来ていない文書の手（送った・ためた手）がある（未保存に数える）。 */
  pendingSteps: boolean;
  /** 結果が確定した手の番号（断られた・失敗した・仮でない結果が届いた。引数の欄が手元の値をやめる）。 */
  acked: number;
}

const EMPTY_DOC: EditorDoc = { order: [], nodes: {}, outputs: [] };

function closedState(panel: EditorPanel, openId: number): EditorState {
  return {
    phase: 'closed',
    openId,
    kind: null,
    item: null,
    session: null,
    source: null,
    originalFormat: null,
    status: null,
    doc: EMPTY_DOC,
    failure: null,
    draft: null,
    warnings: [],
    panel,
    selected: null,
    sheet: null,
    save: null,
    issues: [],
    pendingSteps: false,
    acked: 0,
  };
}

export const useEditorStore = create<EditorState>(() => closedState('ops', 0));

// ===== 種類ごとの口 =====

/** 表示が出たときに添えるもの。 */
export interface EditorViewInfo {
  /** その結果の要求の通し番号。 */
  seq: number;
  /** その要求に添えた表示の要求。 */
  viewport: EditViewportDto;
  /** 結果が仮の値（preview）を含むか。 */
  provisional: boolean;
}

/**
 * 種類ごとの違い（画像・音声。開くたびに作り、閉じたら捨てる）。核は文書・状態・保存を持ち、表示はアダプタが持つ。
 *
 * 呼ぶ順：作る（`EditorAdapterFactory`）→ `begin`（開き始め。応えの前）→ `initialViewport`（開く要求）→ `opened`
 * （セッションが届いた）→ 結果ごとに `onRejected`（断られた手があれば）→ `applyView`（表示が出た）か `onFailed`（表示
 * できなかった）→ `settle`（必ず最後。文書の差分を当てたあと）。要求そのものが通らなければ `onFailed` だけ。閉じる・
 * 開き直すと `dispose`。`Tag` は手に付ける印（画像は仮の筆）。
 */
export interface EditorAdapter<Tag = unknown> {
  /** 開き始め（核の状態を「開いている途中」にしたあと。仮の表示を置く）。 */
  begin?(): void;
  /** 開く要求に添える表示の要求（窓の大きさから見積もる）。 */
  initialViewport(): EditViewportDto;
  /** 開いた（セッション・元の大きさ）。 */
  opened(opened: EditOpenedDto): void;
  /** 今の表示の要求（手・表示の要求に添える）。 */
  viewport(): EditViewportDto;
  /** 要求を送った（仮の筆に通し番号を付ける）。 */
  sent?(req: SentRequest<Tag>): void;
  /** 手が断られた（`index` は要求の中の手の位置）。 */
  onRejected(seq: number, index: number): void;
  /** 表示が出た。 */
  applyView(view: EditViewDto, info: EditorViewInfo): void;
  /** 表示できなかった・要求が通らなかった。 */
  onFailed(seq: number): void;
  /** 結果を受け終えた（`doc` は差分を当てたあとの文書）。 */
  settle(doc: EditorDoc): void;
  /** 手を送る前（切り抜きの途中ならやめる）。`EditorAdapterContext.send` では呼ばない。 */
  beforeSteps?(): void;
  /** 取り消す・やり直すを止めている（描いている途中）。 */
  busy?(): boolean;
  /** 足す操作の引数の既定値（`atEnd` は末尾に足すか）。なければ `defaultParams(desc, null)`。 */
  newParams?(desc: OpDescriptor, atEnd: boolean): ParamValues;
  /** 捨てる（`closing` は閉じたか。偽なら別のアイテムを開き直した）。 */
  dispose(closing: boolean): void;
}

/** アダプタから核を呼ぶ口（開いたエディタ 1 つ分。閉じた・開き直したあとは何もしない）。 */
export interface EditorAdapterContext<Tag = unknown> {
  /** このエディタが今のものか。 */
  current(): boolean;
  /** セッション（開くまで・閉じたあとは null）。 */
  session(): string | null;
  /** 表示を頼む（前に送ったものと同じなら送らない。`force` なら同じでも送る：裏の仕事を進める頼み直し）。 */
  requestView(viewport: EditViewportDto, force?: boolean): void;
  /** 手を送る（先に `beforeSteps`）。送れなければ false。`selectCreated` なら足したノードを選ぶ。 */
  push(steps: QueuedStep<Tag>[], options?: { selectCreated?: boolean }): boolean;
  /** 手を送る（`beforeSteps` を呼ばない。切り抜きの仮の値を出す・捨てるとき）。 */
  send(steps: QueuedStep<Tag>[], options?: { selectCreated?: boolean }): void;
}

/** 種類ごとのアダプタの作り方。 */
export interface EditorAdapterFactory<Tag = unknown> {
  kind: EditorKind;
  create(item: ItemSummaryDto, ctx: EditorAdapterContext<Tag>): EditorAdapter<Tag>;
}

// ===== 純粋な関数 =====

/** 文書の差分を当てる（`base` が null なら全体。変わっていなければ同じものを返す）。 */
export function applyDocDelta(doc: EditorDoc, delta: EditDocDeltaDto): EditorDoc {
  const sameOrder =
    delta.order.length === doc.order.length && delta.order.every((id, i) => doc.order[i] === id);
  if (delta.base !== null && delta.nodes.length === 0 && sameOrder && !delta.outputs) return doc;
  const known: Record<number, EditNodeDto> = delta.base === null ? {} : { ...doc.nodes };
  for (const n of delta.nodes) known[n.id] = n;
  const nodes: Record<number, EditNodeDto> = {};
  for (const id of delta.order) {
    const n = known[id];
    if (n) nodes[id] = n;
  }
  return { order: [...delta.order], nodes, outputs: delta.outputs ?? doc.outputs };
}

/**
 * 保存していない変更があるか（閉じるときに確かめる）。応えの来ていない文書の手（送った・ためた手）も数える。保存のジョブが
 * 今の頭を保存している途中なら、閉じても保存は続くので含めない。
 */
export function hasUnsavedChanges(
  s: Pick<EditorState, 'status' | 'save'> & { pendingSteps?: boolean },
): boolean {
  if (s.pendingSteps) return true;
  const st = s.status;
  if (!st?.dirty) return false;
  const save = s.save;
  return !(
    save &&
    save.mode === 'overwrite' &&
    save.rev === st.head &&
    save.state !== 'failed' &&
    save.state !== 'canceled'
  );
}

/**
 * 元のファイルの形式（出力の形式の「元と同じ」が書く形式）：Rust の `originalFormat`。開き終える前（まだ分からない）だけ、
 * アイテムの今の拡張子から推す。`source.format` は画素の形式（rgba8 など）で、ファイルの形式ではない。アイテムの今の形式
 * （保存したあとの拡張子・上部の「PNG」）の意味では `item.ext` を使う。
 */
export function originalFormatOf(
  s: Pick<EditorState, 'item'> & Partial<Pick<EditorState, 'originalFormat'>>,
): string | null {
  return s.originalFormat ?? formatsFor(s.item)?.formatOfExt(s.item?.ext) ?? null;
}

/**
 * 保存（上書き）できるか：開いていて、保存の途中でなく、保存していない変更がある（応えの来ていない手も）か、元の形式に
 * 書けず形式を変えて保存するとき（ボタン・⌘S・ファイル › 保存）。
 */
export function canSaveEditor(
  s: Pick<EditorState, 'phase' | 'save' | 'status' | 'pendingSteps' | 'doc' | 'item'> &
    Partial<Pick<EditorState, 'originalFormat'>>,
): boolean {
  if (s.phase !== 'open' || s.save || !s.status) return false;
  if (s.status.dirty || s.pendingSteps) return true;
  const formats = formatsFor(s.item);
  if (!formats) return false;
  return formats.effectiveFormat(outputChoice(s.doc.outputs), originalFormatOf(s)).forced;
}

/** 文書を変える手か（まとめを閉じる・仮の値の手は文書を変えない）。 */
export function isDocStep(step: EditStepDto): boolean {
  return step.type !== 'seal' && step.type !== 'preview' && step.type !== 'cancelPreview';
}

/** 失敗の中身（IPC の誤りは code つき）。 */
export function failureOf(kind: EditorFailure['kind'], error: unknown): EditorFailure {
  if (error instanceof IpcError) return { kind, code: error.code, message: error.message };
  return { kind, code: null, message: error instanceof Error ? error.message : String(error) };
}

function problemFailure(kind: EditorFailure['kind'], p: EditProblemDto): EditorFailure {
  return { kind, code: p.code, message: p.message };
}

// ===== React の外の持ち物 =====

interface Runtime {
  token: number;
  /** 開いたアイテム（開いている途中に閉じたら、これで Rust の開く途中を止める）。 */
  itemId: number;
  session: string | null;
  adapter: EditorAdapter;
  queue: RequestQueue | null;
  returnFocus: HTMLElement | null;
  /** 続けて `superseded` になった数（頼み直しを繰り返さない）。 */
  superseded: number;
  /** 次に足したノードを選ぶ（「操作を追加」・切り抜きの確定）。 */
  selectCreated: boolean;
}

/** 結果を受けたあとに呼ぶもの（保存の流れ。`hadSteps` は手を含む要求の結果か）。 */
type ResultHook = (result: EditStepResultDto, hadSteps: boolean) => void;
const resultHooks = new Set<ResultHook>();

/** 結果を受けたあとに呼ぶものを足す（戻り値で外す）。 */
export function onEditorResult(hook: ResultHook): () => void {
  resultHooks.add(hook);
  return () => {
    resultHooks.delete(hook);
  };
}

/** `superseded` のあとに表示を頼み直す回数の上限（続けて取って代わられたら、次の操作を待つ）。 */
const SUPERSEDED_RETRIES = 2;

let runtime: Runtime | null = null;
let openToken = 0;
/**
 * 応えの来ていない開く途中の中止（`edit_cancel_open`）。次の `edit_open` はこれを待ってから送る（中止が遅れて届くと、同じ
 * アイテムを開き直した新しい開く途中を止めてしまうため）。
 */
let pendingCancel: Promise<void> | null = null;
/** 最後に開いた種類の口（開き直すときに使う）。 */
let lastFactory: EditorAdapterFactory | null = null;

function activeElement(): HTMLElement | null {
  return typeof document !== 'undefined' && document.activeElement instanceof HTMLElement
    ? document.activeElement
    : null;
}

function disposeRuntime(
  rt: Runtime,
  closing: boolean,
  discardDraftOf: number | null = null,
): Promise<void> {
  rt.queue?.close();
  rt.adapter.dispose(closing);
  if (!rt.session) {
    // 開いている途中（edit_open がまだ返らない）：閉じるなら Rust の開く途中を止める（直接読めない形式の長い音声の作業用の
    // ファイルを最後まで書かせない）。止められた edit_open の Conflict は openEditorAs が捨てる。ほかを開くときは、次の
    // edit_open が前のエディタを止めるので送らない。開けなかった（失敗のカード）あとは止めるものがない（閉じるときの
    // ストアはまだこのエディタの状態）
    const opening = useEditorStore.getState().phase === 'opening';
    return closing && opening ? cancelOpening(rt.itemId) : Promise.resolve();
  }
  return (
    closeEdit(rt.session)
      // 閉じるときに待っている下書きを置くので、捨てるのは閉じ終えてから
      .then(() => (discardDraftOf !== null ? discardEditDraft(discardDraftOf) : false))
      .then(
        () => undefined,
        (error: unknown) => console.warn('エディタを閉じられませんでした', error),
      )
  );
}

/** 開く途中のエディタを止める（応えまで `pendingCancel` に置く）。 */
function cancelOpening(itemId: number): Promise<void> {
  const canceling = cancelOpenEdit(itemId).then(
    () => undefined,
    (error: unknown) => console.warn('開く途中のエディタを止められませんでした', error),
  );
  pendingCancel = canceling;
  void canceling.then(() => {
    if (pendingCancel === canceling) pendingCancel = null;
  });
  return canceling;
}

/** アダプタから核を呼ぶ口を作る（`rt` が今のものでなければ何もしない）。 */
function contextFor<Tag>(holder: { rt: Runtime | null }): EditorAdapterContext<Tag> {
  const live = (): Runtime | null => {
    const rt = holder.rt;
    return rt !== null && runtime === rt ? rt : null;
  };
  return {
    current: () => live() !== null,
    session: () => live()?.session ?? null,
    requestView: (viewport, force = false) => {
      live()?.queue?.view(viewport, force);
    },
    push: (steps, options = {}) => {
      const rt = live();
      if (!rt?.queue) return false;
      if (options.selectCreated) rt.selectCreated = true;
      return pushSteps(steps as QueuedStep<unknown>[]);
    },
    send: (steps, options = {}) => {
      const rt = live();
      if (!rt?.queue || steps.length === 0) return;
      if (options.selectCreated) rt.selectCreated = true;
      queueSteps(rt, steps as QueuedStep<unknown>[]);
    },
  };
}

/** エディタを開く（種類の口 `factory` で。前のエディタは閉じる）。 */
export async function openEditorAs<Tag>(
  factory: EditorAdapterFactory<Tag>,
  item: ItemSummaryDto,
  options: { fromDraft?: boolean } = {},
): Promise<void> {
  lastFactory = factory as EditorAdapterFactory;
  const previous = runtime;
  const returnFocus = previous?.returnFocus ?? activeElement();
  if (previous) void disposeRuntime(previous, false);
  const token = ++openToken;
  const holder: { rt: Runtime | null } = { rt: null };
  const adapter = factory.create(item, contextFor<Tag>(holder)) as EditorAdapter;
  const rt: Runtime = {
    token,
    itemId: item.id,
    session: null,
    adapter,
    queue: null,
    returnFocus,
    superseded: 0,
    selectCreated: false,
  };
  holder.rt = rt;
  runtime = rt;
  const panel = useEditorStore.getState().panel;
  useEditorStore.setState({
    ...closedState(panel, token),
    phase: 'opening',
    kind: factory.kind,
    item,
  });
  adapter.begin?.();
  const viewport = adapter.initialViewport();
  try {
    if (pendingCancel) {
      await pendingCancel;
      if (runtime !== rt) return;
    }
    const opened = await openEdit(item.id, viewport, options.fromDraft ?? null);
    if (runtime !== rt) {
      // 閉じた・替えたあとに開き終えた（開く途中の中止が間に合わなかった）
      closeEdit(opened.session).catch(() => undefined);
      return;
    }
    rt.session = opened.session;
    adapter.opened(opened);
    rt.queue = new RequestQueue(
      {
        step: (steps, vp) => stepEdit(opened.session, steps, vp),
        view: (vp) => viewEdit(opened.session, vp),
      },
      {
        sent: (req) => adapter.sent?.(req),
        done: (req, result) => {
          if (runtime === rt) {
            applyResult(rt, result, req.seq, req.viewport, req.steps.length > 0, req.upto);
          }
        },
        error: (req, error) => {
          if (runtime === rt) requestFailed(rt, req, error);
        },
      },
    );
    rt.queue.assumeSent(viewport);
    useEditorStore.setState({
      phase: 'open',
      session: opened.session,
      source: opened.source,
      originalFormat: opened.originalFormat,
      draft: opened.draft,
      // 下書きがあれば、捨てるか下書きから開くかを聞く（下書きから開いたあとは聞かない）
      sheet: opened.draft.exists && !opened.draft.opened ? { kind: 'draft' } : null,
    });
    applyResult(rt, opened.result, 0, viewport, false, null);
    // 開いたあとの実際の大きさ・倍率で頼み直す（見積もりと同じなら送らない）
    rt.queue.view(adapter.viewport());
  } catch (error) {
    // 閉じた・替えたあとの失敗（開く途中の中止で返った Conflict を含む）は出さない
    if (runtime !== rt) return;
    useEditorStore.setState({ phase: 'error', failure: failureOf('open', error) });
  }
}

/** 同じ種類の口で開き直す（版に戻したあと・下書きから開く。開いたことがなければ何もしない）。 */
export function reopenEditorWith(
  item: ItemSummaryDto,
  options: { fromDraft?: boolean } = {},
): Promise<void> {
  return lastFactory ? openEditorAs(lastFactory, item, options) : Promise.resolve();
}

/** 開き直す（失敗のあと・Rust がセッションを閉じたあと）。 */
export function reopenEditor(): void {
  const item = useEditorStore.getState().item;
  if (item) void reopenEditorWith(item);
}

/**
 * エディタを閉じる（必ず `edit_close` を呼ぶ。開いている途中なら `edit_cancel_open` で開く途中を止める。未保存かは確かめない：
 * 確かめるのは `requestCloseEditor`）。`discardDraft` なら閉じ終えてから下書きを捨てる（「保存しない」）。
 */
export function closeEditor(options: { discardDraft?: boolean } = {}): Promise<void> {
  const rt = runtime;
  runtime = null;
  openToken++;
  const s = useEditorStore.getState();
  if (!rt && s.phase === 'closed') return Promise.resolve();
  const closing = rt
    ? disposeRuntime(rt, true, options.discardDraft && s.item ? s.item.id : null)
    : Promise.resolve();
  useEditorStore.setState(closedState(s.panel, s.openId));
  const focus = rt?.returnFocus;
  if (focus && focus.isConnected) focus.focus();
  return closing;
}

/** 応えの来ていない文書の手があるか（ストアに合わせる）。 */
function pendingOf(rt: Runtime): boolean {
  return rt.queue?.holds(isDocStep) ?? false;
}

/** 手と表示の結果を受ける（`hadSteps` は手を含む要求の結果か、`upto` はその要求に入れた手の番号の上限）。 */
function applyResult(
  rt: Runtime,
  result: EditStepResultDto,
  seq: number,
  viewport: EditViewportDto,
  hadSteps: boolean,
  upto: number | null,
): void {
  const s = useEditorStore.getState();
  const doc = applyDocDelta(s.doc, result.doc);
  const patch: Partial<EditorState> = { status: result.status, doc, pendingSteps: pendingOf(rt) };
  // 断られた・仮でない結果が届いたら、その要求までの手の結果は確定（引数の欄は手元の値をやめて文書の値を出す）
  if (
    hadSteps &&
    upto !== null &&
    upto > s.acked &&
    (result.rejected || !result.status.provisional)
  ) {
    patch.acked = upto;
  }
  for (const n of result.notices) notify(n);
  if (result.rejected) {
    rt.adapter.onRejected(seq, result.rejected.index);
    notify(result.rejected.problem.message);
  }
  // 指摘は手を送るたびに替える（表示だけの要求では消さない）
  if (hadSteps) patch.issues = result.rejected?.problem.issues ?? [];
  // 選んだ操作：足したノードを選ぶ・消えたら外す
  let selected = s.selected;
  if (hadSteps && rt.selectCreated && result.created.length > 0) {
    selected = result.created[result.created.length - 1] ?? selected;
    rt.selectCreated = false;
  } else if (hadSteps && rt.selectCreated && result.rejected) {
    rt.selectCreated = false;
  }
  if (selected !== null && !doc.order.includes(selected)) selected = null;
  if (selected !== s.selected) patch.selected = selected;
  const outcome = result.view;
  if (outcome.type !== 'superseded') rt.superseded = 0;
  if (outcome.type === 'shown') {
    rt.adapter.applyView(outcome.view, {
      seq,
      viewport,
      provisional: result.status.provisional,
    });
    patch.warnings = outcome.view.warnings;
    if (s.failure?.kind !== 'open') patch.failure = null;
  } else if (outcome.type === 'superseded') {
    // 当てた手は残る。新しい要求の結果を待つ（出していなければ表示を頼み直す）
    rt.superseded++;
    if (rt.superseded <= SUPERSEDED_RETRIES) rt.queue?.view(rt.adapter.viewport(), true);
  } else {
    rt.adapter.onFailed(seq);
    patch.failure = problemFailure('view', outcome.problem);
  }
  rt.adapter.settle(doc);
  useEditorStore.setState(patch);
  for (const hook of resultHooks) hook(result, hadSteps);
}

function requestFailed(rt: Runtime, req: SentRequest<unknown>, error: unknown): void {
  rt.adapter.onFailed(req.seq);
  const s = useEditorStore.getState();
  useEditorStore.setState({
    failure: failureOf('request', error),
    pendingSteps: pendingOf(rt),
    // 届かなかった手の結果も確定（引数の欄は文書の値に戻す）
    ...(req.steps.length > 0 && req.upto > s.acked ? { acked: req.upto } : {}),
  });
}

/** 手をためて送り、応えの来ていない文書の手があるかをストアに合わせる。 */
function queueSteps(rt: Runtime, steps: QueuedStep<unknown>[]): void {
  if (!rt.queue) return;
  rt.queue.push(steps, rt.adapter.viewport());
  const pending = pendingOf(rt);
  if (useEditorStore.getState().pendingSteps !== pending) {
    useEditorStore.setState({ pendingSteps: pending });
  }
}

/** 手を送る（開いていなければ false）。先にアダプタの片付け（切り抜きの途中なら、切り抜きをやめて仮の値を捨てる）。 */
function pushSteps(steps: QueuedStep<unknown>[]): boolean {
  const rt = runtime;
  if (!rt?.queue) return false;
  rt.adapter.beforeSteps?.();
  queueSteps(rt, steps);
  return true;
}

/** 手を送る（右パネル・保存の流れから。`selectCreated` なら足したノードを選ぶ）。開いていなければ false。 */
export function pushEditorSteps(
  steps: EditStepDto[],
  options: { selectCreated?: boolean } = {},
): boolean {
  const rt = runtime;
  if (!rt?.queue || steps.length === 0) return false;
  if (options.selectCreated) rt.selectCreated = true;
  return pushSteps(steps.map((step) => ({ step })));
}

/** 状態を取り直す（保存のジョブのイベントのあと。同じビューポートでも頼む）。 */
export function refreshEditorView(): void {
  const rt = runtime;
  rt?.queue?.view(rt.adapter.viewport(), true);
}

/** 今のセッション（開いていなければ null）。 */
export function editorSession(): string | null {
  return runtime?.session ?? null;
}

/** 応えの来ていない手（送った・ためた手）の結果が届くのを待つ（閉じたら解ける。保存を積む前に）。 */
export function whenEditorSettled(): Promise<void> {
  return runtime?.queue?.settled() ?? Promise.resolve();
}

/** 保存を積んだ結果の状態・文書の差分を当てる（出力の形式を変える手を確定したときの文書）。 */
export function applyEditorStatus(status: EditStatusDto, delta: EditDocDeltaDto | null): void {
  const s = useEditorStore.getState();
  useEditorStore.setState({
    status,
    doc: delta ? applyDocDelta(s.doc, delta) : s.doc,
  });
}

// ===== 取り消し =====

/**
 * 取り消す（⌘Z。描いている途中は何もしない。切り抜きの途中なら切り抜きをやめてから。取り消せる手がなければ Rust が
 * 何もしない）。
 */
export function editorUndo(): void {
  if (runtime?.adapter.busy?.()) return;
  pushSteps([{ step: { type: 'undo' } }]);
}

/** やり直す（⇧⌘Z）。 */
export function editorRedo(): void {
  if (runtime?.adapter.busy?.()) return;
  pushSteps([{ step: { type: 'redo' } }]);
}

// ===== 右パネル：操作の並び =====

/** 操作を選ぶ（null で外す）。 */
export function selectNode(id: number | null): void {
  const s = useEditorStore.getState();
  const next = id !== null && s.doc.order.includes(id) ? id : null;
  if (next !== s.selected) useEditorStore.setState({ selected: next });
}

/** 上・下の操作を選ぶ（↑↓。選んでいなければ ↓ は先頭、↑ は末尾）。 */
export function selectAdjacent(delta: number): void {
  const { doc, selected } = useEditorStore.getState();
  if (doc.order.length === 0) return;
  const i = selected === null ? -1 : doc.order.indexOf(selected);
  const next =
    i < 0
      ? delta > 0
        ? 0
        : doc.order.length - 1
      : Math.min(doc.order.length - 1, Math.max(0, i + delta));
  selectNode(doc.order[next] ?? null);
}

/** 有効・無効を替える。 */
export function setNodeEnabled(id: number, enabled: boolean): void {
  pushSteps(enableSteps(id, enabled).map((step) => ({ step })));
}

/** 操作を消す（選んでいたら、次の操作か前の操作を選ぶ）。 */
export function removeNode(id: number): void {
  const { doc, selected } = useEditorStore.getState();
  const i = doc.order.indexOf(id);
  if (i < 0) return;
  if (selected === id) {
    const next = doc.order[i + 1] ?? doc.order[i - 1] ?? null;
    useEditorStore.setState({ selected: next });
  }
  pushSteps(removeSteps(id).map((step) => ({ step })));
}

/** 選んだ操作を消す（⌫）。 */
export function removeSelected(): void {
  const id = useEditorStore.getState().selected;
  if (id !== null) removeNode(id);
}

/** 操作を前・後ろへ動かす。 */
export function moveNode(id: number, delta: number): void {
  const steps = moveSteps(useEditorStore.getState().doc.order, id, delta);
  if (steps.length > 0) pushSteps(steps.map((step) => ({ step })));
}

/** 選んだ操作を前・後ろへ動かす（⌥⌘↑↓）。 */
export function moveSelected(delta: number): void {
  const id = useEditorStore.getState().selected;
  if (id !== null) moveNode(id, delta);
}

/**
 * 操作を足す（選んだ操作のあと、選んでいなければ末尾。足したら選ぶ）。引数の既定値はアダプタが決める（画像は末尾に足す
 * ときだけ今の出力の大きさを既定にする）。
 */
export function addOp(id: string): void {
  const desc = opSpec(id);
  const { doc, selected } = useEditorStore.getState();
  if (!desc) return;
  const i = selected === null ? -1 : doc.order.indexOf(selected);
  const at = i < 0 ? doc.order.length : i + 1;
  const rt = runtime;
  const params = rt?.adapter.newParams
    ? rt.adapter.newParams(desc, at === doc.order.length)
    : defaultParams(desc, null);
  if (rt) rt.selectCreated = true;
  pushSteps(addSteps(at, desc.id, desc.version, params).map((step) => ({ step })));
}

/**
 * 引数の欄の操作を送る（送り方は `paramSend.ts`）。送った手の番号を返す（`acked` がこれ以上になったら結果が確定。
 * 送れなければ null）。
 */
export function sendParam(node: number, key: string, gesture: ParamGesture): number | null {
  if (!pushSteps(paramSteps(node, key, gesture).map((step) => ({ step })))) return null;
  return runtime?.queue?.pushedCount ?? null;
}

/** 出力の形式を替える（同じなら何もしない）。 */
export function setOutputFormat(format: FormatChoice): void {
  const outputs = useEditorStore.getState().doc.outputs;
  const now = outputs[0]?.format;
  if (now && sameChoice(now, format)) return;
  const steps = outputSteps(outputs, format);
  if (steps.length > 0) pushSteps(steps.map((step) => ({ step })));
}

// ===== 表示 =====

/** 右パネルの「情報」を切り替える（⌘I）。 */
export function toggleEditorInfo(): void {
  const s = useEditorStore.getState();
  useEditorStore.setState({ panel: s.panel === 'info' ? 'ops' : 'info' });
}
