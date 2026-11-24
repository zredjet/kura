/**
 * 音声エディタの表示・再生・選択の状態と操作（M13 の段 6。設計 §2.1〜§2.5、design.md §3.1 の「音声エディタ」の案文）。
 *
 * - 核の `EditorAdapter` を音声で作る（`AUDIO_ADAPTER`）。波形の面は React の外の `WaveController`、編集中の試聴は
 *   `EditorPlayer`（PCM は `PcmFeed`）。開くたびに作り、閉じたら捨てる。
 * - 画面に出すもの（表示の要約・見ている区間・カーソル・選択・道具・切り詰めの枠・ドラッグの手元の値・再生の状態・
 *   聞き比べ・ラウドネス・波形の段の状態）は Zustand（`useAudioStore`）。文書・状態・保存は核のストア。
 * - 結果 1 つ分の変更はためて、`settle` で 1 回で当てる（画像と同じ）。
 * - 選択は文書に入れない（⌘Z の外）。Space はカーソルから鳴らし、止めた所が新しいカーソル。選択があれば選択の始めから
 *   終わりまで鳴らして止まる（カーソルは動かさない。途中で止めたら止めた所）。
 * - ほかの音との排他：鳴らし始めたらプレーヤーを一時停止し、試聴・効果音を止める。プレーヤーが鳴り始めたら止める。
 * - ラウドネスは、仮でない結果が届いたときに、今の出力（`samplesId`）を計っていなければ `edit_measure` で頼む。計って
 *   いる間・古い間は前の値を出す（`loudnessFresh` が偽）。長い音声を計っている間（Measuring）は、計りが進むよう
 *   work: true で頼み続ける。一時的な誤り（Busy）は覚えずに頼み直す。
 */

import { create } from 'zustand';

import { ja } from '../../../i18n/ja';
import {
  type EditAudioNodeDto,
  type EditAudioSourceDto,
  type EditAudioView,
  type EditAudioViewport,
  type EditLoudnessStateDto,
  type EditMarkerDto,
  type EditSourcePieceDto,
  type EditSpanDto,
  type EditStepDto,
  type EditViewportDto,
  measureEdit,
  specAverageEdit,
} from '../../../ipc/edit';
import type { ItemSummaryDto } from '../../../ipc/items';
import { isIpcError } from '../../../ipc/result';
import { notify } from '../../../stores/noticeStore';
import { audioRuntime, holdAudio } from '../../asset/audioRuntime';
import { stopAudition } from '../../asset/auditionStore';
import { playerController } from '../../player/playerActions';
import { usePlayerStore } from '../../player/playerStore';
import {
  type EditorAdapter,
  type EditorAdapterContext,
  type EditorAdapterFactory,
  useEditorStore,
} from '../editorStore';
import { defaultParams } from '../opsCatalog';
import { drawSpectrum } from './drawSpectrum';
import { drawWave } from './drawWave';
import { targetNode } from './handles';
import {
  type CrossfadeRecord,
  EditorPlayer,
  PcmFeed,
  type PlacedRecord,
  type PlayRequest,
  type PlaySource,
  type SourceInfo,
} from './playback';
import {
  DEFAULT_SPECTRUM_SETTINGS,
  SpectrumController,
  type SpectrumSettings,
} from './spectrumController';
import { fromSource, toSource } from './timeline';
import { trimFrameOf } from './trim';
import { EMPTY_OVERLAY, type FetchBytes, WaveController, type WaveOverlay } from './waveController';
import type { FrameSpan, WaveWindow } from './waveMath';

const t = ja.editor.audio;

/** 道具。 */
export type AudioTool = 'select' | 'trim' | 'fade' | 'loop';
export const AUDIO_TOOLS: readonly AudioTool[] = ['select', 'trim', 'fade', 'loop'];

/** 表示の要約。 */
export interface AudioViewSummary {
  rev: number;
  rate: number;
  channels: number;
  frames: number;
  loop: EditSpanDto | null;
  markers: EditMarkerDto[];
  samplesId: string;
  valid: EditSpanDto[];
  sourceMap: EditSourcePieceDto[] | null;
  nodes: EditAudioNodeDto[];
  /**
   * 流し解きの進み（0〜1。M17 段 3）。null でなければ、この表示は頭の文書より古い（最後に公開した rev の計画で描いた
   * もの）。手元の値・枠をこの表示から戻さず、解き終えた表示を待つ。
   */
  solving: number | null;
}

/** 切り詰めの枠（T）。 */
export interface AudioTrim {
  /** 直す切り詰めのノード（null は末尾に足す）。 */
  node: number | null;
  start: number;
  end: number;
  /** 枠を動かせる長さ（その操作の入力を表示し終えるまで null）。 */
  bounds: number | null;
  /** その操作を仮に無効にして入力を表示している。 */
  previewed: boolean;
}

/** ドラッグの途中の手元の値（フェード・ループ。届くまで描く値）。 */
export interface AudioLocal {
  fadeIn?: number;
  fadeOut?: number;
  loop?: FrameSpan | null;
}

/** 舞台の面（文書に入らない。開くと毎回波形から。M17 段 5）。 */
export type AudioFace = 'wave' | 'spectrum';

/** 表示の設定を覚える所（見た目の設定なので localStorage。ライブラリのデータではない）。 */
const SPECTRUM_SETTINGS_KEY = 'kura.audioSpectrum.v1';

/** 覚えた表示の設定（読めなければ既定）。 */
export function loadSpectrumSettings(): SpectrumSettings {
  try {
    const raw = localStorage.getItem(SPECTRUM_SETTINGS_KEY);
    if (!raw) return DEFAULT_SPECTRUM_SETTINGS;
    const v = JSON.parse(raw) as Partial<SpectrumSettings>;
    const num = (x: unknown, d: number) => (typeof x === 'number' && Number.isFinite(x) ? x : d);
    const d = DEFAULT_SPECTRUM_SETTINGS;
    const ch = v.channel;
    const channel =
      ch && typeof ch === 'object' && 'type' in ch
        ? ch.type === 'index'
          ? Number.isInteger((ch as { index?: unknown }).index)
            ? ch
            : d.channel
          : ['sum', 'mid', 'side'].includes(ch.type)
            ? ch
            : d.channel
        : d.channel;
    return {
      fall: num(v.fall, d.fall),
      tilt: num(v.tilt, d.tilt),
      channel,
      range: num(v.range, d.range),
      eqScale: num(v.eqScale, d.eqScale),
    };
  } catch {
    return DEFAULT_SPECTRUM_SETTINGS;
  }
}

