/**
 * 画像エディタの状態と操作（M12 の段 5。docs/editing.md §12.8・§12.12、design.md §3.1「画像エディタ」）。
 *
 * - 画面に出すもの（セッション・状態・文書の要約・表示の段と大きさ・失敗・右パネル・倍率の表示）は Zustand に置く。
 * - キャンバス（タイル・ズーム・パン・仮の筆）は React の外の `CanvasController`、要求は `RequestQueue`（1 つだけ出す）。
 *   開くたびに作り、閉じたら捨てる（ImageBitmap は close）。
 * - 開く（`openEditor`）：画像 1 件だけ。前のエディタは閉じる。下書きがあれば下書きのシート（段 5b）。閉じる（`closeEditor`）：
 *   必ず `edit_close` を呼ぶ。未保存で閉じるときのシート・保存の流れは `editorSave.ts`。ライブラリを切り替える・閉じるときも
 *   閉じる（App）。
 * - 右パネル（段 5b）：選んだ操作・操作の並びの手（有効・並べ替え・消す・足す）・引数の手（送り方は `paramSend.ts`）・
 *   出力の形式。引数の欄は、送った手の番号（`sendParam` の戻り値）と、結果が確定した番号（`acked`）を比べて手元の値を
 *   やめる。切り抜きの枠（C）：その操作の入力を表示して（操作を仮に無効にする preview）枠を重ね、↵ で確定する。後ろに
 *   有効な幾何の操作（大きさ・向き・並びを変える）があれば、仮に無効にしても入力にならないので、枠は出さずに引数パネルで
 *   直す。
 * - 応えの来ていない文書の手（送った・ためた手）があれば `pendingSteps`（未保存に数える。閉じる・保存する前に待つ）。
 * - 描画の道具：筆は描いている間フロントで仮に重ね、離したら `paint{actions:[筆]}`（描く先は Rust が決める）。
 *   塗りつぶしは仮描きせずに送る。スポイトはクリックのときだけ `edit_sample`。
 */

import { create } from 'zustand';

import { ja } from '../../i18n/ja';
import {
  closeEdit,
  discardEditDraft,
  type EditDocDeltaDto,
  type EditDraftDto,
  type EditFrameDto,
  type EditNodeDto,
  type EditProblemDto,
  type EditSourceDto,
  type EditStatusDto,
  type EditStepDto,
  type EditStepResultDto,
  type EditViewDto,
  type EditViewportDto,
  type FormatChoice,
  type Issue,
  openEdit,
  type OutputSpec,
  sampleEdit,
  stepEdit,
  viewEdit,
} from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import type { JobProgress } from '../../ipc/jobs';
import { IpcError, isIpcError } from '../../ipc/result';
import type { Point } from '../../lib/paint';
import { notify } from '../../stores/noticeStore';
import { CanvasController, type ControllerInfo } from './canvasController';
import { type Bounds, clampCrop, isFullCrop, nudgeCrop, readRect, sameRect } from './crop';
import { defaultParams, hasLaterGeometry, opSpec, paramsOf } from './opsCatalog';
import {
  addSteps,
  commitStep,
  enableSteps,
  moveSteps,
  outputSteps,
  type ParamGesture,
  paramSteps,
  removeSteps,
  sameChoice,
} from './paramSend';
import { drawScene } from './drawScene';
import { effectiveFormat, outputChoice } from './formats';
import { type QueuedStep, RequestQueue, type SentRequest } from './requestQueue';
import { fillAction, type PendingStroke, type StrokeSpec } from './stroke';
import { type EditorTool, useToolStore } from './toolStore';
import { fitZoom, formatCanBeTransparent, type Rect } from './viewMath';

const t = ja.editor;

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
  | { kind: 'saveFailed'; mode: SaveMode; code: string | null; message: string };

/** ジョブの状態。 */
type JobStateDto = JobProgress['state'];

/** 保存する前のアイテムの大きさ（上書きで大きさが変わったかを見る）。 */
export interface SizeBefore {
  w: number;
  h: number;
}

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

