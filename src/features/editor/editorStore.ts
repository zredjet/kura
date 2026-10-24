/**
 * 画像エディタの状態と操作（M12 の段 5。docs/editing.md §12.8・§12.12、design.md §3.1「画像エディタ」）。
 *
 * - 画面に出すもの（セッション・状態・文書の要約・表示の段と大きさ・失敗・右パネル・倍率の表示）は Zustand に置く。
 * - キャンバス（タイル・ズーム・パン・仮の筆）は React の外の `CanvasController`、要求は `RequestQueue`（1 つだけ出す）。
 *   開くたびに作り、閉じたら捨てる（ImageBitmap は close）。
 * - 開く（`openEditor`）：画像 1 件だけ。前のエディタは閉じる。閉じる（`closeEditor`）：必ず `edit_close` を呼ぶ
 *   （未保存で閉じるときのシートは段 5b）。ライブラリを切り替える・閉じるときも閉じる（App）。
 * - 描画の道具：筆は描いている間フロントで仮に重ね、離したら `paint{actions:[筆]}`（描く先は Rust が決める）。
 *   塗りつぶしは仮描きせずに送る。スポイトはクリックのときだけ `edit_sample`。
 */

import { create } from 'zustand';

import { ja } from '../../i18n/ja';
import {
  closeEdit,
  type EditDocDeltaDto,
  type EditDraftDto,
  type EditFrameDto,
  type EditNodeDto,
  type EditProblemDto,
  type EditSourceDto,
  type EditStatusDto,
  type EditStepResultDto,
  type EditViewDto,
  type EditViewportDto,
  type Issue,
  openEdit,
  sampleEdit,
  stepEdit,
  viewEdit,
} from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import { IpcError, isIpcError } from '../../ipc/result';
import type { Point } from '../../lib/paint';
import { notify } from '../../stores/noticeStore';
import { CanvasController, type ControllerInfo } from './canvasController';
import { drawScene } from './drawScene';
import { type QueuedStep, RequestQueue, type SentRequest } from './requestQueue';
import { fillAction, type PendingStroke, type StrokeSpec } from './stroke';
import { useToolStore } from './toolStore';
import { fitZoom, formatCanBeTransparent, type Rect } from './viewMath';

const t = ja.editor;

export type EditorPhase = 'closed' | 'opening' | 'open' | 'error';
export type EditorPanel = 'ops' | 'info';

/** 文書の要約（ノードの並びと、各ノードの要約）。 */
export interface EditorDoc {
  order: number[];
  nodes: Record<number, EditNodeDto>;
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
}

const EMPTY_DOC: EditorDoc = { order: [], nodes: {} };

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
  if (delta.base !== null && delta.nodes.length === 0 && sameOrder) return doc;
  const known: Record<number, EditNodeDto> = delta.base === null ? {} : { ...doc.nodes };
  for (const n of delta.nodes) known[n.id] = n;
  const nodes: Record<number, EditNodeDto> = {};
  for (const id of delta.order) {
    const n = known[id];
    if (n) nodes[id] = n;
  }
  return { order: [...delta.order], nodes };
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
}

/** `superseded` のあとに表示を頼み直す回数の上限（続けて取って代わられたら、次の操作を待つ）。 */
const SUPERSEDED_RETRIES = 2;

let runtime: Runtime | null = null;
let openToken = 0;

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

function disposeRuntime(rt: Runtime): void {
  rt.queue?.close();
  rt.controller.dispose();
  if (rt.session) {
    closeEdit(rt.session).catch((error: unknown) =>
      console.warn('エディタを閉じられませんでした', error),
    );
  }
}

/** エディタを開く（画像 1 件だけ。前のエディタは閉じる）。 */
export async function openEditor(
  item: ItemSummaryDto,
  options: { fromDraft?: boolean } = {},
): Promise<void> {
  if (!canEdit(item)) return;
  const previous = runtime;
  const returnFocus = previous?.returnFocus ?? activeElement();
  if (previous) disposeRuntime(previous);
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
          if (runtime === rt) applyResult(rt, result, req.seq, req.viewport);
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
    });
    applyResult(rt, opened.result, 0, viewport);
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

/** エディタを閉じる（必ず `edit_close` を呼ぶ）。 */
export function closeEditor(): void {
  const rt = runtime;
  runtime = null;
  openToken++;
  const s = useEditorStore.getState();
  if (!rt && s.phase === 'closed') return;
  if (rt) disposeRuntime(rt);
  useEditorStore.setState(closedState(s.panel, s.openId));
  const focus = rt?.returnFocus;
  if (focus && focus.isConnected) focus.focus();
}

function markSent(rt: Runtime, req: SentRequest<PendingStroke>): void {
  req.steps.forEach((s, i) => {
    if (s.tag) rt.controller.strokeSent(s.tag, req.seq, i);
  });
}

/** 手と表示の結果を受ける。 */
function applyResult(
  rt: Runtime,
  result: EditStepResultDto,
  seq: number,
  viewport: EditViewportDto,
): void {
  const s = useEditorStore.getState();
  const doc = applyDocDelta(s.doc, result.doc);
  const patch: Partial<EditorState> = { status: result.status, doc };
  for (const n of result.notices) notify(n);
  if (result.rejected) {
    rt.controller.onRejected(seq, result.rejected.index);
    notify(result.rejected.problem.message);
  }
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
}

function requestFailed(rt: Runtime, req: SentRequest<PendingStroke>, error: unknown): void {
  rt.controller.onFailed(req.seq);
  useEditorStore.setState({ failure: failureOf('request', error) });
}

/** 手を送る（開いていなければ false）。 */
function pushSteps(steps: QueuedStep<PendingStroke>[]): boolean {
  const rt = runtime;
  if (!rt?.queue) return false;
  rt.queue.push(steps, rt.controller.viewport());
  return true;
}

// ===== 取り消し =====

/** 取り消す（⌘Z。描いている途中は何もしない。取り消せる手がなければ Rust が何もしない）。 */
export function editorUndo(): void {
  if (runtime?.controller.drawing) return;
  pushSteps([{ step: { type: 'undo' } }]);
}

/** やり直す（⇧⌘Z）。 */
export function editorRedo(): void {
  if (runtime?.controller.drawing) return;
  pushSteps([{ step: { type: 'redo' } }]);
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
  if (!rt?.queue || !rt.controller.contains(p)) return;
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

/** スポイト：等倍の今の値の 1 画素を色にする（クリックのときだけ。画像の外は何もしない）。 */
export async function sampleAt(p: Point): Promise<void> {
  const rt = runtime;
  if (!rt?.session || !rt.controller.contains(p)) return;
  try {
    const got = await sampleEdit(rt.session, useEditorStore.getState().frame, p[0], p[1]);
    if (runtime !== rt || !got) return;
    useToolStore.getState().setColor({ rgba: [...got.rgba], index: got.index });
  } catch (error) {
    if (runtime !== rt) return;
    notify(t.sampleFailed(isIpcError(error) ? error.message : String(error)));
  }
}
