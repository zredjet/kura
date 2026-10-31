/**
 * 音声エディタの道具（M13 の段 6。設計 §2.6）：選択 S・切り詰め T・フェード F・ループ R と、そのハンドルの手。
 *
 * - 選択：ドラッグで選ぶ・⇧クリックで広げる・クリックはカーソル。ボタンで「選択の範囲に切り詰める」（末尾に切り詰めを
 *   足す。選択は今の出力の位置なので）・「選択をループにする」（ループ点のノードの引数にする）。
 * - 切り詰め：切り抜きと同じ流れ（`trim.ts`）。端をドラッグ・数の欄で直し、↵ で確定、esc・ほかの道具・ほかの手でやめる。
 * - フェード：段の上の角の三角をドラッグして `handle{fadeIn|fadeOut, node, preview}`、離したら `commitPreview`。
 * - ループ：目盛りの段の旗 2 本をドラッグして `handle{loop, preview}`。手元の値はストアに持ち、ループを試聴していれば
 *   次の折り返しから替える（標本は変わらないので Rust を待たない）。離したら、吸着がオンなら `edit_snap(frame,
 *   round(rate × 0.005))` で寄せた値を `preview` → `commitPreview` で送る。［ループ点を消す］は `handle{loop: null, step}`。
 * - I／O：今の道具の始め・終わりを、再生位置（鳴っていなければカーソル）にする。
 */

import { ja } from '../../../i18n/ja';
import { type EditStepDto, snapEdit } from '../../../ipc/edit';
import { notify } from '../../../stores/noticeStore';
import { pushEditorSteps, useEditorStore } from '../editorStore';
import {
  type AudioEditorState,
  type AudioTool,
  audioController,
  audioPlayer,
  audioPlayhead,
  cancelTrim,
  endTrim,
  fadeNodeOf,
  followLoopAudition,
  openedAudio,
  overlayOf,
  setAudioDragging,
  setAudioLocal,
  setCursor,
  setSelection,
  useAudioStore,
} from './audioStore';
import {
  CANCEL_DRAG,
  COMMIT_DRAG,
  dragSteps,
  fadeInHandle,
  fadeOutHandle,
  handleStep,
  inOut,
  loopHandle,
  SEAL,
  targetNode,
  trimHandle,
} from './handles';
import { beginTrim, clampTrim, confirmTrimSteps } from './trim';
import type { FrameSpan } from './waveMath';

const t = ja.editor.audio;

/** ゼロ交差を探す幅（秒）。 */
export const SNAP_SECONDS = 0.005;

function state(): AudioEditorState {
  return useAudioStore.getState();
}

function total(): number {
  return state().view?.frames ?? 0;
}

/** 手を送る（開いていなければ false）。 */
function send(steps: EditStepDto[], selectCreated = false): boolean {
  return pushEditorSteps(steps, { selectCreated });
}

/** 今の位置（鳴っていれば再生位置、なければカーソル）。 */
function nowPos(): number {
  return audioPlayhead() ?? state().cursor;
}

// ===== 道具 =====

/** 道具を選ぶ（切り詰めは枠を出す。ほかの道具を選んだら切り詰めをやめる）。 */
export function selectAudioTool(tool: AudioTool): void {
  if (tool === 'trim') {
    startTrim();
    return;
  }
  if (state().trim) cancelTrim();
  if (state().tool !== tool) useAudioStore.setState({ tool });
}

// ===== 切り詰め =====

/** 切り詰めの道具を始める（`trim.ts` の決め方）。 */
export function startTrim(): void {
  const rt = openedAudio();
  const s = state();
  const editor = useEditorStore.getState();
  if (!rt || editor.phase !== 'open' || s.trim || !s.view) return;
  const begin = beginTrim(editor.doc, editor.selected, s.view.nodes, s.view.frames);
  if (begin.kind === 'params') {
    useEditorStore.setState({ selected: begin.node, panel: 'ops' });
    notify(t.trimUseParams);
    return;
  }
  if (begin.trim.node !== null) useEditorStore.setState({ selected: begin.trim.node });
  useAudioStore.setState({ tool: 'trim', trim: begin.trim, selection: null });
  if (begin.preview) rt.ctx.send([{ step: begin.preview }]);
}

/** 切り詰めを確定する（↵）。新しく足す枠が全体のままなら何も足さない。 */
export function confirmTrim(): void {
  const trim = state().trim;
  if (!trim || trim.bounds === null) return;
  const { steps, create } = confirmTrimSteps(trim);
  endTrim(
    steps.map((step) => ({ step })),
    create,
  );
}