/** 切り抜きの枠（C）。 */
export interface EditorCrop {
  /** 直す切り抜きのノード（null は末尾に足す）。 */
  node: number | null;
  rect: Rect;
  /** 切り抜く前の画像の大きさ（その操作の入力を表示し終えるまで null）。 */
  bounds: Bounds | null;
  /** 切り抜きを始める前の文書の出力の大きさ（枠を出している間に足す操作の既定値に使う）。 */
  output: Bounds | null;
  /** その操作を仮に無効にして入力を表示している（やめる・確定するときに仮の値を捨てる）。 */
  previewed: boolean;
  /** 切り抜きを終えたら戻す道具。 */
  returnTool: EditorTool;
}

/** 表示の要約（段・大きさ・フレーム・画素の形式）。 */
export interface EditorView {
  rev: number;
  level: number;
  fullWidth: number;
  fullHeight: number;
  frames: EditFrameDto[];
  format: string | null;
  fallback: string | null;
}

/** 失敗（開けなかった・表示できなかった・要求が通らなかった）。 */
export interface EditorFailure {
  kind: 'open' | 'view' | 'request';
  code: string | null;
  message: string;
}

export interface EditorState {
  phase: EditorPhase;
  /** 開くたびに増える（キャンバスをつなぎ直す）。 */
  openId: number;
  item: ItemSummaryDto | null;
  session: string | null;
  source: EditSourceDto | null;
  pixelArt: boolean;
  status: EditStatusDto | null;
  doc: EditorDoc;
  view: EditorView | null;
  frame: number;
  failure: EditorFailure | null;
  draft: EditDraftDto | null;
  warnings: Issue[];
  panel: EditorPanel;
  /** 倍率（CanvasController が間引いて送る）。 */
  zoom: number;
  /** 見えているタイルの絵が出そろった（開いた直後の仮の絵を外す）。 */
  ready: boolean;
  imageRect: Rect | null;
  /** ポインタの下の画素（画像の外は null）。 */
  pointer: Point | null;
  /** 押している間だけの道具（Space の手のひら・⌥ のスポイト）。 */
  temporary: 'hand' | 'eyedropper' | null;
  drawing: boolean;
  /** 右パネルで選んだ操作（ノード ID）。 */
  selected: number | null;
  sheet: EditorSheet | null;
  save: EditorSave | null;
  /** 断られた手の指摘（引数の欄の下に出す）。 */
  issues: Issue[];
  crop: EditorCrop | null;
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
    item: null,
    session: null,
    source: null,
    pixelArt: false,
    status: null,
    doc: EMPTY_DOC,
    view: null,
    frame: 0,
    failure: null,
    draft: null,
    warnings: [],
    panel,
    zoom: 1,
    ready: false,
    imageRect: null,
    pointer: null,
    temporary: null,
    drawing: false,
    selected: null,
    sheet: null,
    save: null,
    issues: [],
    crop: null,
    pendingSteps: false,
    acked: 0,
  };
}

export const useEditorStore = create<EditorState>(() => closedState('ops', 0));

// ===== 純粋な関数 =====

/** 編集できるアイテムか（画像だけ。ゴミ箱・ユニットは呼ぶ側で外す）。 */
export function canEdit(item: Pick<ItemSummaryDto, 'kind'> | null | undefined): boolean {
  return item?.kind === 'image';
}

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
 * 保存（上書き）できるか：開いていて、保存の途中でなく、保存していない変更がある（応えの来ていない手も）か、元の形式に
 * 書けず形式を変えて保存するとき（ボタン・⌘S・ファイル › 保存）。
 */
export function canSaveEditor(
  s: Pick<EditorState, 'phase' | 'save' | 'status' | 'pendingSteps' | 'doc' | 'source'>,
): boolean {
  if (s.phase !== 'open' || s.save || !s.status) return false;
  if (s.status.dirty || s.pendingSteps) return true;
  return effectiveFormat(outputChoice(s.doc.outputs), s.source?.format ?? null).forced;
}

/** 文書を変える手か（まとめを閉じる・仮の値の手は文書を変えない）。 */
export function isDocStep(step: EditStepDto): boolean {
  return step.type !== 'seal' && step.type !== 'preview' && step.type !== 'cancelPreview';
}