function saveSpectrumSettings(settings: SpectrumSettings): void {
  try {
    localStorage.setItem(SPECTRUM_SETTINGS_KEY, JSON.stringify(settings));
  } catch {
    // 覚えられなくても表示は続ける
  }
}

/** 再生の状態。 */
export interface AudioPlayState {
  status: 'idle' | 'playing';
  /** カーソルから・選択だけ・ループを試聴。 */
  mode: 'cursor' | 'selection' | 'loop';
}

export interface AudioEditorState {
  view: AudioViewSummary | null;
  /** 元の形（開いたときの `source.audio`）。 */
  source: EditAudioSourceDto | null;
  window: WaveWindow;
  /** 波形の段を作り直している・まだない。 */
  peaksPending: boolean;
  peaksBuilding: boolean;
  cursor: number;
  selection: FrameSpan | null;
  tool: AudioTool;
  trim: AudioTrim | null;
  local: AudioLocal | null;
  play: AudioPlayState;
  /** 聞き比べ：鳴らす音（編集後・元）。 */
  ab: PlaySource;
  /** 最後に分かったラウドネス（古いかは `loudnessFresh`）。 */
  loudness: EditLoudnessStateDto;
  loudnessFresh: boolean;
  /** ラウドネスの目標（LUFS。既定 −23）。 */
  loudnessTarget: number;
  /** ループの旗を離したらゼロ交差に寄せる。 */
  snapZero: boolean;
  /** 舞台の面（波形・スペクトル）。 */
  face: AudioFace;
  /** スペクトルの面の表示の設定。 */
  spectrum: SpectrumSettings;
}

const IDLE_PLAY: AudioPlayState = { status: 'idle', mode: 'cursor' };

function initialState(
  keep?: Pick<AudioEditorState, 'loudnessTarget' | 'snapZero' | 'spectrum'>,
): AudioEditorState {
  return {
    view: null,
    source: null,
    window: { start: 0, span: 1 },
    peaksPending: true,
    peaksBuilding: true,
    cursor: 0,
    selection: null,
    tool: 'select',
    trim: null,
    local: null,
    play: IDLE_PLAY,
    ab: 'edited',
    loudness: { type: 'idle' },
    loudnessFresh: false,
    loudnessTarget: keep?.loudnessTarget ?? -23,
    snapZero: keep?.snapZero ?? true,
    face: 'wave',
    spectrum: keep?.spectrum ?? loadSpectrumSettings(),
  };
}

export const useAudioStore = create<AudioEditorState>(() => initialState());

// ===== 純粋な関数 =====

/** 表示の結果の要約。 */
export function summarizeAudioView(view: EditAudioView): AudioViewSummary {
  return {
    rev: view.rev,
    rate: view.rate,
    channels: view.channels,
    frames: view.frames,
    loop: view.loop,
    markers: view.markers,
    samplesId: view.samplesId,
    valid: view.valid,
    sourceMap: view.sourceMap,
    nodes: view.nodes,
    solving: view.solving,
  };
}

/** 最後の有効なノード（その操作のもの）。 */
export function lastNodeOf(
  nodes: readonly EditAudioNodeDto[],
  op: string,
): EditAudioNodeDto | null {
  for (let i = nodes.length - 1; i >= 0; i--) {
    const n = nodes[i];
    if (n && n.op === op && n.enabled) return n;
  }
  return null;
}

/**
 * 曲線・欄に出すフェードのノード。フェードの道具ではドラッグ・欄が直すのと同じノード（選んだフェード、なければ最後の
 * フェード。外していても：直すと有効に戻る）、ほかの道具では音に効いている有効なノードだけ（対象が外れていれば最後の
 * 有効なフェード）。
 */
export function fadeNodeOf(s: AudioEditorState): EditAudioNodeDto | null {
  const view = s.view;
  if (!view) return null;
  const editor = useEditorStore.getState();
  const id = targetNode(editor.doc, editor.selected, 'audio.fade');
  const target = id === null ? null : (view.nodes.find((n) => n.id === id) ?? null);
  if (s.tool === 'fade') return target ?? lastNodeOf(view.nodes, 'audio.fade');
  return target?.enabled ? target : lastNodeOf(view.nodes, 'audio.fade');
}

/** 波形に重ねるもの（ストアの状態から）。 */
export function overlayOf(s: AudioEditorState): WaveOverlay {
  const view = s.view;
  if (!view) return EMPTY_OVERLAY;
  const total = view.frames;
  const fadeNode = fadeNodeOf(s);
  const marks = fadeNode?.marks?.type === 'fade' ? fadeNode.marks : null;
  const inEnd = s.local?.fadeIn ?? marks?.inEnd ?? 0;
  const outStart = s.local?.fadeOut ?? marks?.outStart ?? total;
  // 曲線の見かけの始まり・終わり（印のまま。null は写せないので描かない）。ノードがなければ末尾に足す（下流がない）ので
  // 0 と出力の長さ
  const inStart = fadeNode === null ? 0 : (marks?.inStart ?? null);
  const outEnd = fadeNode === null ? total : (marks?.outEnd ?? null);
  const showFade = s.tool === 'fade' || (marks !== null && (inEnd > 0 || outStart < total));
  const loop = s.local && 'loop' in s.local ? (s.local.loop ?? null) : view.loop;
  return {
    cursor: Math.min(total, Math.max(0, s.cursor)),
    selection: s.selection,
    trim: s.trim && s.trim.bounds !== null ? { start: s.trim.start, end: s.trim.end } : null,
    fade: showFade
      ? {
          inStart,
          inEnd: Math.min(total, Math.max(0, inEnd)),
          outStart: Math.min(total, Math.max(0, outStart)),
          outEnd,
          curve: curveOf(fadeNode),
          handles: s.tool === 'fade',
        }
      : null,
    loop,
    loopFlags: s.tool === 'loop' || loop !== null,
    markers: view.markers,
  };
}

/** フェードの曲線（ノードの引数。なければ直線）。 */
function curveOf(node: EditAudioNodeDto | null): string {
  if (!node) return 'linear';
  const n = useEditorStore.getState().doc.nodes[node.id];
  const p = n?.params;
  return p && typeof p === 'object' && !Array.isArray(p) && typeof p.curve === 'string'
    ? p.curve
    : 'linear';
}