/** 枠を替える（ドラッグ・数の欄。入力の中の整数に収める）。 */
export function setTrimFrame(start: number, end: number): void {
  const trim = state().trim;
  if (!trim || trim.bounds === null) return;
  const next = clampTrim(start, end, trim.bounds);
  if (next.start !== trim.start || next.end !== trim.end) {
    useAudioStore.setState({ trim: { ...trim, ...next } });
  }
}

// ===== 選択 =====

/** 選択の範囲に切り詰める（末尾に切り詰めを足す）。 */
export function selectionToTrim(): void {
  const sel = state().selection;
  if (!sel || sel.end <= sel.start) return;
  if (send([handleStep(trimHandle(sel), null, 'step')], true)) {
    useAudioStore.setState({ selection: null, cursor: 0 });
  }
}

/** 選択をループにする（ループ点のノードの引数にする。なければ末尾に足す）。 */
export function selectionToLoop(): void {
  const sel = state().selection;
  if (!sel || sel.end <= sel.start) return;
  const editor = useEditorStore.getState();
  const node = targetNode(editor.doc, editor.selected, 'audio.set_loop');
  send([handleStep(loopHandle(sel), node, 'step')], node === null);
}

// ===== フェード・ループ（数の欄・ボタン） =====

/** フェードインの終わり・フェードアウトの始めを数の欄から（merge。Enter・離れたら `sealHandle`）。 */
export function setFade(
  which: 'in' | 'out',
  frame: number,
  mode: 'merge' | 'step' = 'merge',
): void {
  const editor = useEditorStore.getState();
  const node = targetNode(editor.doc, editor.selected, 'audio.fade');
  const f = Math.min(total(), Math.max(0, Math.round(frame)));
  send(
    [handleStep(which === 'in' ? fadeInHandle(f) : fadeOutHandle(f), node, mode)],
    node === null,
  );
}

/** ループの区間を数の欄から（merge）。 */
export function setLoop(span: FrameSpan, mode: 'merge' | 'step' = 'merge'): void {
  if (span.end <= span.start) return;
  const editor = useEditorStore.getState();
  const node = targetNode(editor.doc, editor.selected, 'audio.set_loop');
  send([handleStep(loopHandle(span), node, mode)], node === null);
}

/** 数の欄のまとめを閉じる（Enter・欄を離れた）。 */
export function sealHandle(): void {
  send([SEAL]);
}

/** ［ループ点を消す］。 */
export function clearLoop(): void {
  const editor = useEditorStore.getState();
  const node = targetNode(editor.doc, editor.selected, 'audio.set_loop');
  if (node === null && !state().view?.loop) return;
  send([handleStep(loopHandle(null), node, 'step')], node === null);
}

/** ゼロ交差に吸着するか。 */
export function setSnapZero(on: boolean): void {
  useAudioStore.setState({ snapZero: on });
}

// ===== I／O =====

/** I（始め）・O（終わり）：今の道具の始め・終わりを今の位置にする。 */
export function setInOut(which: 'in' | 'out'): void {
  const s = state();
  if (!s.view) return;
  const loop = overlayOf(s).loop;
  const target = inOut(s.tool, which, nowPos(), {
    total: s.view.frames,
    selection: s.selection,
    trim: s.trim && s.trim.bounds !== null ? { start: s.trim.start, end: s.trim.end } : null,
    loop,
  });
  if (!target) return;
  switch (target.kind) {
    case 'selection':
      setSelection(target.span);
      return;
    case 'trim':
      setTrimFrame(target.span.start, target.span.end);
      return;
    case 'fadeIn':
      setFade('in', target.end, 'step');
      return;
    case 'fadeOut':
      setFade('out', target.start, 'step');
      return;
    case 'loop':
      setLoop(target.span, 'step');
      return;
  }
}

// ===== ポインタ（波形の面のドラッグ） =====

/** 波形の面のポインタの扱い（x・y は面の中の CSS px。`down` が真ならドラッグをつかむ）。 */
export interface WavePointer {
  down(e: { shiftKey: boolean }, x: number, y: number): boolean;
  move(e: { shiftKey: boolean }, x: number, y: number): void;
  up(e: { shiftKey: boolean }, x: number, y: number, canceled: boolean): void;
  /** ポインタの形（`data-cursor`）。 */
  cursor(x: number, y: number): string | undefined;
}

