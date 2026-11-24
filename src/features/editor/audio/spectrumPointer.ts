/**
 * スペクトルの面のポインタ（M17 段 5。docs/design.md「音声エディタのスペクトルとダイナミクス」のスペクトルの面）。
 *
 * - 点のドラッグで周波数と利得（⌥ の間は 1/10 の速さ。押し離ししたらその点で錨を置き直す：ADR-230）、選んだ点の左右の印の
 *   ドラッグで Q。動いてから preview を送り、離したら commitPreview（esc でやめる）。ドラッグの間は操作中（`interacting`）。
 * - 選んだ点の上のホイールで Q（トラックパッドで点の上を通っただけでは手にしない）。
 * - 空きのダブルクリックでベル（押した位置の周波数・利得、Q は既定）、点のダブルクリックでバンドを消す。
 * - 山の印からのドラッグで、その周波数にベル（Q 4。縦で利得）。イコライザがなければ、離したときに足す（P5）。
 * - 右クリックで型・チャンネル・オン／オフ。
 * 座標は面の要素の中の CSS px。
 */

import { openMenuAt, type MenuEntry } from '../../../components/Menu/menuStore';
import { ja } from '../../../i18n/ja';
import {
  addEqNode,
  eqTargetParams,
  sealEq,
  sendEqBands,
  setAudioDragging,
  setEqBand,
  spectrumController,
  useAudioStore,
} from './audioStore';
import {
  BAND_CHANNELS,
  BAND_TYPES,
  type EqBand,
  clampFreq,
  clampGain,
  clampQ,
  hasGain,
  newBell,
  qFromHandle,
} from './eqModel';

const ts = ja.editor.audio.spectrum;

/** 動いたとみなす距離（CSS px）。 */
const MOVE_PX = 3;
/** 山の印をつかめる距離（CSS px）。 */
const PEAK_PX = 8;
/** 山から作るベルの Q。 */
export const PEAK_Q = 4;
/** ホイール 1 目盛り（100 px）の Q の倍率。 */
const WHEEL_Q_STEP = 1.1;

interface Drag {
  kind: 'point' | 'qLo' | 'qHi' | 'peak';
  band: number;
  /** イコライザのノード（山から作る間にまだなければ null）。 */
  node: number | null;
  /** 錨（⌥ を押し離ししたら置き直す）：ポインタの位置と、その時のバンド。 */
  anchor: { x: number; y: number; band: EqBand };
  fine: boolean;
  moved: boolean;
  start: { x: number; y: number };
  bands: EqBand[];
}

let drag: Drag | null = null;

/** 今のポートのレート（周波数を収める）。 */
function rate(): number {
  return useAudioStore.getState().view?.rate ?? 48_000;
}

/** ドラッグで動かしたバンド（錨からの移動量。⌥ は 1/10）。 */
function dragged(d: Drag, x: number, y: number): EqBand {
  const c = spectrumController();
  const a = d.anchor;
  const k = d.fine ? 0.1 : 1;
  const b = a.band;
  if (!c) return b;
  if (d.kind === 'qLo' || d.kind === 'qHi') {
    const f = c.freqAt(a.x + (x - a.x) * k);
    return { ...b, q: qFromHandle(b, f) };
  }
  const oct = (x - a.x) * k * c.octPerPx();
  const freq = clampFreq(b.freq * Math.pow(2, oct), rate());
  if (!hasGain(b.type)) return { ...b, freq };
  const gainDb = clampGain(b.gainDb - (y - a.y) * k * c.eqDbPerPx());
  return { ...b, freq, gainDb };
}