// ===== React の外の持ち物 =====

interface AudioRuntime {
  ctx: EditorAdapterContext;
  controller: WaveController;
  /** スペクトルの面（M17 段 5）。 */
  spectrum: SpectrumController;
  feed: PcmFeed;
  player: EditorPlayer;
  patch: Partial<AudioEditorState>;
  /** `edit_measure` を頼んだ出力（同じものは頼み直さない）。 */
  measuredFor: string | null;
  /** 計っている（Measuring）出力。計り終えるまで work: true で頼み続ける。 */
  measuringFor: string | null;
  /** 計っていると分かったときに送り終えていた要求の番号（それより後の要求の表示だけで「計りかけを捨てた」とみる）。 */
  measuringAfter: number;
  /** 最後に送った要求の番号。 */
  lastSentSeq: number;
  lastView: EditAudioView | null;
  /** ドラッグの途中（取り消しを止める）。 */
  dragging: boolean;
  unsubscribe: () => void;
}

let current: AudioRuntime | null = null;

/** kura:// の応答が 2xx でなかった。 */
class FetchError extends Error {
  constructor(readonly status: number) {
    super(`HTTP ${status}`);
  }
}

const realFetch: FetchBytes = async (url) => {
  const res = await fetch(url);
  if (!res.ok) throw new FetchError(res.status);
  return res.arrayBuffer();
};

let fetchImpl: FetchBytes = realFetch;
let drawImpl = drawWave;

/** テストで取る口・描く関数を差し替える（null で元に戻す）。 */
export function setAudioIoForTest(io: {
  fetch?: FetchBytes | null;
  draw?: typeof drawWave | null;
}): void {
  fetchImpl = io.fetch ?? realFetch;
  drawImpl = io.draw ?? drawWave;
}

/**
 * 音声エディタの自己点検（dev。`VITE_KURA_CHECK=1` のときだけ読み込む `src/dev/waveCheck.ts`）が受けるもの：継ぎ目
 * （置いた区間）・交差フェード・往復の時間（送った手と表示）・PCM を取る時間・波形の描画。
 */
export interface AudioCheck {
  attach(parts: {
    player: EditorPlayer;
    controller: WaveController;
    session: () => string | null;
  }): void;
  sent?(seq: number, steps: readonly EditStepDto[]): void;
  shown?(seq: number, view: EditAudioView): void;
  placed?(record: PlacedRecord): void;
  crossfade?(record: CrossfadeRecord): void;
  fetched?(url: string, ms: number): void;
}

let checkHook: AudioCheck | null = null;

// 通常のビルドでは import.meta.env.VITE_KURA_CHECK が undefined に置き換わり、この枝ごと消える
if (import.meta.env.VITE_KURA_CHECK === '1') {
  void import('../../../dev/waveCheck').then(
    (m) => {
      checkHook = m.createAudioCheck();
    },
    () => undefined,
  );
}

/** 取る（自己点検があれば時間を渡す）。 */
function fetchTimed(url: string): Promise<ArrayBuffer> {
  const hook = checkHook;
  if (!hook?.fetched) return fetchImpl(url);
  const t0 = performance.now();
  return fetchImpl(url).then((buf) => {
    hook.fetched?.(url, performance.now() - t0);
    return buf;
  });
}

/** 今の波形の面の制御（開いていなければ null）。 */
export function audioController(): WaveController | null {
  return live()?.controller ?? null;
}

/** 今のスペクトルの面（開いていなければ null）。 */
export function spectrumController(): SpectrumController | null {
  return live()?.spectrum ?? null;
}

/** 面の見るイコライザ：選んだイコライザ、なければ最後のイコライザ（なければ null）。 */
export function spectrumTarget(): number | null {
  const { doc, selected } = useEditorStore.getState();
  return targetNode(doc, selected, 'audio.eq');
}

/** 面を替える（スペクトルへ替えるときは切り詰め・フェード・ループの道具を片付けて「選択」に戻す）。 */
export function setAudioFace(face: AudioFace): void {
  const s = useAudioStore.getState();
  if (s.face === face) return;
  if (face === 'spectrum') {
    if (s.trim) cancelTrim();
    useAudioStore.setState({ face, tool: 'select', local: null });
  } else {
    useAudioStore.setState({ face });
  }
}

/** スペクトルの面の表示の設定を替える（覚える）。 */
export function setSpectrumSettings(patch: Partial<SpectrumSettings>): void {
  const next = { ...useAudioStore.getState().spectrum, ...patch };
  saveSpectrumSettings(next);
  useAudioStore.setState({ spectrum: next });
}

/** 選んでよいチャンネルにする（入力のチャンネル数で選べないものは和に）。 */
export function usableChannel(
  channel: SpectrumSettings['channel'],
  channels: number,
): SpectrumSettings['channel'] {
  if (channel.type === 'index') return channel.index < channels ? channel : { type: 'sum' };
  if (channel.type === 'mid' || channel.type === 'side') {
    return channels === 2 ? channel : { type: 'sum' };
  }
  return channel;
}

/** 今の試聴（開いていなければ null。自己点検・テスト）。 */
export function audioPlayer(): EditorPlayer | null {
  return live()?.player ?? null;
}

function live(): AudioRuntime | null {
  return current?.ctx.current() ? current : null;
}

/** 開いていて手を送れる持ち物（セッションがまだなら null）。 */
export function openedAudio(): AudioRuntime | null {
  const rt = live();
  return rt?.ctx.session() ? rt : null;
}

/** 結果 1 つ分の変更に足す（結果の外なら、すぐ当てる）。 */
function patchState(rt: AudioRuntime, patch: Partial<AudioEditorState>, now: boolean): void {
  if (now) useAudioStore.setState(patch);
  else Object.assign(rt.patch, patch);
}

/** 元の形（聞き比べの「元」）。 */
function sourceInfo(s: AudioEditorState): SourceInfo | null {
  const a = s.source;
  return a ? { rate: a.rate, channels: a.channels, frames: a.frames } : null;
}

/** 出力の形。 */
function outputInfo(s: AudioEditorState): SourceInfo | null {
  const v = s.view;
  return v ? { rate: v.rate, channels: v.channels, frames: v.frames } : null;
}

/** 出力の位置を鳴らす音の位置にする（元なら sourceMap で。写せなければ null）。 */
function toPlayPos(s: AudioEditorState, ab: PlaySource, pos: number): number | null {
  if (ab === 'edited') return pos;
  return toSource(pos, s.view?.sourceMap ?? null);
}