type Drag =
  | { kind: 'select'; anchor: number; moved: boolean }
  | { kind: 'selEdge'; edge: 'start' | 'end' }
  | { kind: 'trim'; edge: 'start' | 'end' }
  | { kind: 'fade'; edge: 'in' | 'out'; node: number | null; frame: number; moved: boolean }
  | {
      kind: 'loop';
      edge: 'start' | 'end' | 'new';
      node: number | null;
      anchor: number;
      span: FrameSpan;
      moved: boolean;
    };

let drag: Drag | null = null;

/**
 * 今つかめるもの（道具ごと）。切り詰めの枠を出している間は、枠のほかのハンドル（ループの旗・フェードの三角）は
 * つかめない（表示は切り詰めを仮に外した入力の座標だが、手は切り詰めを戻した出力の座標として読まれるため）。
 */
function hitScene(s: AudioEditorState) {
  const o = overlayOf(s);
  const trimming = s.trim !== null;
  return {
    loop: !trimming && (s.tool === 'loop' || o.loop) ? o.loop : null,
    fade:
      !trimming && s.tool === 'fade' && o.fade
        ? { inEnd: o.fade.inEnd, outStart: o.fade.outStart }
        : null,
    trim: s.trim && s.trim.bounds !== null ? { start: s.trim.start, end: s.trim.end } : null,
    selection: s.tool === 'select' ? s.selection : null,
  };
}

/** 区間を 2 つの位置から（小さいほうが始め）。 */
function spanOf(a: number, b: number): FrameSpan {
  return { start: Math.min(a, b), end: Math.max(a, b) };
}

/** ループのドラッグの手元の値を出し、試聴していれば次の折り返しから替える。 */
function showLoop(span: FrameSpan, node: number | null): void {
  setAudioLocal({ ...state().local, loop: span });
  const player = audioPlayer();
  if (player?.looping) player.setLoop(span);
  if (span.end > span.start) send(dragSteps(loopHandle(span), node, 'move'));
}

/** ループのドラッグを終える（吸着がオンならゼロ交差に寄せた値で確定する）。 */
async function finishLoop(d: Extract<Drag, { kind: 'loop' }>, canceled: boolean): Promise<void> {
  const span = d.span;
  if (!d.moved) {
    setAudioLocal(null);
    return;
  }
  if (canceled || span.end <= span.start) {
    send(dragSteps(loopHandle(span), d.node, 'cancel'));
    setAudioLocal(null);
    // 試聴していれば、ドラッグの途中に替えた区間をやめて文書のループ点に戻す
    followLoopAudition();
    return;
  }
  let next = span;
  const rt = openedAudio();
  const s = state();
  const session = rt?.ctx.session();
  if (s.snapZero && session && s.view) {
    const radius = Math.max(1, Math.round(s.view.rate * SNAP_SECONDS));
    const snap = async (f: number) => {
      try {
        const got = await snapEdit(session, f, radius);
        return got.found ? got.frame : f;
      } catch {
        return f;
      }
    };
    const start = d.edge === 'end' ? span.start : await snap(span.start);
    const end = d.edge === 'start' ? span.end : await snap(span.end);
    if (end > start) next = { start, end };
  }
  if (openedAudio() !== rt) return;
  setAudioLocal({ ...state().local, loop: next });
  audioPlayer()?.setLoop(next);
  send([handleStep(loopHandle(next), d.node, 'preview'), COMMIT_DRAG], d.node === null);
}