export const SPECTRUM_POINTER = {
  /** 押した（手を始めたら真）。 */
  down(e: { altKey: boolean }, x: number, y: number): boolean {
    const c = spectrumController();
    if (!c) return false;
    sealEq();
    const target = eqTargetParams();
    const hit = c.hitBand(x, y);
    if (hit && target) {
      setEqBand(hit.band);
      const band = target.params.bands[hit.band];
      if (!band) return false;
      drag = {
        kind: hit.part === 'point' ? 'point' : hit.part,
        band: hit.band,
        node: target.node,
        anchor: { x, y, band },
        fine: e.altKey,
        moved: false,
        start: { x, y },
        bands: target.params.bands,
      };
      return true;
    }
    const peak = c.peakNear(x);
    if (peak && Math.hypot(x - peak.x, y - peak.y) <= PEAK_PX) {
      const bands = target ? [...target.params.bands] : [];
      const band = newBell(clampFreq(peak.freq, rate()), 0, PEAK_Q);
      bands.push(band);
      drag = {
        kind: 'peak',
        band: bands.length - 1,
        node: target?.node ?? null,
        anchor: { x: peak.x, y, band },
        fine: e.altKey,
        moved: false,
        start: { x, y },
        bands,
      };
      return true;
    }
    if (!e.altKey) setEqBand(null);
    return false;
  },

  move(e: { altKey: boolean }, x: number, y: number): void {
    const d = drag;
    if (!d) return;
    if (!d.moved && Math.hypot(x - d.start.x, y - d.start.y) < MOVE_PX) return;
    if (e.altKey !== d.fine) {
      // ⌥ を押し離しした：その点で錨を置き直す（飛ばない）
      const now = d.bands[d.band];
      if (now) d.anchor = { x, y, band: now };
      d.fine = e.altKey;
    }
    if (!d.moved) {
      d.moved = true;
      setAudioDragging(true);
    }
    const band = dragged(d, x, y);
    const bands = d.bands.map((b, i) => (i === d.band ? band : b));
    d.bands = bands;
    if (d.node !== null) {
      sendEqBands(d.node, bands, 'drag');
    } else {
      // イコライザがまだない：手元の曲線だけ（離したときに足す）
      useAudioStore.setState({ eqLocal: { node: null, bands, ticket: null } });
    }
  },

  up(_e: unknown, _x: number, _y: number, canceled: boolean): void {
    const d = drag;
    drag = null;
    if (!d) return;
    if (!d.moved) {
      // 山を押しただけ：何もしない（点を押しただけなら選んだだけ）
      return;
    }
    setAudioDragging(false);
    if (d.node === null) {
      useAudioStore.setState({ eqLocal: null });
      if (!canceled) addEqNode(d.bands);
      return;
    }
    sendEqBands(d.node, d.bands, canceled ? 'cancel' : 'release');
    if (!canceled && d.kind === 'peak') setEqBand(d.band);
  },

  /** ドラッグ中ならやめる（esc）。やめたら真。 */
  cancel(): boolean {
    if (!drag) return false;
    SPECTRUM_POINTER.up(null, 0, 0, true);
    return true;
  },

  /** ダブルクリック：空きにベル、点ならバンドを消す。 */
  doubleClick(x: number, y: number): void {
    const c = spectrumController();
    if (!c) return;
    const target = eqTargetParams();
    const hit = c.hitBand(x, y);
    if (hit && target) {
      const bands = target.params.bands.filter((_, i) => i !== hit.band);
      setEqBand(null);
      sendEqBands(target.node, bands, 'set');
      return;
    }
    const band = newBell(clampFreq(c.freqAt(x), rate()), clampGain(c.eqDbAt(y)));
    if (!target) {
      addEqNode([band]);
      return;
    }
    const bands = [...target.params.bands, band];
    sendEqBands(target.node, bands, 'set');
    setEqBand(bands.length - 1);
  },

  /** ホイール：選んだ点の上なら Q（手にしたら真）。 */
  wheel(dy: number, x: number, y: number): boolean {
    const c = spectrumController();
    const target = eqTargetParams();
    const sel = useAudioStore.getState().eqBand;
    if (!c || !target || sel === null) return false;
    const hit = c.hitBand(x, y);
    if (!hit || hit.band !== sel || hit.part !== 'point') return false;
    const b = target.params.bands[sel];
    if (!b || b.type === 'lowCut' || b.type === 'highCut' || b.type === 'tilt') return false;
    const q = clampQ(b.q * Math.pow(WHEEL_Q_STEP, -dy / 100), b.type);
    const bands = target.params.bands.map((v, i) => (i === sel ? { ...v, q } : v));
    sendEqBands(target.node, bands, 'nudge', `${sel}:q`);
    return true;
  },

  /** 右クリック：点なら型・チャンネル・オン／オフのメニュー（開いたら真）。 */
  contextMenu(clientX: number, clientY: number, x: number, y: number): boolean {
    const c = spectrumController();
    const target = eqTargetParams();
    if (!c || !target) return false;
    const hit = c.hitBand(x, y);
    if (!hit) return false;
    setEqBand(hit.band);
    const b = target.params.bands[hit.band];
    if (!b) return false;
    const set = (patch: Partial<EqBand>) => {
      const bands = target.params.bands.map((v, i) => (i === hit.band ? { ...v, ...patch } : v));
      sendEqBands(target.node, bands, 'set');
    };
    const entries: MenuEntry[] = [
      {
        label: ts.bandType,
        submenu: BAND_TYPES.map((type) => ({
          label: ts.bandTypes[type],
          checked: b.type === type,
          onSelect: () => set({ type, q: clampQ(b.q, type) }),
        })),
      },
      {
        label: ts.bandChannel,
        submenu: BAND_CHANNELS.map((channel) => ({
          label: ts.bandChannels[channel],
          checked: b.channel === channel,
          onSelect: () => set({ channel }),
        })),
      },
      'separator',
      {
        label: b.enabled ? ts.bandOff : ts.bandOn,
        onSelect: () => set({ enabled: !b.enabled }),
      },
    ];
    openMenuAt(clientX, clientY, ts.bandMenu, entries);
    return true;
  },

  /** ドラッグしているか。 */
  get dragging(): boolean {
    return drag !== null;
  },
};
