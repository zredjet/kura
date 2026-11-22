/**
 * 画像エディタの表示と道具の状態・操作（M12 の段 5。M13 の段 6a で核の `editorStore.ts` から分けた。docs/editing.md
 * §12.8・§12.12、design.md §3.1「画像エディタ」）。
 *
 * - 核の `EditorAdapter` を画像で作る（`IMAGE_ADAPTER`）。キャンバス（タイル・ズーム・パン・仮の筆）は React の外の
 *   `CanvasController`。開くたびに作り、閉じたら捨てる（ImageBitmap は close）。
 * - 画面に出すもの（表示の段と大きさ・フレーム・倍率・ポインタ・押している間だけの道具・切り抜きの枠）は Zustand
 *   （`useImageStore`）。文書・状態・保存は核のストア。
 * - 結果 1 つ分の変更はためて、`settle` で 1 回で当てる（切り抜きの判断は結果の前の状態で行う。分ける前と同じ）。
 * - 切り抜きの枠（C）：その操作の入力を表示して（操作を仮に無効にする preview）枠を重ね、↵ で確定する。後ろに有効な
 *   幾何の操作（大きさ・向き・並びを変える）があれば、仮に無効にしても入力にならないので、枠は出さずに引数パネルで直す。
 * - 描画の道具：筆は描いている間フロントで仮に重ね、離したら `paint{actions:[筆]}`（描く先は Rust が決める）。
 *   塗りつぶしは仮描きせずに送る。スポイトはクリックのときだけ `edit_sample`。
 */

import { create } from 'zustand';

import { ja } from '../../../i18n/ja';
import {
  type EditFrameDto,
  type EditStepDto,
  type EditImageView,
  type EditImageViewport,
  sampleEdit,
} from '../../../ipc/edit';
import type { ItemSummaryDto } from '../../../ipc/items';
import { isIpcError } from '../../../ipc/result';
import type { Point } from '../../../lib/paint';
import { notify } from '../../../stores/noticeStore';
import {
  type EditorAdapter,
  type EditorAdapterContext,
  type EditorAdapterFactory,
  type EditorDoc,
  useEditorStore,
} from '../editorStore';
import { defaultParams, hasLaterGeometry, opSpec, paramsOf } from '../opsCatalog';
import { addSteps, commitStep } from '../paramSend';
import { CanvasController, type ControllerInfo } from './canvasController';
import { type Bounds, clampCrop, isFullCrop, nudgeCrop, readRect, sameRect } from './crop';
import { drawScene } from './drawScene';
import { fillAction, type PendingStroke, type StrokeSpec } from './stroke';
import { type EditorTool, useToolStore } from './toolStore';
import { fitZoom, formatCanBeTransparent, type Rect } from './viewMath';

const t = ja.editor;

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

export interface ImageEditorState {
  pixelArt: boolean;
  view: EditorView | null;
  frame: number;
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
  crop: EditorCrop | null;
}

function initialState(pixelArt: boolean): ImageEditorState {
  return {
    pixelArt,
    view: null,
    frame: 0,
    zoom: 1,
    ready: false,
    imageRect: null,
    pointer: null,
    temporary: null,
    drawing: false,
    crop: null,
  };
}

export const useImageStore = create<ImageEditorState>(() => initialState(false));

// ===== 純粋な関数 =====

/** 編集中のノード（末尾が有効な描画なら、その ID。Rust のキャッシュに入力を固定する）。 */
export function focusNode(doc: EditorDoc): number | null {
  const last = doc.order[doc.order.length - 1];
  const node = last === undefined ? undefined : doc.nodes[last];
  return node && node.op === 'paint.actions' && node.enabled ? node.id : null;
}