/** 波形の面のポインタ（道具ごとに振り分ける）。 */
export const AUDIO_POINTER: WavePointer = {
  down(e, x, y) {
    const c = audioController();
    const s = state();
    if (!c || !s.view || !openedAudio()) return false;
    const hit = c.hit(x, y, hitScene(s));
    const f = c.frameAt(x);
    const editor = useEditorStore.getState();
    switch (hit.kind) {
      case 'loop': {
        const loop = overlayOf(s).loop;
        if (!loop) return false;
        const node = targetNode(editor.doc, editor.selected, 'audio.set_loop');
        drag = { kind: 'loop', edge: hit.edge, node, anchor: f, span: loop, moved: false };
        setAudioDragging(true);
        return true;
      }
      case 'fade': {
        const node = targetNode(editor.doc, editor.selected, 'audio.fade');
        drag = { kind: 'fade', edge: hit.edge, node, frame: f, moved: false };
        setAudioDragging(true);
        return true;
      }
      case 'trim':
        drag = { kind: 'trim', edge: hit.edge };
        return true;
      case 'selection':
        drag = { kind: 'selEdge', edge: hit.edge };
        return true;
      case 'ruler':
        if (s.tool === 'loop') {
          const node = targetNode(editor.doc, editor.selected, 'audio.set_loop');
          drag = {
            kind: 'loop',
            edge: 'new',
            node,
            anchor: f,
            span: { start: f, end: f },
            moved: false,
          };
          setAudioDragging(true);
          return true;
        }
        return false;
      case 'lanes':
        if (s.tool === 'select') {
          if (e.shiftKey) {
            // ⇧クリック：選択（なければカーソル）から広げる
            const base = s.selection ?? { start: s.cursor, end: s.cursor };
            const anchor =
              Math.abs(f - base.start) > Math.abs(f - base.end) ? base.start : base.end;
            drag = { kind: 'select', anchor, moved: true };
            setSelection(spanOf(anchor, f));
          } else {
            drag = { kind: 'select', anchor: f, moved: false };
          }
          return true;
        }
        return false;
      default:
        return false;
    }
  },
  move(_e, x) {
    const c = audioController();
    const d = drag;
    if (!c || !d) return;
    const f = c.frameAt(x);
    const s = state();
    switch (d.kind) {
      case 'select':
        if (f !== d.anchor) d.moved = true;
        if (d.moved) setSelection(spanOf(d.anchor, f));
        return;
      case 'selEdge': {
        const sel = s.selection;
        if (!sel) return;
        const other = d.edge === 'start' ? sel.end : sel.start;
        setSelection(spanOf(other, f));
        if ((d.edge === 'start' && f > other) || (d.edge === 'end' && f < other)) {
          d.edge = d.edge === 'start' ? 'end' : 'start';
        }
        return;
      }
      case 'trim': {
        const trim = s.trim;
        if (!trim) return;
        if (d.edge === 'start') setTrimFrame(Math.min(f, trim.end - 1), trim.end);
        else setTrimFrame(trim.start, Math.max(f, trim.start + 1));
        return;
      }
      case 'fade': {
        d.frame = f;
        d.moved = true;
        setAudioLocal({ ...s.local, ...(d.edge === 'in' ? { fadeIn: f } : { fadeOut: f }) });
        send(dragSteps(d.edge === 'in' ? fadeInHandle(f) : fadeOutHandle(f), d.node, 'move'));
        return;
      }
      case 'loop': {
        let span: FrameSpan;
        if (d.edge === 'new') span = spanOf(d.anchor, f);
        else if (d.edge === 'start') span = { start: Math.min(f, d.span.end - 1), end: d.span.end };
        else span = { start: d.span.start, end: Math.max(f, d.span.start + 1) };
        d.span = span;
        d.moved = true;
        showLoop(span, d.node);
        return;
      }
    }
  },
  up(_e, x, _y, canceled) {
    const c = audioController();
    const d = drag;
    drag = null;
    if (!c || !d) return;
    const f = c.frameAt(x);
    switch (d.kind) {
      case 'select':
        if (!d.moved) {
          setSelection(null);
          setCursor(f);
        }
        return;
      case 'selEdge':
      case 'trim':
        return;
      case 'fade': {
        setAudioDragging(false);
        if (!d.moved) {
          setAudioLocal(null);
          return;
        }
        if (canceled) {
          send([CANCEL_DRAG]);
          setAudioLocal(null);
        } else {
          send([COMMIT_DRAG], d.node === null);
        }
        return;
      }
      case 'loop':
        setAudioDragging(false);
        void finishLoop(d, canceled);
        return;
    }
  },
  cursor(x, y) {
    const c = audioController();
    const s = state();
    if (!c || !s.view) return undefined;
    const hit = c.hit(x, y, hitScene(s));
    if (
      hit.kind === 'loop' ||
      hit.kind === 'fade' ||
      hit.kind === 'trim' ||
      hit.kind === 'selection'
    ) {
      return 'ew';
    }
    if (hit.kind === 'overview') return 'grab';
    if (hit.kind === 'lanes' && s.tool === 'select') return 'text';
    return undefined;
  },
};

/** ドラッグをやめる（閉じた・開き直した）。 */
export function resetAudioDrag(): void {
  drag = null;
}

/** フェードの対象の長さ（右パネル・カプセルの欄）。 */
export function fadeLengths(): { fadeIn: number; fadeOut: number } {
  const s = state();
  const view = s.view;
  if (!view) return { fadeIn: 0, fadeOut: 0 };
  const node = fadeNodeOf(s);
  const marks = node?.marks?.type === 'fade' ? node.marks : null;
  const inEnd = s.local?.fadeIn ?? marks?.inEnd ?? 0;
  const outStart = s.local?.fadeOut ?? marks?.outStart ?? view.frames;
  return { fadeIn: Math.max(0, inEnd), fadeOut: Math.max(0, view.frames - outStart) };
}