/** 編集中のノード（末尾が有効な描画なら、その ID。Rust のキャッシュに入力を固定する）。 */
export function focusNode(doc: EditorDoc): number | null {
  const last = doc.order[doc.order.length - 1];
  const node = last === undefined ? undefined : doc.nodes[last];
  return node && node.op === 'paint.actions' && node.enabled ? node.id : null;
}

/** 表示の結果の要約。 */
export function summarizeView(view: EditViewDto): EditorView {
  return {
    rev: view.rev,
    level: view.level,
    fullWidth: view.fullWidth,
    fullHeight: view.fullHeight,
    frames: view.frames,
    format: view.format,
    fallback: view.fallback,
  };
}

/** 失敗の中身（IPC の誤りは code つき）。 */
export function failureOf(kind: EditorFailure['kind'], error: unknown): EditorFailure {
  if (error instanceof IpcError) return { kind, code: error.code, message: error.message };
  return { kind, code: null, message: error instanceof Error ? error.message : String(error) };
}

function problemFailure(kind: EditorFailure['kind'], p: EditProblemDto): EditorFailure {
  return { kind, code: p.code, message: p.message };
}

/** 開く前の見積もりの要求：窓の大きさから「全体を表示」の倍率を見積もる（段を Rust が決めるのに使う）。 */
export function guessViewport(
  item: Pick<ItemSummaryDto, 'width' | 'height' | 'pixelArt'>,
): EditViewportDto {
  const dpr =
    typeof window !== 'undefined' && window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
  const w = typeof window !== 'undefined' ? window.innerWidth : 1280;
  const h = typeof window !== 'undefined' ? window.innerHeight : 800;
  // 右パネル（340px）・余白・上部と下のカプセルを除いた大きさ
  const area = { w: Math.max(200, w - 340 - 80), h: Math.max(200, h - 180) };
  const zoom =
    item.width && item.height ? fitZoom(item.width, item.height, area, dpr, item.pixelArt) : 1;
  return { frame: 0, rect: null, scale: zoom * dpr, interacting: false, focus: null };
}

// ===== React の外の持ち物 =====