/** 表示の結果の要約。 */
export function summarizeView(view: EditImageView): EditorView {
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

/** 開く前の見積もりの要求：窓の大きさから「全体を表示」の倍率を見積もる（段を Rust が決めるのに使う）。 */
export function guessViewport(
  item: Pick<ItemSummaryDto, 'width' | 'height' | 'pixelArt'>,
): EditImageViewport {
  const dpr =
    typeof window !== 'undefined' && window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
  const w = typeof window !== 'undefined' ? window.innerWidth : 1280;
  const h = typeof window !== 'undefined' ? window.innerHeight : 800;
  // 右パネル（340px）・余白・上部と下のカプセルを除いた大きさ
  const area = { w: Math.max(200, w - 340 - 80), h: Math.max(200, h - 180) };
  const zoom =
    item.width && item.height ? fitZoom(item.width, item.height, area, dpr, item.pixelArt) : 1;
  return {
    type: 'image',
    frame: 0,
    rect: null,
    scale: zoom * dpr,
    interacting: false,
    focus: null,
  };
}

// ===== React の外の持ち物 =====

interface ImageRuntime {
  ctx: EditorAdapterContext<PendingStroke>;
  controller: CanvasController;
  /** 自己点検（VITE_KURA_CHECK）にタイルを渡したか。 */
  checked: boolean;
  /** 結果 1 つ分の変更（`settle` で当てる）。 */
  patch: Partial<ImageEditorState>;
}

let current: ImageRuntime | null = null;
/** スポイトの問い合わせの番号（最後にクリックした画素の応えだけを使う）。 */
let sampleSeq = 0;

/** タイルの形式の自己点検（dev。VITE_KURA_CHECK=1 のときだけ読み込む）。 */
let tileCheckHook: ((controller: CanvasController, session: string) => void) | null = null;

// 通常のビルドでは import.meta.env.VITE_KURA_CHECK が undefined に置き換わり、この枝ごと消える
if (import.meta.env.VITE_KURA_CHECK === '1') {
  void import('../../../dev/tileCheck').then(
    (m) => {
      tileCheckHook = m.checkTiles;
    },
    () => undefined,
  );
}

/** 今のキャンバスの制御（開いていなければ null）。 */
export function editorController(): CanvasController | null {
  return current?.controller ?? null;
}

/** 今のエディタの画像の持ち物（核が閉じた・開き直したあとは null）。 */
function live(): ImageRuntime | null {
  return current?.ctx.current() ? current : null;
}

/** 開いていて手を送れる画像の持ち物（セッションがまだなら null）。 */
function opened(): ImageRuntime | null {
  const img = live();
  return img?.ctx.session() ? img : null;
}

function setInfo(info: ControllerInfo): void {
  useImageStore.setState({
    zoom: info.zoom,
    ready: info.ready,
    // 仮の絵を置く矩形は、絵が出そろうまでだけ要る
    imageRect: info.ready ? null : info.imageRect,
  });
}

/** 今の出力の大きさ（足す操作の既定値に使う）。 */
function outputSize(): Bounds | null {
  const v = useImageStore.getState().view;
  return v ? { w: v.fullWidth, h: v.fullHeight } : null;
}

/** 画像のアダプタを作る。 */
function createImageAdapter(
  item: ItemSummaryDto,
  ctx: EditorAdapterContext<PendingStroke>,
): EditorAdapter<PendingStroke> {
  // パレットの番号は画像ごとに意味が違うので、開くたびに捨てる（スポイトで拾い直す）
  useToolStore.getState().forgetPaletteIndexes();
  const controller = new CanvasController({
    draw: drawScene,
    onInfo: (info) => {
      if (ctx.current()) setInfo(info);
    },
    onIdle: (viewport) => {
      if (ctx.current()) ctx.requestView(viewport);
    },
  });
  const img: ImageRuntime = { ctx, controller, checked: false, patch: {} };
  current = img;
  return {
    begin() {
      useImageStore.setState(initialState(item.pixelArt));
      // 開く前の大きさ（仮の絵を置く矩形を出す。開いたら元の大きさで置き直す）
      if (item.width && item.height) {
        controller.setImage({ w: item.width, h: item.height, pixelArt: item.pixelArt });
      }
    },
    initialViewport: () => guessViewport(item),
    opened(opened) {
      controller.setSession(opened.session);
      controller.setImage({
        w: opened.source.width,
        h: opened.source.height,
        pixelArt: opened.pixelArt,
      });
      useImageStore.setState({ pixelArt: opened.pixelArt });
    },
    viewport: () => controller.viewport(),
    sent(req) {
      req.steps.forEach((s, i) => {
        if (s.tag) controller.strokeSent(s.tag, req.seq, i);
      });
    },
    onRejected(seq, index) {
      controller.onRejected(seq, index);
      // 切り抜く前の画像を表示する仮の値が断られたら、切り抜きをやめる
      const crop = useImageStore.getState().crop;
      if (crop?.previewed && crop.bounds === null) {
        img.patch.crop = null;
        const tools = useToolStore.getState();
        if (tools.tool === 'crop') tools.setTool(crop.returnTool);
      }
    },
    applyView(view, info) {
      if (view.type !== 'image') return;
      const requested = info.viewport.type === 'image' ? info.viewport : controller.viewport();
      controller.applyView(view, info.seq, requested);
      const s = useImageStore.getState();
      const summary = summarizeView(view);
      img.patch.view = summary;
      if (summary.frames.length > 0 && s.frame >= summary.frames.length) {
        img.patch.frame = 0;
        controller.setFrame(0);
      }
      // 前に選んだ消しゴムのまま、透明を持てない画像を開いたらペンに戻す
      const tools = useToolStore.getState();
      if (tools.tool === 'eraser' && !formatCanBeTransparent(summary.format)) tools.setTool('pen');
      // 切り抜きの枠：その操作の入力（仮に無効にした結果）を表示し終えたら、その大きさで枠を置く
      const crop = s.crop;
      if (crop && crop.bounds === null && (!crop.previewed || info.provisional)) {
        const bounds = { w: summary.fullWidth, h: summary.fullHeight };
        img.patch.crop = { ...crop, bounds, rect: clampCrop(crop.rect, bounds) };
      }
      const session = ctx.session();
      if (!img.checked && session && tileCheckHook) {
        img.checked = true;
        tileCheckHook(controller, session);
      }
    },
    onFailed(seq) {
      controller.onFailed(seq);
    },
    settle(doc) {
      controller.focus = focusNode(doc);
      const patch = img.patch;
      img.patch = {};
      if (Object.keys(patch).length > 0) useImageStore.setState(patch);
    },
    beforeSteps() {
      if (useImageStore.getState().crop) cancelCrop();
    },
    busy: () => controller.drawing,
    newParams(desc, atEnd) {
      // 末尾に足すときだけ、今の出力の大きさを既定にする（途中の入力の大きさはフロントでは分からない）。切り抜きの枠を
      // 出している間の表示は切り抜く前の画像なので、切り抜きを始める前の出力の大きさを使う（足すと切り抜きはやめる）
      const crop = useImageStore.getState().crop;
      return defaultParams(desc, atEnd ? (crop ? crop.output : outputSize()) : null);
    },
    dispose(closing) {
      controller.dispose();
      if (current === img) current = null;
      if (!closing) return;
      // 切り抜きの途中で閉じたら、道具を戻す（切り抜きは保存した設定から戻さない）
      const crop = useImageStore.getState().crop;
      if (crop) useToolStore.getState().setTool(crop.returnTool);
      useImageStore.setState(initialState(false));
    },
  };
}

/** 画像のアダプタの作り方（`editorKinds.ts` が開く口に渡す）。 */
export const IMAGE_ADAPTER: EditorAdapterFactory<PendingStroke> = {
  kind: 'image',
  create: createImageAdapter,
};

// ===== 切り抜き =====

/**
 * 切り抜きの道具を選ぶ：選んだ切り抜き（なければ最後の切り抜き）を直す。なければ末尾に足す枠。直す切り抜きの後ろに有効な
 * 幾何の操作があれば、枠は出さずにその切り抜きを選んで引数パネルで直す（仮に無効にしても表示がその入力にならない）。
 */
export function startCrop(): void {
  const img = opened();
  const s = useEditorStore.getState();
  if (!img || s.phase !== 'open' || useImageStore.getState().crop) return;
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
    useImageStore.setState({
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
  useEditorStore.setState({ selected: node });
  useImageStore.setState({
    crop: { node, rect, bounds: previewed ? null : output, output, previewed, returnTool },
  });
  // その操作の入力を表示する（仮に無効にする。履歴に入らない。後ろに幾何の操作はないので、出力がその入力になる）
  if (previewed) {
    img.ctx.send([
      { step: { type: 'preview', edit: { type: 'setEnabled', node, enabled: false } } },
    ]);
  }
}

/** 切り抜きを終える（仮の値を捨て、道具を戻す）。`steps` があれば続けて送る。 */
function endCrop(steps: EditStepDto[] = [], selectCreated = false): void {
  const crop = useImageStore.getState().crop;
  if (!crop) return;
  useImageStore.setState({ crop: null });
  const tools = useToolStore.getState();
  if (tools.tool === 'crop') tools.setTool(crop.returnTool);
  const all: EditStepDto[] = [
    ...(crop.previewed ? [{ type: 'cancelPreview' as const }] : []),
    ...steps,
  ];
  live()?.ctx.send(
    all.map((step) => ({ step })),
    { selectCreated },
  );
}

/** 切り抜きをやめる（esc・ほかの道具・ほかの手）。 */
export function cancelCrop(): void {
  endCrop();
}

/** 切り抜きを確定する（↵）。枠が変わっていなければ何も足さない。 */
export function confirmCrop(): void {
  const s = useEditorStore.getState();
  const crop = useImageStore.getState().crop;
  if (!crop?.bounds) return;
  const steps: EditStepDto[] = [];
  let selectCreated = false;
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
      selectCreated = true;
    }
  }
  endCrop(steps, selectCreated);
}

/** 枠を替える（ドラッグ・数の欄。画像の中の整数に収める）。 */
export function setCropRect(rect: Rect): void {
  const crop = useImageStore.getState().crop;
  if (!crop?.bounds) return;
  const next = clampCrop(rect, crop.bounds);
  if (!sameRect(next, crop.rect)) useImageStore.setState({ crop: { ...crop, rect: next } });
}

/** 矢印で枠を動かす（1px・⇧で 10px）。動かせたら true。 */
export function nudgeCropRect(key: string, shift: boolean): boolean {
  const crop = useImageStore.getState().crop;
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
  if (useImageStore.getState().crop) cancelCrop();
  useToolStore.getState().setTool(tool);
}

// ===== 表示 =====

export function editorFit(): void {
  live()?.controller.fit();
}

export function editorActualSize(): void {
  live()?.controller.actualSize();
}

export function editorZoom(dir: number): void {
  live()?.controller.zoomStep(dir);
}

/** 表示するコマ（アニメの画像）。 */
export function setEditorFrame(frame: number): void {
  const s = useImageStore.getState();
  const count = s.view?.frames.length ?? 1;
  if (count <= 1 || frame < 0 || frame >= count || frame === s.frame) return;
  live()?.controller.cancelStroke();
  useImageStore.setState({ frame, drawing: false });
  live()?.controller.setFrame(frame);
}

/** 前・次のコマ（端で回る）。 */
export function stepEditorFrame(delta: number): void {
  const s = useImageStore.getState();
  const count = s.view?.frames.length ?? 1;
  if (count <= 1) return;
  setEditorFrame((((s.frame + delta) % count) + count) % count);
}

/** 押している間だけの道具。 */
export function setTemporaryTool(tool: 'hand' | 'eyedropper' | null): void {
  if (useImageStore.getState().temporary !== tool) useImageStore.setState({ temporary: tool });
}

/** ポインタの下の画素（画像の外は null）。 */
export function setEditorPointer(p: Point | null): void {
  const now = useImageStore.getState().pointer;
  if (now === p || (now && p && now[0] === p[0] && now[1] === p[1])) return;
  useImageStore.setState({ pointer: p });
}

// ===== 描画の道具 =====

/** パレットの画像か。 */
function indexedNow(): boolean {
  return useImageStore.getState().view?.format === 'indexed8';
}

/** 描く色を使えるか：パレットの画像は、この画像でスポイトで拾った番号の色だけで描く（なければ拾うよう知らせる）。 */
function paletteColorReady(): boolean {
  if (!indexedNow() || useToolStore.getState().color.index !== null) return true;
  notify(t.indexedPick);
  return false;
}

/** 筆の条件（道具・筆・色・フレーム）。描けなければ null。 */
function strokeSpec(img: ImageRuntime, tool: 'pen' | 'eraser'): StrokeSpec | null {
  const bounds = img.controller.frameBounds();
  if (!bounds) return null;
  const tools = useToolStore.getState();
  return {
    tool,
    color: tool === 'pen' ? tools.color : null,
    blend: tools.blend,
    brush: { shape: tools.shape, size: tools.size },
    pixelPerfect: tools.pixelPerfect,
    frame: useImageStore.getState().frame,
    bounds,
    indexed: indexedNow(),
  };
}

/** 筆を描き始める（⇧ なら、最後に描いた筆の終わりからの直線）。 */
export function strokeStart(p: Point, shift: boolean): boolean {
  const img = opened();
  if (!img) return false;
  const tool = useToolStore.getState().tool === 'eraser' ? 'eraser' : 'pen';
  if (tool === 'eraser' && !formatCanBeTransparent(useImageStore.getState().view?.format)) {
    notify(t.eraserUnavailable);
    return false;
  }
  if (tool === 'pen' && !paletteColorReady()) return false;
  const spec = strokeSpec(img, tool);
  if (!spec) return false;
  const start = shift ? img.controller.lineStart(spec.frame) : null;
  img.controller.beginStroke(spec, start ?? p);
  if (start) img.controller.extendStroke([p]);
  useImageStore.setState({ drawing: true });
  return true;
}

/** 点を足す。 */
export function strokeMove(points: readonly Point[]): void {
  live()?.controller.extendStroke(points);
}

/** 描き終えて送る（1 手。描く先は Rust が決める）。 */
export function strokeEnd(): void {
  const img = live();
  if (!img) return;
  const done = img.controller.endStroke();
  useImageStore.setState({ drawing: false });
  if (!done) return;
  img.ctx.push([{ step: { type: 'paint', actions: [done.action] }, tag: done.pending }]);
  if (done.action.tool === 'pen') {
    const tools = useToolStore.getState();
    tools.rememberColor(tools.color);
  }
}

/** 描いている筆をやめる（esc。送らない）。やめたら true。 */
export function strokeCancel(): boolean {
  const canceled = live()?.controller.cancelStroke() ?? false;
  if (canceled) useImageStore.setState({ drawing: false });
  return canceled;
}

/** 塗りつぶす（仮描きしない。画像の外は何もしない）。 */
export function fillAt(p: Point): void {
  const img = opened();
  if (!img || !img.controller.contains(p) || !paletteColorReady()) return;
  const tools = useToolStore.getState();
  const action = fillAction(
    {
      color: tools.color,
      tolerance: tools.tolerance,
      connectivity: tools.connectivity,
      contiguous: tools.contiguous,
      frame: useImageStore.getState().frame,
      indexed: indexedNow(),
    },
    p,
  );
  if (img.ctx.push([{ step: { type: 'paint', actions: [action] } }])) {
    tools.rememberColor(tools.color);
  }
}

/**
 * スポイト：等倍の今の値の 1 画素を色にする（クリックのときだけ。画像の外は何もしない）。応えの順は約束されないので、
 * 最後にクリックした画素の応えだけを使う。
 */
export async function sampleAt(p: Point): Promise<void> {
  const img = opened();
  const session = img?.ctx.session();
  if (!img || !session || !img.controller.contains(p)) return;
  const n = ++sampleSeq;
  try {
    const got = await sampleEdit(session, useImageStore.getState().frame, p[0], p[1]);
    if (live() !== img || n !== sampleSeq || !got) return;
    useToolStore.getState().setColor({ rgba: [...got.rgba], index: got.index });
  } catch (error) {
    if (live() !== img || n !== sampleSeq) return;
    notify(t.sampleFailed(isIpcError(error) ? error.message : String(error)));
  }
}