/** 鳴らしている音の位置を出力の位置にする（写せなければ null）。 */
function toOutputPos(s: AudioEditorState, ab: PlaySource, pos: number): number | null {
  if (ab === 'edited') return pos;
  return fromSource(pos, s.view?.sourceMap ?? null);
}

/** 再生位置（出力のフレーム。鳴らしていない・写せなければ null）。波形の面が描く時刻ごとに読む。 */
export function audioPlayhead(): number | null {
  const rt = live();
  const pos = rt?.player.position() ?? null;
  if (!rt || pos === null) return null;
  const s = useAudioStore.getState();
  return toOutputPos(s, rt.player.source ?? s.ab, pos);
}

/** 裏の仕事を進める頼み（音声のビューポートの `work: true`）か。 */
function isWorkRequest(viewport: EditViewportDto): boolean {
  return viewport.type === 'audio' && viewport.work;
}

/** 音声のアダプタを作る。 */
function createAudioAdapter(_item: ItemSummaryDto, ctx: EditorAdapterContext): EditorAdapter {
  const controller = new WaveController({
    draw: (canvas, scene) => drawImpl(canvas, scene),
    fetch: fetchTimed,
    onInfo: (info) => {
      if (!ctx.current()) return;
      useAudioStore.setState({
        window: info.window,
        peaksPending: info.pending,
        peaksBuilding: info.building,
      });
    },
    onIdle: (viewport, force) => {
      if (ctx.current()) ctx.requestView(viewport, force);
    },
  });
  const feed = new PcmFeed({
    fetch: fetchTimed,
    output: controller.pcm,
    onLoaded: () => {
      if (ctx.current()) rt.player.tick();
    },
    onFailed: (source, index, count) => {
      if (ctx.current()) rt.player.chunkFailed(source, index, count);
    },
  });
  const player = new EditorPlayer({
    pool: audioRuntime().pool,
    peek: (source, i) => feed.peek(source, i),
    want: (source, i) => feed.want(source, i),
    need: (source, spans) => {
      if (source !== 'edited') return;
      // スペクトルの面の頼みも再生の区間（チャンク揃え）にしてから、再生の頼みを出す
      if (useAudioStore.getState().face === 'spectrum') {
        spectrum.setPlaybackSpans(spans);
        controller.setSpec(spectrum.specRequests());
      }
      controller.setPlaybackSpans(spans);
    },
    volume: () => usePlayerStore.getState().prefs.volume / 100,
    hold: holdAudio,
    onEnded: (pos, source) => {
      if (ctx.current()) playEnded(pos, source);
    },
    onPlaced: (record) => checkHook?.placed?.(record),
    onCrossfade: (record) => checkHook?.crossfade?.(record),
    onSwitchFailed: (reason) => {
      // 待っていた聞き比べの替えをやめた：表示を鳴っている音に戻す
      if (!ctx.current()) return;
      useAudioStore.setState({ ab: rt.player.source ?? useAudioStore.getState().ab });
      notify(reason === 'unmappable' ? t.abUnavailable : t.playFailed(t.pcmUnavailable));
    },
    onFailed: () => {
      if (ctx.current()) playFailed();
    },
  });
  const spectrum = new SpectrumController({
    fetch: fetchTimed,
    playhead: () => (player.playing ? audioPlayhead() : null),
    average: (session, request) => specAverageEdit(session, request),
    draw: (canvas, scene) => {
      const c2d = canvas.getContext('2d');
      if (c2d) drawSpectrum(c2d, scene);
    },
    onStale: () => {
      if (ctx.current()) ctx.requestView(controller.viewport(), true);
    },
    onWork: (on) => controller.setExtraWork(on, 'specAverage'),
  });
  // スペクトルの面の入力（面・設定・見るノード・止まっているときのカーソルと選択・再生）を制御に渡し、頼みの spec を替える
  let stillTimer: ReturnType<typeof setTimeout> | null = null;
  const syncSpectrum = (immediateStill: boolean) => {
    const s = useAudioStore.getState();
    const onFace = s.face === 'spectrum';
    controller.setStrip(onFace);
    if (!onFace) {
      controller.setSpec(null);
      spectrum.setPlaying(false);
      return;
    }
    const target = spectrumTarget();
    spectrum.setTarget(target);
    const node = target === null ? null : rt.lastView?.nodes.find((n) => n.id === target);
    const channels = node?.inChannels ?? s.view?.channels ?? 2;
    spectrum.setSettings({ ...s.spectrum, channel: usableChannel(s.spectrum.channel, channels) });
    spectrum.setPlaying(s.play.status === 'playing');
    const applyStill = () => {
      const now = useAudioStore.getState();
      spectrum.setStill({ cursor: now.cursor, selection: now.selection });
      controller.setSpec(spectrum.specRequests());
    };
    if (stillTimer !== null) clearTimeout(stillTimer);
    stillTimer = null;
    if (immediateStill || rt.dragging) {
      if (!rt.dragging) applyStill();
      else controller.setSpec(spectrum.specRequests());
    } else {
      // 選択・カーソルは手が止まってから（ドラッグのたびに平均を頼まない）
      stillTimer = setTimeout(() => {
        stillTimer = null;
        if (live() === rt) applyStill();
      }, 150);
      controller.setSpec(spectrum.specRequests());
    }
  };
  const offAudio = useAudioStore.subscribe((s, before) => {
    controller.setOverlay(overlayOf(s));
    if (
      s.face !== before.face ||
      s.spectrum !== before.spectrum ||
      s.play.status !== before.play.status
    ) {
      syncSpectrum(true);
    } else if (
      s.face === 'spectrum' &&
      (s.cursor !== before.cursor || s.selection !== before.selection)
    ) {
      syncSpectrum(false);
    }
  });
  // 選んだ操作・文書が変わったら、フェードの印を出し直し、面の見るノードを決め直す。イコライザをポインタで選んだ・足した
  // ときは、波形からスペクトルへ替える（↑↓ でたどって通っただけでは替えない）
  const offEditor = useEditorStore.subscribe((now, before) => {
    if (now.selected !== before.selected || now.doc !== before.doc) {
      controller.setOverlay(overlayOf(useAudioStore.getState()));
      const chosenEq =
        now.selected !== before.selected &&
        now.selected !== null &&
        now.selectedBy !== 'key' &&
        now.doc.nodes[now.selected]?.op === 'audio.eq';
      if (chosenEq && useAudioStore.getState().face === 'wave') setAudioFace('spectrum');
      else syncSpectrum(true);
    }
  });
  const unsubscribe = () => {
    offAudio();
    offEditor();
    if (stillTimer !== null) clearTimeout(stillTimer);
  };
  const rt: AudioRuntime = {
    ctx,
    controller,
    spectrum,
    feed,
    player,
    patch: {},
    measuredFor: null,
    measuringFor: null,
    measuringAfter: 0,
    lastSentSeq: 0,
    lastView: null,
    dragging: false,
    unsubscribe,
  };
  current = rt;
  checkHook?.attach({ player, controller, session: () => ctx.session() });
  return {
    begin() {
      const s = useAudioStore.getState();
      useAudioStore.setState(initialState(s));
    },
    initialViewport: (): EditAudioViewport => ({
      type: 'audio',
      visible: null,
      spans: [],
      interacting: false,
      focus: null,
      work: false,
    }),
    opened(o) {
      controller.setSession(o.session);
      spectrum.setSession(o.session);
      feed.setSession(o.session);
      const audio = o.source.audio;
      if (audio) {
        feed.setInput({ rate: audio.rate, channels: audio.channels, frames: audio.frames });
        controller.setShape({ rate: audio.rate, channels: audio.channels, frames: audio.frames });
      }
      useAudioStore.setState({ source: audio });
    },
    viewport: () => controller.viewport(),
    sent(req) {
      rt.lastSentSeq = req.seq;
      // 裏の仕事の頼みのほか（手・再生・表示の頼み）が出たら、波形の面が頼みの連鎖をやめる
      controller.noteSent(req.steps.length === 0 && isWorkRequest(req.viewport));
      checkHook?.sent?.(
        req.seq,
        req.steps.map((q) => q.step),
      );
    },
    onRejected() {
      // 切り詰める前の音声を表示する仮の値が断られたら、切り詰めをやめる
      const trim = useAudioStore.getState().trim;
      if (trim?.previewed && trim.bounds === null) {
        rt.patch.trim = null;
        rt.patch.tool = 'select';
      }
      // ドラッグの手元の値は捨てる（文書の値を出す）
      rt.patch.local = null;
    },
    applyView(view, info) {
      if (view.type !== 'audio') return;
      checkHook?.shown?.(info.seq, view);
      // 流し解きの間の表示（頭より古い。M17 段 3）：波形・PCM・再生は当てるが、手元の値・切り詰めの枠・計りの頼みは
      // この表示から決めない（解き終えた表示で決める）
      const stale = view.solving !== null;
      const prev = rt.lastView;
      rt.lastView = view;
      const s = useAudioStore.getState();
      // ラウドネス（計っている間の裏の仕事を、波形の面が頼み直しを決める前に決める）
      applyLoudness(rt, view, s.loudness, info);
      controller.applyView(view, isWorkRequest(info.viewport));
      spectrum.applyView(view);
      feed.setView(view, view.changed);
      const summary = summarizeAudioView(view);
      rt.patch.view = summary;
      // 再生：形が変わったら（切り詰めの始まり・レート・チャンネル数）元を通して位置を写して始め直し、ほかは変わった所
      // だけ替える
      if (player.playing && player.source === 'edited' && prev) {
        if (
          prev.rate !== view.rate ||
          prev.frames !== view.frames ||
          prev.channels !== view.channels
        ) {
          restartAfterReshape(rt, prev, view);
        } else {
          player.refresh(view.changed);
        }
      }
      // カーソル・選択を新しい長さに収める
      if (s.cursor > view.frames) rt.patch.cursor = view.frames;
      if (s.selection && s.selection.end > view.frames) {
        const end = view.frames;
        rt.patch.selection = s.selection.start < end ? { start: s.selection.start, end } : null;
      }
      // 切り詰めの枠：その操作の入力（仮に無効にした結果）を表示し終えたら、その表示の印と長さで枠を置く。外した表示
      // でも写せなければ、枠を出さずに引数パネルで直すよう知らせる（仮の値は結果を受け終えてから捨てる）
      const trim = s.trim;
      if (!stale && trim && trim.bounds === null && (!trim.previewed || info.provisional)) {
        const framed = trimFrameOf(trim, view.nodes, view.frames);
        if (framed) {
          rt.patch.trim = framed;
        } else {
          rt.patch.trim = null;
          rt.patch.tool = 'select';
          const node = trim.node;
          queueMicrotask(() => {
            if (live() === rt && node !== null) trimUnmappable(rt, node);
          });
        }
      }

      // ドラッグの手元の値は、仮でない結果が届いたらやめる（文書の値を出す。古い表示では残す）
      if (!stale && !info.provisional && !rt.dragging) rt.patch.local = null;
    },
    onFailed() {
      // 表示できなかった：前の表示のまま（裏の仕事の頼みの連鎖はやめる）
      controller.viewFailed();
    },
    settle() {
      const patch = rt.patch;
      rt.patch = {};
      if (Object.keys(patch).length > 0) useAudioStore.setState(patch);
      // ループの試聴は、結果で替わったループ点（と、手元の値をやめた文書の値）で鳴らす
      followLoop(rt);
    },
    beforeSteps() {
      if (useAudioStore.getState().trim) cancelTrim();
    },
    busy: () => rt.dragging,
    newParams: (desc) => defaultParams(desc, null),
    dispose(closing) {
      unsubscribe();
      player.dispose();
      feed.dispose();
      controller.dispose();
      spectrum.dispose();
      if (current === rt) current = null;
      if (closing) {
        const s = useAudioStore.getState();
        useAudioStore.setState(initialState(s));
      }
    },
  };
}