interface Runtime {
  token: number;
  session: string | null;
  controller: CanvasController;
  queue: RequestQueue<PendingStroke> | null;
  returnFocus: HTMLElement | null;
  /** 自己点検（VITE_KURA_CHECK）にタイルを渡したか。 */
  checked: boolean;
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
/** スポイトの問い合わせの番号（最後にクリックした画素の応えだけを使う）。 */
let sampleSeq = 0;

/** タイルの形式の自己点検（dev。VITE_KURA_CHECK=1 のときだけ読み込む）。 */
let tileCheckHook: ((controller: CanvasController, session: string) => void) | null = null;

// 通常のビルドでは import.meta.env.VITE_KURA_CHECK が undefined に置き換わり、この枝ごと消える
if (import.meta.env.VITE_KURA_CHECK === '1') {
  void import('../../dev/tileCheck').then(
    (m) => {
      tileCheckHook = m.checkTiles;
    },
    () => undefined,
  );
}

/** 今のキャンバスの制御（開いていなければ null）。 */
export function editorController(): CanvasController | null {
  return runtime?.controller ?? null;
}

function activeElement(): HTMLElement | null {
  return typeof document !== 'undefined' && document.activeElement instanceof HTMLElement
    ? document.activeElement
    : null;
}

function setInfo(info: ControllerInfo): void {
  useEditorStore.setState({
    zoom: info.zoom,
    ready: info.ready,
    // 仮の絵を置く矩形は、絵が出そろうまでだけ要る
    imageRect: info.ready ? null : info.imageRect,
  });
}

function disposeRuntime(rt: Runtime, discardDraftOf: number | null = null): Promise<void> {
  rt.queue?.close();
  rt.controller.dispose();
  if (!rt.session) return Promise.resolve();
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

/** エディタを開く（画像 1 件だけ。前のエディタは閉じる）。 */
export async function openEditor(
  item: ItemSummaryDto,
  options: { fromDraft?: boolean } = {},
): Promise<void> {
  if (!canEdit(item)) return;
  // パレットの番号は画像ごとに意味が違うので、開くたびに捨てる（スポイトで拾い直す）
  useToolStore.getState().forgetPaletteIndexes();
  const previous = runtime;
  const returnFocus = previous?.returnFocus ?? activeElement();
  if (previous) void disposeRuntime(previous);
  const token = ++openToken;
  const controller = new CanvasController({
    draw: drawScene,
    onInfo: (info) => {
      if (runtime?.token === token) setInfo(info);
    },
    onIdle: (viewport) => {
      if (runtime?.token === token) runtime.queue?.view(viewport);
    },
  });
  const rt: Runtime = {
    token,
    session: null,
    controller,
    queue: null,
    returnFocus,
    checked: false,
    superseded: 0,
    selectCreated: false,
  };
  runtime = rt;
  const panel = useEditorStore.getState().panel;
  useEditorStore.setState({
    ...closedState(panel, token),
    phase: 'opening',
    item,
    pixelArt: item.pixelArt,
  });
  // 開く前の大きさ（仮の絵を置く矩形を出す。開いたら元の大きさで置き直す）
  if (item.width && item.height) {
    controller.setImage({ w: item.width, h: item.height, pixelArt: item.pixelArt });
  }
  const viewport = guessViewport(item);
  try {
    const opened = await openEdit(item.id, viewport, options.fromDraft ?? null);
    if (runtime !== rt) {
      closeEdit(opened.session).catch(() => undefined);
      return;
    }
    rt.session = opened.session;
    controller.setSession(opened.session);
    controller.setImage({
      w: opened.source.width,
      h: opened.source.height,
      pixelArt: opened.pixelArt,
    });
    rt.queue = new RequestQueue<PendingStroke>(
      {
        step: (steps, vp) => stepEdit(opened.session, steps, vp),
        view: (vp) => viewEdit(opened.session, vp),
      },
      {
        sent: (req) => markSent(rt, req),
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
      pixelArt: opened.pixelArt,
      draft: opened.draft,
      // 下書きがあれば、捨てるか下書きから開くかを聞く（下書きから開いたあとは聞かない）
      sheet: opened.draft.exists && !opened.draft.opened ? { kind: 'draft' } : null,
    });
    applyResult(rt, opened.result, 0, viewport, false, null);
    // 開いたあとの実際の大きさ・倍率で頼み直す（見積もりと同じなら送らない）
    rt.queue.view(controller.viewport());
  } catch (error) {
    if (runtime !== rt) return;
    useEditorStore.setState({ phase: 'error', failure: failureOf('open', error) });
  }
}

/** 開き直す（失敗のあと・Rust がセッションを閉じたあと）。 */
export function reopenEditor(): void {
  const item = useEditorStore.getState().item;
  if (item) void openEditor(item);
}

/**
 * エディタを閉じる（必ず `edit_close` を呼ぶ。未保存かは確かめない：確かめるのは `requestCloseEditor`）。`discardDraft` なら
 * 閉じ終えてから下書きを捨てる（「保存しない」）。
 */
export function closeEditor(options: { discardDraft?: boolean } = {}): Promise<void> {
  const rt = runtime;
  runtime = null;
  openToken++;
  const s = useEditorStore.getState();
  if (!rt && s.phase === 'closed') return Promise.resolve();
  const closing = rt
    ? disposeRuntime(rt, options.discardDraft && s.item ? s.item.id : null)
    : Promise.resolve();
  // 切り抜きの途中で閉じたら、道具を戻す（切り抜きは保存した設定から戻さない）
  if (s.crop) useToolStore.getState().setTool(s.crop.returnTool);
  useEditorStore.setState(closedState(s.panel, s.openId));
  const focus = rt?.returnFocus;
  if (focus && focus.isConnected) focus.focus();
  return closing;
}

function markSent(rt: Runtime, req: SentRequest<PendingStroke>): void {
  req.steps.forEach((s, i) => {
    if (s.tag) rt.controller.strokeSent(s.tag, req.seq, i);
  });
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
    rt.controller.onRejected(seq, result.rejected.index);
    notify(result.rejected.problem.message);
  }
  // 切り抜く前の画像を表示する仮の値が断られたら、切り抜きをやめる
  if (result.rejected && s.crop?.previewed && s.crop.bounds === null) {
    patch.crop = null;
    const tools = useToolStore.getState();
    if (tools.tool === 'crop') tools.setTool(s.crop.returnTool);
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
    rt.controller.applyView(outcome.view, seq, viewport);
    const view = summarizeView(outcome.view);
    patch.view = view;
    patch.warnings = outcome.view.warnings;
    if (s.failure?.kind !== 'open') patch.failure = null;
    if (view.frames.length > 0 && s.frame >= view.frames.length) {
      patch.frame = 0;
      rt.controller.setFrame(0);
    }
    // 前に選んだ消しゴムのまま、透明を持てない画像を開いたらペンに戻す
    const tools = useToolStore.getState();
    if (tools.tool === 'eraser' && !formatCanBeTransparent(view.format)) tools.setTool('pen');
    // 切り抜きの枠：その操作の入力（仮に無効にした結果）を表示し終えたら、その大きさで枠を置く
    const crop = s.crop;
    if (crop && crop.bounds === null && (!crop.previewed || result.status.provisional)) {
      const bounds = { w: view.fullWidth, h: view.fullHeight };
      patch.crop = { ...crop, bounds, rect: clampCrop(crop.rect, bounds) };
    }
    if (!rt.checked && rt.session && tileCheckHook) {
      rt.checked = true;
      tileCheckHook(rt.controller, rt.session);
    }
  } else if (outcome.type === 'superseded') {
    // 当てた手は残る。新しい要求の結果を待つ（出していなければ表示を頼み直す）
    rt.superseded++;
    if (rt.superseded <= SUPERSEDED_RETRIES) rt.queue?.view(rt.controller.viewport(), true);
  } else {
    rt.controller.onFailed(seq);
    patch.failure = problemFailure('view', outcome.problem);
  }
  rt.controller.focus = focusNode(doc);
  useEditorStore.setState(patch);
  for (const hook of resultHooks) hook(result, hadSteps);
}

function requestFailed(rt: Runtime, req: SentRequest<PendingStroke>, error: unknown): void {
  rt.controller.onFailed(req.seq);
  const s = useEditorStore.getState();
  useEditorStore.setState({
    failure: failureOf('request', error),
    pendingSteps: pendingOf(rt),
    // 届かなかった手の結果も確定（引数の欄は文書の値に戻す）
    ...(req.steps.length > 0 && req.upto > s.acked ? { acked: req.upto } : {}),
  });
}

/** 手をためて送り、応えの来ていない文書の手があるかをストアに合わせる。 */
function queueSteps(rt: Runtime, steps: QueuedStep<PendingStroke>[]): void {
  if (!rt.queue) return;
  rt.queue.push(steps, rt.controller.viewport());
  const pending = pendingOf(rt);
  if (useEditorStore.getState().pendingSteps !== pending) {
    useEditorStore.setState({ pendingSteps: pending });
  }
}

/** 手を送る（開いていなければ false）。切り抜きの途中なら、先に切り抜きをやめる（仮の値を捨てる）。 */
function pushSteps(steps: QueuedStep<PendingStroke>[]): boolean {
  const rt = runtime;
  if (!rt?.queue) return false;
  if (useEditorStore.getState().crop) cancelCrop();
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
  rt?.queue?.view(rt.controller.viewport(), true);
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

/** 取り消す（⌘Z。描いている途中は何もしない。切り抜きの途中なら切り抜きをやめてから。取り消せる手がなければ Rust が何もしない）。 */
export function editorUndo(): void {
  if (runtime?.controller.drawing) return;
  pushSteps([{ step: { type: 'undo' } }]);
}

/** やり直す（⇧⌘Z）。 */
export function editorRedo(): void {
  if (runtime?.controller.drawing) return;
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

/** 今の出力の大きさ（足す操作の既定値に使う）。 */
function outputSize(): Bounds | null {
  const v = useEditorStore.getState().view;
  return v ? { w: v.fullWidth, h: v.fullHeight } : null;
}

/** 操作を足す（選んだ操作のあと、選んでいなければ末尾。足したら選ぶ）。 */
export function addOp(id: string): void {
  const desc = opSpec(id);
  const { doc, selected, crop } = useEditorStore.getState();
  if (!desc) return;
  const i = selected === null ? -1 : doc.order.indexOf(selected);
  const at = i < 0 ? doc.order.length : i + 1;
  // 末尾に足すときだけ、今の出力の大きさを既定にする（途中の入力の大きさはフロントでは分からない）。切り抜きの枠を
  // 出している間の表示は切り抜く前の画像なので、切り抜きを始める前の出力の大きさを使う（足すと切り抜きはやめる）
  const size = at === doc.order.length ? (crop ? crop.output : outputSize()) : null;
  const rt = runtime;
  if (rt) rt.selectCreated = true;
  pushSteps(
    addSteps(at, desc.id, desc.version, defaultParams(desc, size)).map((step) => ({ step })),
  );
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

// ===== 切り抜き =====

/**
 * 切り抜きの道具を選ぶ：選んだ切り抜き（なければ最後の切り抜き）を直す。なければ末尾に足す枠。直す切り抜きの後ろに有効な
 * 幾何の操作があれば、枠は出さずにその切り抜きを選んで引数パネルで直す（仮に無効にしても表示がその入力にならない）。
 */
export function startCrop(): void {
  const rt = runtime;
  const s = useEditorStore.getState();
  if (!rt?.queue || s.phase !== 'open' || s.crop) return;
  strokeCancel();
  const tools = useToolStore.getState();
  const returnTool: EditorTool = tools.tool === 'crop' ? 'pen' : tools.tool;
  const pick = (id: number | null | undefined) =>
    id != null && s.doc.nodes[id]?.op === 'image.crop' ? id : null;
  const node =
    pick(s.selected) ??
    [...s.doc.order].reverse().find((id) => s.doc.nodes[id]?.op === 'image.crop') ??
    null;
  if (node !== null && hasLaterGeometry(s.doc, node)) {
    useEditorStore.setState({ selected: node, panel: 'ops' });
    notify(t.cropUseParams);
    return;
  }
  tools.setTool('crop');
  const output = outputSize();
  if (node === null) {
    useEditorStore.setState({
      crop: {
        node: null,
        rect: output ? { x: 0, y: 0, w: output.w, h: output.h } : { x: 0, y: 0, w: 1, h: 1 },
        bounds: output,
        output,
        previewed: false,
        returnTool,
      },
    });
    return;
  }
  const n = s.doc.nodes[node];
  const rect = readRect(n ? paramsOf(n).rect : null) ?? { x: 0, y: 0, w: 1, h: 1 };
  const previewed = n?.enabled ?? false;
  useEditorStore.setState({
    selected: node,
    crop: { node, rect, bounds: previewed ? null : output, output, previewed, returnTool },
  });
  // その操作の入力を表示する（仮に無効にする。履歴に入らない。後ろに幾何の操作はないので、出力がその入力になる）
  if (previewed) {
    queueSteps(rt, [
      { step: { type: 'preview', edit: { type: 'setEnabled', node, enabled: false } } },
    ]);
  }
}

/** 切り抜きを終える（仮の値を捨て、道具を戻す）。`steps` があれば続けて送る。 */
function endCrop(steps: EditStepDto[] = []): void {
  const rt = runtime;
  const crop = useEditorStore.getState().crop;
  if (!crop) return;
  useEditorStore.setState({ crop: null });
  const tools = useToolStore.getState();
  if (tools.tool === 'crop') tools.setTool(crop.returnTool);
  const all: EditStepDto[] = [
    ...(crop.previewed ? [{ type: 'cancelPreview' as const }] : []),
    ...steps,
  ];
  if (rt?.queue && all.length > 0) {
    queueSteps(
      rt,
      all.map((step) => ({ step })),
    );
  }
}

/** 切り抜きをやめる（esc・ほかの道具・ほかの手）。 */
export function cancelCrop(): void {
  endCrop();
}

/** 切り抜きを確定する（↵）。枠が変わっていなければ何も足さない。 */
export function confirmCrop(): void {
  const s = useEditorStore.getState();
  const crop = s.crop;
  if (!crop?.bounds) return;
  const steps: EditStepDto[] = [];
  if (crop.node !== null) {
    const node = s.doc.nodes[crop.node];
    if (!sameRect(readRect(node ? paramsOf(node).rect : null), crop.rect)) {
      steps.push(
        commitStep({ type: 'setParam', node: crop.node, key: 'rect', value: { ...crop.rect } }),
      );
    }
    if (node && !node.enabled) {
      steps.push(commitStep({ type: 'setEnabled', node: crop.node, enabled: true }));
    }
  } else if (!isFullCrop(crop.rect, crop.bounds)) {
    const desc = opSpec('image.crop');
    if (desc) {
      steps.push(
        ...addSteps(s.doc.order.length, desc.id, desc.version, { rect: { ...crop.rect } }),
      );
      if (runtime) runtime.selectCreated = true;
    }
  }
  endCrop(steps);
}

/** 枠を替える（ドラッグ・数の欄。画像の中の整数に収める）。 */
export function setCropRect(rect: Rect): void {
  const crop = useEditorStore.getState().crop;
  if (!crop?.bounds) return;
  const next = clampCrop(rect, crop.bounds);
  if (!sameRect(next, crop.rect)) useEditorStore.setState({ crop: { ...crop, rect: next } });
}

/** 矢印で枠を動かす（1px・⇧で 10px）。動かせたら true。 */
export function nudgeCropRect(key: string, shift: boolean): boolean {
  const crop = useEditorStore.getState().crop;
  if (!crop?.bounds) return false;
  const next = nudgeCrop(crop.rect, key, shift, crop.bounds);
  if (!next) return false;
  setCropRect(next);
  return true;
}

/** 道具を選ぶ（切り抜きは枠を出す。ほかの道具を選んだら切り抜きをやめる）。 */
export function selectTool(tool: EditorTool): void {
  if (tool === 'crop') {
    startCrop();
    return;
  }
  if (useEditorStore.getState().crop) cancelCrop();
  useToolStore.getState().setTool(tool);
}

// ===== 表示 =====

export function editorFit(): void {
  runtime?.controller.fit();
}

export function editorActualSize(): void {
  runtime?.controller.actualSize();
}

export function editorZoom(dir: number): void {
  runtime?.controller.zoomStep(dir);
}

/** 表示するコマ（アニメの画像）。 */
export function setEditorFrame(frame: number): void {
  const s = useEditorStore.getState();
  const count = s.view?.frames.length ?? 1;
  if (count <= 1 || frame < 0 || frame >= count || frame === s.frame) return;
  runtime?.controller.cancelStroke();
  useEditorStore.setState({ frame, drawing: false });
  runtime?.controller.setFrame(frame);
}

/** 前・次のコマ（端で回る）。 */
export function stepEditorFrame(delta: number): void {
  const s = useEditorStore.getState();
  const count = s.view?.frames.length ?? 1;
  if (count <= 1) return;
  setEditorFrame((((s.frame + delta) % count) + count) % count);
}

/** 右パネルの「情報」を切り替える（⌘I）。 */
export function toggleEditorInfo(): void {
  const s = useEditorStore.getState();
  useEditorStore.setState({ panel: s.panel === 'info' ? 'ops' : 'info' });
}

/** 押している間だけの道具。 */
export function setTemporaryTool(tool: 'hand' | 'eyedropper' | null): void {
  if (useEditorStore.getState().temporary !== tool) useEditorStore.setState({ temporary: tool });
}

/** ポインタの下の画素（画像の外は null）。 */
export function setEditorPointer(p: Point | null): void {
  const now = useEditorStore.getState().pointer;
  if (now === p || (now && p && now[0] === p[0] && now[1] === p[1])) return;
  useEditorStore.setState({ pointer: p });
}

// ===== 描画の道具 =====

/** パレットの画像か。 */
function indexedNow(): boolean {
  return useEditorStore.getState().view?.format === 'indexed8';
}

/** 描く色を使えるか：パレットの画像は、この画像でスポイトで拾った番号の色だけで描く（なければ拾うよう知らせる）。 */
function paletteColorReady(): boolean {
  if (!indexedNow() || useToolStore.getState().color.index !== null) return true;
  notify(t.indexedPick);
  return false;
}

/** 筆の条件（道具・筆・色・フレーム）。描けなければ null。 */
function strokeSpec(tool: 'pen' | 'eraser'): StrokeSpec | null {
  const rt = runtime;
  const bounds = rt?.controller.frameBounds();
  if (!rt || !bounds) return null;
  const tools = useToolStore.getState();
  return {
    tool,
    color: tool === 'pen' ? tools.color : null,
    blend: tools.blend,
    brush: { shape: tools.shape, size: tools.size },
    pixelPerfect: tools.pixelPerfect,
    frame: useEditorStore.getState().frame,
    bounds,
    indexed: indexedNow(),
  };
}

/** 筆を描き始める（⇧ なら、最後に描いた筆の終わりからの直線）。 */
export function strokeStart(p: Point, shift: boolean): boolean {
  const rt = runtime;
  if (!rt?.queue) return false;
  const tool = useToolStore.getState().tool === 'eraser' ? 'eraser' : 'pen';
  if (tool === 'eraser' && !formatCanBeTransparent(useEditorStore.getState().view?.format)) {
    notify(t.eraserUnavailable);
    return false;
  }
  if (tool === 'pen' && !paletteColorReady()) return false;
  const spec = strokeSpec(tool);
  if (!spec) return false;
  const start = shift ? rt.controller.lineStart(spec.frame) : null;
  rt.controller.beginStroke(spec, start ?? p);
  if (start) rt.controller.extendStroke([p]);
  useEditorStore.setState({ drawing: true });
  return true;
}

/** 点を足す。 */
export function strokeMove(points: readonly Point[]): void {
  runtime?.controller.extendStroke(points);
}

/** 描き終えて送る（1 手。描く先は Rust が決める）。 */
export function strokeEnd(): void {
  const rt = runtime;
  if (!rt) return;
  const done = rt.controller.endStroke();
  useEditorStore.setState({ drawing: false });
  if (!done) return;
  pushSteps([{ step: { type: 'paint', actions: [done.action] }, tag: done.pending }]);
  if (done.action.tool === 'pen') {
    const tools = useToolStore.getState();
    tools.rememberColor(tools.color);
  }
}

/** 描いている筆をやめる（esc。送らない）。やめたら true。 */
export function strokeCancel(): boolean {
  const canceled = runtime?.controller.cancelStroke() ?? false;
  if (canceled) useEditorStore.setState({ drawing: false });
  return canceled;
}

/** 塗りつぶす（仮描きしない。画像の外は何もしない）。 */
export function fillAt(p: Point): void {
  const rt = runtime;
  if (!rt?.queue || !rt.controller.contains(p) || !paletteColorReady()) return;
  const tools = useToolStore.getState();
  const action = fillAction(
    {
      color: tools.color,
      tolerance: tools.tolerance,
      connectivity: tools.connectivity,
      contiguous: tools.contiguous,
      frame: useEditorStore.getState().frame,
      indexed: indexedNow(),
    },
    p,
  );
  if (pushSteps([{ step: { type: 'paint', actions: [action] } }])) {
    tools.rememberColor(tools.color);
  }
}

/**
 * スポイト：等倍の今の値の 1 画素を色にする（クリックのときだけ。画像の外は何もしない）。応えの順は約束されないので、
 * 最後にクリックした画素の応えだけを使う。
 */
export async function sampleAt(p: Point): Promise<void> {
  const rt = runtime;
  if (!rt?.session || !rt.controller.contains(p)) return;
  const n = ++sampleSeq;
  try {
    const got = await sampleEdit(rt.session, useEditorStore.getState().frame, p[0], p[1]);
    if (runtime !== rt || n !== sampleSeq || !got) return;
    useToolStore.getState().setColor({ rgba: [...got.rgba], index: got.index });
  } catch (error) {
    if (runtime !== rt || n !== sampleSeq) return;
    notify(t.sampleFailed(isIpcError(error) ? error.message : String(error)));
  }
}
