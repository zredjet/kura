/**
 * スペクトルの面とバンドの表の行のキー（M17 段 5。docs/design.md「音声エディタのスペクトルとダイナミクス」のキー）。
 *
 * 面にフォーカスがあるとき：← → で選んだバンドの周波数（1/12 オクターブ）、↑ ↓ で利得（0.5 dB。利得のない型は、カットは
 * 傾き、ノッチ・バンドパスは Q）、⇧ で大きく（×4）・⌥ で細かく（÷5）、⌫ で選んだバンドを消す、[ ] で前・次のバンド。
 * バンドを選んでいないときの ←→↑↓⌫ は何もしない（面の外のカーソル移動に落とさない）。
 * バンドの表の行にフォーカスがあるとき：↑ ↓ でバンドを移り、⌫ でバンドを消す（操作は消さない）。
 * キーの手のあとだけ、選んだバンドの読みを `aria-live` に出す。
 */

import { EQ_LIMITS } from '../../../ipc/edit';
import { eqInputRate, eqTargetParams, sendEqBands, setEqBand, useAudioStore } from './audioStore';
import { bandSummary } from './EqBandsField';
import { type EqBand, clampFreq, clampGain, clampQ, hasGain } from './eqModel';

/** 面・行の印（キーの行き先の見分け）。 */
export const FACE_ATTR = 'data-spectrum-face';
export const ROW_ATTR = 'data-eq-band-row';
/** ［＋ バンドを足す］の印（行を消したあとのフォーカスの行き先）。 */
export const ADD_ATTR = 'data-eq-band-add';

/** 1 回のキーの量（⇧ ×4・⌥ ÷5）。 */
function scale(e: { shiftKey: boolean; altKey: boolean }): number {
  return e.shiftKey ? 4 : e.altKey ? 0.2 : 1;
}

/** キーの手のあとに読み上げる（`SpectrumStage` の `aria-live` が読む）。 */
function announce(bands: readonly EqBand[], band: number | null): void {
  const b = band === null ? undefined : bands[band];
  useAudioStore.setState({ eqAnnounce: b && band !== null ? bandSummary(b, band) : '' });
}

/** カットの傾きを 1 段替える。 */
function stepSlope(slope: number, dir: number): number {
  const list = EQ_LIMITS.slopes as readonly number[];
  const i = list.indexOf(slope);
  const at = i < 0 ? list.indexOf(EQ_LIMITS.defaultSlope) : i;
  return list[Math.min(list.length - 1, Math.max(0, at + dir))] ?? slope;
}

/** バンドを `delta` だけ動かした値。 */
function nudged(b: EqBand, key: string, k: number, rate: number): { band: EqBand; field: string } {
  if (key === 'ArrowLeft' || key === 'ArrowRight') {
    const dir = key === 'ArrowRight' ? 1 : -1;
    return {
      band: { ...b, freq: clampFreq(b.freq * Math.pow(2, (dir * k) / 12), rate) },
      field: 'freq',
    };
  }
  const dir = key === 'ArrowUp' ? 1 : -1;
  if (hasGain(b.type)) {
    return { band: { ...b, gainDb: clampGain(b.gainDb + dir * 0.5 * k) }, field: 'gain' };
  }
  if (b.type === 'lowCut' || b.type === 'highCut') {
    return { band: { ...b, slope: stepSlope(b.slope, dir) }, field: 'slope' };
  }
  return { band: { ...b, q: clampQ(b.q * Math.pow(1.1, dir * k), b.type) }, field: 'q' };
}

/**
 * 面・行のキー（取ったら真、面・行の外なら null）。`audioKeys` の `down` が、⌥ を核へ回す前に呼ぶ。
 */
export function spectrumKey(e: KeyboardEvent, key: string): boolean | null {
  const target = e.target instanceof Element ? e.target : null;
  const onFace = !!target?.closest(`[${FACE_ATTR}]`);
  const row = target?.closest(`[${ROW_ATTR}]`) ?? null;
  if (!onFace && !row) return null;
  const eq = eqTargetParams();
  const bands = eq?.params.bands ?? [];
  const sel = useAudioStore.getState().eqBand;
  const rate = eqInputRate();
  if (row) {
    if (key !== 'ArrowUp' && key !== 'ArrowDown' && key !== 'Backspace' && key !== 'Delete') {
      return null;
    }
    const i = Number(row.getAttribute(ROW_ATTR));
    if (!eq || !Number.isInteger(i)) return true;
    if (key === 'Backspace' || key === 'Delete') {
      const next = bands.filter((_, j) => j !== i);
      setEqBand(null);
      sendEqBands(eq.node, next, 'set');
      announce(next, null);
      // フォーカスを残った行（なければ［＋ バンドを足す］）へ移す（消えた行と一緒にフォーカスが本文へ落ちると、次の ⌫ が
      // 核の「操作を消す」になる）
      const list = row.closest('[role="list"]');
      const to = Math.min(i, next.length - 1);
      const field = list?.parentElement ?? null;
      queueMicrotask(() => {
        const rowEl =
          to >= 0 ? list?.querySelector<HTMLElement>(`[${ROW_ATTR}="${to}"]`) : undefined;
        (rowEl ?? field?.querySelector<HTMLElement>(`[${ADD_ATTR}]`))?.focus();
      });
      return true;
    }
    const to = Math.min(bands.length - 1, Math.max(0, i + (key === 'ArrowDown' ? 1 : -1)));
    setEqBand(to);
    const list = row.closest('[role="list"]');
    const next = list?.querySelector<HTMLElement>(`[${ROW_ATTR}="${to}"]`);
    next?.focus();
    announce(bands, to);
    return true;
  }
  // 面
  if (key === '[' || key === ']') {
    if (bands.length === 0) return true;
    const to =
      sel === null
        ? key === ']'
          ? 0
          : bands.length - 1
        : Math.min(bands.length - 1, Math.max(0, sel + (key === ']' ? 1 : -1)));
    setEqBand(to);
    announce(bands, to);
    return true;
  }
  const arrows = ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'];
  if (!arrows.includes(key) && key !== 'Backspace' && key !== 'Delete') return null;
  const b = sel === null ? undefined : bands[sel];
  if (!eq || !b || sel === null) return true;
  if (key === 'Backspace' || key === 'Delete') {
    const next = bands.filter((_, j) => j !== sel);
    setEqBand(null);
    sendEqBands(eq.node, next, 'set');
    announce(next, null);
    return true;
  }
  const { band, field } = nudged(b, key, scale(e), rate);
  const next = bands.map((v, j) => (j === sel ? band : v));
  sendEqBands(eq.node, next, 'nudge', `${sel}:${field}`);
  announce(next, sel);
  return true;
}