/** 音声のアダプタの作り方（`editorKinds.ts` が開く口に渡す）。 */
export const AUDIO_ADAPTER: EditorAdapterFactory = {
  kind: 'audio',
  create: createAudioAdapter,
};

// ===== ラウドネス =====

/** 一時的な誤りのあと、計るのを頼み直すまで（ms）。 */
export const MEASURE_RETRY_MS = 1000;

/**
 * 出すラウドネス：確かな値（Ready・Failed）は替え、計っている間（Measuring）・計っていない（Idle）は最後に分かった値を
 * 残す（`loudnessFresh` を偽にして tertiary で出す。前の値がなければ Measuring を置いて「計っています…」）。
 */
export function keepLastLoudness(
  known: EditLoudnessStateDto,
  incoming: EditLoudnessStateDto,
): EditLoudnessStateDto {
  if (incoming.type === 'ready' || incoming.type === 'failed') return incoming;
  if (known.type === 'ready') return known;
  return incoming.type === 'measuring' ? incoming : known;
}

/**
 * 表示の結果のラウドネスを当てる。今の出力の確かな値なら出し、計っている間（Measuring）は前の値を残して、計りが
 * 進むよう work: true で頼み続ける（長い音声は edit_view の裏の仕事の区切りでだけ進む）。どちらもなければ、仮でない
 * 結果のときに edit_measure を頼む。計っていたはずの今の出力を表示が計っていない（Idle。Rust が計りかけを捨てた）なら
 * 頼み直す。
 */
function applyLoudness(
  rt: AudioRuntime,
  view: EditAudioView,
  known: EditLoudnessStateDto,
  info: { seq: number; provisional: boolean },
): void {
  const id = view.samplesId;
  const l = view.loudness;
  if ((l.type === 'ready' || l.type === 'failed') && l.samplesId === id) {
    rt.patch.loudness = l;
    rt.patch.loudnessFresh = true;
    rt.measuredFor = id;
    rt.measuringFor = null;
  } else if (l.type === 'measuring' && l.samplesId === id) {
    rt.patch.loudness = keepLastLoudness(known, l);
    rt.patch.loudnessFresh = false;
    rt.measuredFor = id;
    rt.measuringFor = id;
    rt.measuringAfter = Math.max(rt.measuringAfter, info.seq);
  } else if ((known.type === 'ready' || known.type === 'failed') && known.samplesId === id) {
    // 計った値（edit_measure の応え）が今の出力のもの
    rt.patch.loudnessFresh = true;
    rt.measuringFor = null;
  } else {
    rt.patch.loudnessFresh = false;
    // 流し解きの間の表示は頭より古いので、その出力の計りは頼まない（解き終えた表示で頼む）
    if (!info.provisional && view.solving === null) {
      // 計っていると分かったあとに頼んだ表示が、今の出力を計っていない：計りかけを捨てたので頼み直す
      if (l.type === 'idle' && rt.measuringFor === id && info.seq > rt.measuringAfter) {
        rt.measuringFor = null;
        rt.measuredFor = null;
      }
      if (rt.measuredFor !== id) {
        rt.measuredFor = id;
        void measure(rt, id);
      }
    }
  }
  rt.controller.setExtraWork(rt.measuringFor === id);
}

/** 一時的な誤りか（Busy：一括・保存が予算を使っている。IPC の外の誤り）。覚えずにあとで頼み直す。 */
function transientMeasureError(error: unknown): boolean {
  return !isIpcError(error) || error.code === 'Busy';
}

/** 今の出力のラウドネスを計る（同じ出力のうちに届いたものだけ出す）。 */
async function measure(rt: AudioRuntime, samplesId: string): Promise<void> {
  const session = rt.ctx.session();
  if (!session) return;
  const current = () => live() === rt && useAudioStore.getState().view?.samplesId === samplesId;
  try {
    const state = await measureEdit(session);
    if (!current() || state.type === 'idle' || state.samplesId !== samplesId) return;
    if (state.type === 'measuring') {
      // 長い音声：計りは edit_view { work: true } の区切りでだけ進むので、計り終えるまで頼み続ける
      rt.measuringFor = samplesId;
      rt.measuringAfter = rt.lastSentSeq;
      rt.controller.setExtraWork(true);
      useAudioStore.setState({
        loudness: keepLastLoudness(useAudioStore.getState().loudness, state),
        loudnessFresh: false,
      });
      return;
    }
    rt.measuringFor = null;
    rt.controller.setExtraWork(false);
    useAudioStore.setState({ loudness: state, loudnessFresh: true });
  } catch (error) {
    if (!current()) return;
    if (transientMeasureError(error)) {
      // 確かな失敗として覚えない：次の仮でない結果か、少し待って頼み直す
      rt.measuredFor = null;
      setTimeout(() => {
        if (!current() || rt.measuredFor !== null) return;
        rt.measuredFor = samplesId;
        void measure(rt, samplesId);
      }, MEASURE_RETRY_MS);
      return;
    }
    const message = isIpcError(error) ? error.message : String(error);
    useAudioStore.setState({
      loudness: { type: 'failed', samplesId, message },
      loudnessFresh: true,
    });
  }
}

// ===== 再生 =====

/** ほかの音を止める（プレーヤーは一時停止・試聴と効果音は止める）。 */
function silenceOthers(): void {
  const player = usePlayerStore.getState();
  if (player.active && player.snapshot.playing) playerController().pause();
  stopAudition();
  audioRuntime().sfx.stop();
}

/** 鳴らす（出力の位置で頼む。聞き比べの「元」なら写す）。 */
function startPlayback(
  rt: AudioRuntime,
  o: { from: number; end?: number | null; loop?: FrameSpan | null; mode: AudioPlayState['mode'] },
): void {
  const s = useAudioStore.getState();
  const req = requestFor(s, s.ab, o);
  if (!req) {
    notify(t.abUnavailable);
    return;
  }
  silenceOthers();
  const out = rt.player.play(req);
  if (out === 'rateUnsupported') {
    notify(t.rateUnsupported(String(req.info.rate)));
    useAudioStore.setState({ play: IDLE_PLAY });
    return;
  }
  useAudioStore.setState({ play: { status: 'playing', mode: o.mode } });
}

/** 出力の区間の終わり（含まない位置）を鳴らす音の位置にする（元なら最後のフレームを写して 1 足す）。 */
function toPlayEnd(s: AudioEditorState, ab: PlaySource, end: number): number | null {
  if (ab === 'edited') return end;
  const last = toPlayPos(s, ab, Math.max(0, end - 1));
  return last === null ? null : last + 1;
}

/** 出力のループの区間を鳴らす音の区間にする（写せなければ null）。 */
function toPlayLoop(s: AudioEditorState, ab: PlaySource, loop: FrameSpan): FrameSpan | null {
  const a = toPlayPos(s, ab, loop.start);
  const b = toPlayEnd(s, ab, loop.end);
  return a === null || b === null ? null : { start: a, end: b };
}

/** 鳴らす頼み（元なら位置・止める所・ループを写す。写せなければ null）。 */
function requestFor(
  s: AudioEditorState,
  ab: PlaySource,
  o: { from: number; end?: number | null; loop?: FrameSpan | null },
): PlayRequest | null {
  const info = ab === 'edited' ? outputInfo(s) : sourceInfo(s);
  if (!info) return null;
  const from = toPlayPos(s, ab, o.from);
  if (from === null) return null;
  const end = o.end == null ? null : toPlayEnd(s, ab, o.end);
  let loop: FrameSpan | null = null;
  if (o.loop) {
    loop = toPlayLoop(s, ab, o.loop);
    if (!loop) return null;
  }
  return { from, end, loop, source: ab, info };
}

/**
 * ループの試聴を表示のループ点に合わせる（時刻の欄・I/O・［選択をループにする］・⌘Z・引数パネル・旗のドラッグの取り消し・
 * 断られた手で替わったとき。結果を受け終えるたびと、旗のドラッグを終えたときに呼ぶ）。旗をドラッグしている間（手元の値が
 * ある間）は手元の値で鳴らすので何もしない。ループ点がなくなった・元へ写せなければ止める。
 */
function followLoop(rt: AudioRuntime): void {
  const player = rt.player;
  if (!player.playing || !player.looping || rt.dragging) return;
  const s = useAudioStore.getState();
  if (s.local && 'loop' in s.local) return;
  // 流し解きの間の表示（頭より古い）のループ点には合わせない
  if (s.view?.solving != null) return;
  const loop = s.view?.loop ?? null;
  if (!loop || loop.end <= loop.start) {
    stopPlayback();
    return;
  }
  const mapped = toPlayLoop(s, player.source ?? 'edited', loop);
  if (!mapped) {
    stopPlayback();
    notify(t.abUnavailable);
    return;
  }
  player.setLoop(mapped);
}

/** ループの試聴を表示のループ点に合わせる（旗のドラッグを終えた・やめたとき。`followLoop`）。 */
export function followLoopAudition(): void {
  const rt = live();
  if (rt) followLoop(rt);
}

/**
 * 形が変わった（レート・長さ・チャンネル数）：元を通して位置を写し、止めてから始め直す。写せない・始め直せない（3 kHz
 * 未満など）なら止めて（始め直せなければ知らせ）、先読みの区間を外す。
 */
function restartAfterReshape(rt: AudioRuntime, prev: EditAudioView, view: EditAudioView): void {
  const pos = rt.player.position();
  const src = pos === null ? null : toSource(pos, prev.sourceMap);
  const next = src === null ? null : fromSource(src, view.sourceMap);
  const s = useAudioStore.getState();
  const halt = () => {
    rt.player.stop();
    rt.controller.setPlaybackSpans([]);
    rt.patch.play = IDLE_PLAY;
  };
  if (next === null || next >= view.frames) {
    halt();
    return;
  }
  const loop = s.play.mode === 'loop' ? (s.local?.loop ?? view.loop) : null;
  const end =
    s.play.mode === 'selection' && s.selection ? Math.min(view.frames, s.selection.end) : null;
  const out = rt.player.play({
    from: next,
    end,
    loop,
    source: 'edited',
    info: { rate: view.rate, channels: view.channels, frames: view.frames },
  });
  if (out !== 'playing') {
    // play が断るのはレートが鳴らせないときだけ（3〜384 kHz の外・文脈を作れない）
    halt();
    notify(t.rateUnsupported(String(view.rate)));
  }
}

/** 終わりまで鳴った（選択だけなら、カーソルは動かさない）。位置は鳴らしていた音 `source` で写す。 */
function playEnded(pos: number, source: PlaySource): void {
  live()?.controller.setPlaybackSpans([]);
  const s = useAudioStore.getState();
  const out = toOutputPos(s, source, pos);
  useAudioStore.setState({
    play: IDLE_PLAY,
    ...(s.play.mode === 'cursor' && out !== null ? { cursor: out } : {}),
  });
}

/** 待っているチャンクを取れないので止まった（知らせて、先読みの区間を外す）。 */
function playFailed(): void {
  live()?.controller.setPlaybackSpans([]);
  useAudioStore.setState({ play: IDLE_PLAY });
  notify(t.playFailed(t.pcmUnavailable));
}

/** 止める（止めた所を新しいカーソルにする）。 */
export function stopPlayback(): void {
  const rt = live();
  if (!rt) return;
  const playing = rt.player.playing;
  const source = rt.player.source;
  const pos = rt.player.stop();
  rt.controller.setPlaybackSpans([]);
  const s = useAudioStore.getState();
  const out = pos === null || source === null ? null : toOutputPos(s, source, pos);
  useAudioStore.setState({
    play: IDLE_PLAY,
    ...(playing && out !== null && s.play.mode !== 'loop' ? { cursor: out } : {}),
  });
}

/** Space：鳴らす・止める（選択があれば選択だけ。カーソルが終わりにあれば頭から）。 */
export function togglePlay(): void {
  const rt = openedAudio();
  if (!rt) return;
  if (rt.player.playing) {
    stopPlayback();
    return;
  }
  const s = useAudioStore.getState();
  const frames = s.view?.frames ?? 0;
  if (frames <= 0) return;
  if (s.selection && s.selection.end > s.selection.start) {
    startPlayback(rt, { from: s.selection.start, end: s.selection.end, mode: 'selection' });
    return;
  }
  startPlayback(rt, { from: s.cursor >= frames ? 0 : s.cursor, mode: 'cursor' });
}

/** L：ループを試聴する・止める（ループ点がなければ知らせる）。 */
export function toggleLoopAudition(): void {
  const rt = openedAudio();
  if (!rt) return;
  if (rt.player.playing && rt.player.looping) {
    stopPlayback();
    return;
  }
  const s = useAudioStore.getState();
  const loop = s.local && 'loop' in s.local ? (s.local.loop ?? null) : (s.view?.loop ?? null);
  if (!loop || loop.end <= loop.start) {
    notify(t.noLoop);
    return;
  }
  const from = s.cursor >= loop.start && s.cursor < loop.end ? s.cursor : loop.start;
  startPlayback(rt, { from, loop, mode: 'loop' });
}

/** 先頭へ（鳴らしていれば頭から鳴らし直す）。 */
export function playheadToStart(): void {
  const rt = live();
  if (!rt) return;
  const s = useAudioStore.getState();
  useAudioStore.setState({ cursor: 0 });
  rt.controller.follow(0);
  if (rt.player.playing && s.play.mode === 'cursor') startPlayback(rt, { from: 0, mode: 'cursor' });
}

/** B：聞き比べの元・編集後を替える（鳴らしていれば、同じ位置で替える）。 */
export function toggleAB(): void {
  const rt = openedAudio();
  if (!rt) return;
  const s = useAudioStore.getState();
  if (!s.view?.sourceMap || !s.source) {
    notify(t.abUnavailable);
    return;
  }
  const next: PlaySource = s.ab === 'edited' ? 'source' : 'edited';
  const cur = rt.player.source;
  if (!rt.player.playing || cur === null) {
    useAudioStore.setState({ ab: next });
    return;
  }
  // 鳴らしていれば、替えが成り立ったときだけ表示（ab）を替える。位置は替える時刻に鳴っている位置を写す
  const info = next === 'edited' ? outputInfo(s) : sourceInfo(s);
  const loopOut = s.play.mode === 'loop' ? (s.local?.loop ?? s.view.loop) : null;
  const endOut = s.play.mode === 'selection' ? (s.selection?.end ?? null) : null;
  const loop = loopOut ? toPlayLoop(s, next, loopOut) : null;
  const end = endOut === null ? null : toPlayEnd(s, next, endOut);
  if (!info || (loopOut && !loop) || (endOut !== null && end === null)) {
    notify(t.abUnavailable);
    return;
  }
  const result = rt.player.switchTo({
    source: next,
    info,
    end,
    loop,
    map: (pos) => {
      const now = useAudioStore.getState();
      const out = toOutputPos(now, cur, pos);
      return out === null ? null : toPlayPos(now, next, out);
    },
  });
  switch (result) {
    case 'playing':
      useAudioStore.setState({ ab: next });
      return;
    case 'unmappable':
      notify(t.abUnavailable);
      return;
    default:
      // 始め直せなかった（止まった）
      rt.controller.setPlaybackSpans([]);
      notify(t.rateUnsupported(String(info.rate)));
      useAudioStore.setState({ play: IDLE_PLAY });
  }
}

// プレーヤーが鳴り始めたら止める（鳴らし始めるときにプレーヤーを一時停止するのと対にする）
usePlayerStore.subscribe((now, before) => {
  const playing = now.active && now.snapshot.playing;
  const was = before.active && before.snapshot.playing;
  if (playing && !was && live()?.player.playing) stopPlayback();
});

// ===== カーソル・選択 =====

/** 出力の長さ。 */
function totalFrames(): number {
  return useAudioStore.getState().view?.frames ?? 0;
}

/** カーソルを置く（全体の中の整数）。 */
export function setCursor(frame: number): void {
  const f = Math.min(totalFrames(), Math.max(0, Math.round(frame)));
  if (useAudioStore.getState().cursor !== f) useAudioStore.setState({ cursor: f });
}

/** カーソルを `seconds` 秒動かす（← →）。 */
export function nudgeCursor(seconds: number): void {
  const s = useAudioStore.getState();
  const rate = s.view?.rate ?? 0;
  if (rate <= 0) return;
  setCursor(s.cursor + seconds * rate);
  live()?.controller.follow(useAudioStore.getState().cursor);
}

/** カーソルを先頭・末尾へ（Home・End）。 */
export function cursorToEdge(end: boolean): void {
  setCursor(end ? totalFrames() : 0);
  live()?.controller.follow(Math.max(0, useAudioStore.getState().cursor - 1));
}

/** 選択を替える（空・逆なら外す）。 */
export function setSelection(span: FrameSpan | null): void {
  const total = totalFrames();
  const next =
    span && span.end > span.start
      ? { start: Math.max(0, Math.round(span.start)), end: Math.min(total, Math.round(span.end)) }
      : null;
  const now = useAudioStore.getState().selection;
  if (now === next || (now && next && now.start === next.start && now.end === next.end)) return;
  useAudioStore.setState({ selection: next && next.end > next.start ? next : null });
}

/** ⌘A：すべてを選ぶ（開いていなければ false）。 */
export function selectAllAudio(): boolean {
  if (!live() || totalFrames() <= 0) return false;
  setSelection({ start: 0, end: totalFrames() });
  return true;
}

/** 選択を外す（外したら true）。 */
export function clearSelection(): boolean {
  if (!useAudioStore.getState().selection) return false;
  useAudioStore.setState({ selection: null });
  return true;
}

// ===== 表示 =====

export function audioFit(): void {
  live()?.controller.fit();
}

export function audioZoom(dir: number): void {
  live()?.controller.zoomStep(dir);
}

/** ドラッグの途中か（取り消しを止める・波形の段を直さない）。 */
export function setAudioDragging(on: boolean): void {
  const rt = live();
  if (!rt) return;
  rt.dragging = on;
  rt.controller.setInteracting(on);
}

/** ドラッグの途中の手元の値。 */
export function setAudioLocal(local: AudioLocal | null): void {
  useAudioStore.setState({ local });
}

// ===== 切り詰め（状態の片付けだけ。流れは audioTools.ts） =====

/** 切り詰めを終える（仮の値を捨て、道具を選択に戻す）。`steps` があれば続けて送る。 */
export function endTrim(
  steps: Parameters<EditorAdapterContext['send']>[0] = [],
  selectCreated = false,
): void {
  const trim = useAudioStore.getState().trim;
  if (!trim) return;
  const tool = useAudioStore.getState().tool;
  useAudioStore.setState({ trim: null, tool: tool === 'trim' ? 'select' : tool });
  const all = [...(trim.previewed ? [{ step: { type: 'cancelPreview' as const } }] : []), ...steps];
  if (all.length > 0) live()?.ctx.send(all, { selectCreated });
}

/** 切り詰めをやめる（esc・ほかの道具・ほかの手）。 */
export function cancelTrim(): void {
  endTrim();
}

/** 仮に外した表示でも写せない：仮の値を捨て、その操作を選んで引数パネルで直すよう知らせる。 */
function trimUnmappable(rt: AudioRuntime, node: number): void {
  rt.ctx.send([{ step: { type: 'cancelPreview' } }]);
  useEditorStore.setState({ selected: node, panel: 'ops' });
  notify(t.trimUseParams);
}

/** 結果の外で状態に足す（道具の流れ）。 */
export function patchAudio(patch: Partial<AudioEditorState>): void {
  const rt = live();
  if (rt) patchState(rt, patch, true);
}
