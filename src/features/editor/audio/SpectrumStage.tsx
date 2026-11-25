import { type PointerEvent, useEffect, useLayoutEffect, useRef, useState } from 'react';

import { ChevronDownIcon } from '../../../components/icons/ChevronDownIcon';
import { MoreIcon } from '../../../components/icons/MoreIcon';
import { type MenuAction, openMenuBelow } from '../../../components/Menu/menuStore';
import { ja } from '../../../i18n/ja';
import type { EditSpecChannelDto } from '../../../ipc/edit';
import { useEditorStore } from '../editorStore';
import styles from './Audio.module.css';
import {
  sealEq,
  setEqHover,
  setSoloHeld,
  setSpectrumSettings,
  spectrumController,
  useAudioStore,
  usableChannel,
  useEqPort,
} from './audioStore';
import { SPECTRUM_READING_ID } from './AudioCapsule';
import { dbText, freqText } from './eqModel';
import { SPECTRUM_POINTER } from './spectrumPointer';
import { noteOf } from './spectrumMath';
import { DEFAULT_SPECTRUM_COLORS, type SpectrumColors } from './drawSpectrum';
import { targetNode } from './handles';
import { SPECTRUM_CHOICES } from './spectrumController';
import { measureWaveInsets, readWaveMetrics, WaveStage } from './WaveStage';

const t = ja.editor.audio;
const ts = t.spectrum;

/** 上部からさらにあける量（CSS px）。 */
const GAP = 12;
/** 帯の 1 段が測れないときの高さ（CSS px）。 */
const DEFAULT_LANE = 52;
/** 面の中の目盛りのための余白（CSS px）：上は札、左はイコライザの dB、右はスペクトルの dB、下は周波数。 */
const AXIS = { top: 40, left: 36, right: 40, bottom: 20 };

/** スペクトルの面の色（CSS の変数を計算した色）。 */
function resolveSpectrumColors(host: HTMLElement): SpectrumColors {
  const probe = document.createElement('span');
  probe.style.display = 'none';
  host.appendChild(probe);
  const read = (token: string, fallback: string): string => {
    probe.style.color = '';
    probe.style.color = `var(${token})`;
    const value = getComputedStyle(probe).color;
    return value && value !== '' ? value : fallback;
  };
  const d = DEFAULT_SPECTRUM_COLORS;
  const colors: SpectrumColors = {
    post: read('--spec-post', d.post),
    postLine: read('--spec-post-line', d.postLine),
    pre: read('--spec-pre', d.pre),
    hold: read('--spec-hold', d.hold),
    grid: read('--stroke-1', d.grid),
    label: read('--text-tertiary', d.label),
    zero: read('--stroke-2', d.zero),
    accent: read('--accent', d.accent),
    band: read('--eq-band', d.band),
    bandFill: read('--eq-band-fill', d.bandFill),
    point: read('--text-primary', d.point),
    pointLine: read('--bg-media', d.pointLine),
  };
  probe.remove();
  return colors;
}

/** チャンネルの選び方の値（`<select>` の値）。 */
function channelValue(c: EditSpecChannelDto): string {
  return c.type === 'index' ? `index:${c.index}` : c.type;
}

function channelOf(value: string): EditSpecChannelDto {
  if (value.startsWith('index:')) return { type: 'index', index: Number(value.slice(6)) };
  return { type: value === 'mid' ? 'mid' : value === 'side' ? 'side' : 'sum' };
}

/** 選べるチャンネル（和・L・R・M・S。モノラルは和だけ、3 つ以上は和と番号）。 */
function channelOptions(channels: number): { value: string; label: string }[] {
  const out: { value: string; label: string }[] = [{ value: 'sum', label: ts.channelNames.sum }];
  if (channels < 2) return out;
  for (let i = 0; i < channels; i++) {
    out.push({ value: `index:${i}`, label: ts.channelIndex(i, channels) });
  }
  if (channels === 2) {
    out.push({ value: 'mid', label: ts.channelNames.mid });
    out.push({ value: 'side', label: ts.channelNames.side });
  }
  return out;
}

/** 面の左上の凡例（線の見本つき。押せない文字）。 */
function SpectrumLegend({ left }: { left: number }) {
  const doc = useEditorStore((s) => s.doc);
  const selected = useEditorStore((s) => s.selected);
  // 見るイコライザがなければ「出力」だけ
  const hasTarget = targetNode(doc, selected, 'audio.eq') !== null;
  return (
    <ul
      className={styles.specLegend}
      style={{ left }}
      aria-label={ts.legendLabel}
      data-spec-chip=""
    >
      <li>
        <span className={`${styles.swatch} ${styles.swatchPost}`} aria-hidden="true" />
        {hasTarget ? ts.legend.post : ts.legend.output}
      </li>
      {hasTarget && (
        <li>
          <span className={`${styles.swatch} ${styles.swatchPre}`} aria-hidden="true" />
          {ts.legend.pre}
        </li>
      )}
      <li>
        <span className={`${styles.swatch} ${styles.swatchHold}`} aria-hidden="true" />
        {ts.legend.hold}
      </li>
    </ul>
  );
}

/** 選ぶメニューの項目（今の値に印）。 */
function choices<T>(
  list: readonly T[],
  now: T,
  label: (v: T) => string,
  set: (v: T) => void,
): MenuAction[] {
  return list.map((v) => ({ label: label(v), checked: v === now, onSelect: () => set(v) }));
}

/**
 * 面の右上の表示の設定（表示だけ。文書に入らない）：今の値を出したボタン（チャンネル・イコライザの目盛り）と［…］（落ちる
 * 速さ・傾きの補正・スペクトルの範囲・最大の保持を消す）。押すと下にメニューを開く。押せるものだけ枠と地を付け、凡例と
 * 見分ける（画面での確かめ：凡例と見分けにくかった）。
 */
function SpectrumControls() {
  const settings = useAudioStore((s) => s.spectrum);
  // 選べるチャンネルは、面が出すノードの入力の数（なければ出力の数）
  const { channels } = useEqPort();
  const channel = usableChannel(settings.channel, channels);
  const options = channelOptions(channels);
  const channelLabel =
    options.find((o) => o.value === channelValue(channel))?.label ?? ts.channelNames.sum;
  return (
    <div className={styles.specControls} data-spec-chip="">
      <button
        type="button"
        className={styles.specButton}
        aria-haspopup="menu"
        aria-label={ts.valueLabel(ts.channel, channelLabel)}
        title={ts.channel}
        disabled={options.length < 2}
        onClick={(e) =>
          openMenuBelow(
            e.currentTarget,
            ts.channel,
            options.map((o) => ({
              label: o.label,
              checked: o.value === channelValue(channel),
              onSelect: () => setSpectrumSettings({ channel: channelOf(o.value) }),
            })),
          )
        }
      >
        {channelLabel}
        <ChevronDownIcon size={12} strokeWidth={2} />
      </button>
      <button
        type="button"
        className={styles.specButton}
        aria-haspopup="menu"
        aria-label={ts.valueLabel(ts.eqScale, ts.eqScaleValue(settings.eqScale))}
        title={ts.eqScale}
        onClick={(e) =>
          openMenuBelow(
            e.currentTarget,
            ts.eqScale,
            choices(SPECTRUM_CHOICES.eqScale, settings.eqScale, ts.eqScaleValue, (v) =>
              setSpectrumSettings({ eqScale: v }),
            ),
          )
        }
      >
        {ts.eqScaleValue(settings.eqScale)}
        <ChevronDownIcon size={12} strokeWidth={2} />
      </button>
      <button
        type="button"
        className={`${styles.specButton} ${styles.specMore}`}
        aria-haspopup="menu"
        aria-label={ts.settings}
        title={ts.settings}
        onClick={(e) =>
          openMenuBelow(e.currentTarget, ts.settings, [
            {
              label: ts.fall,
              submenu: choices(SPECTRUM_CHOICES.fall, settings.fall, ts.fallValue, (v) =>
                setSpectrumSettings({ fall: v }),
              ),
            },
            {
              label: ts.tilt,
              submenu: choices(SPECTRUM_CHOICES.tilt, settings.tilt, ts.tiltValue, (v) =>
                setSpectrumSettings({ tilt: v }),
              ),
            },
            {
              label: ts.range,
              submenu: choices(SPECTRUM_CHOICES.range, settings.range, ts.rangeValue, (v) =>
                setSpectrumSettings({ range: v }),
              ),
            },
            'separator',
            { label: ts.resetHold, onSelect: () => spectrumController()?.resetHold() },
          ])
        }
      >
        <MoreIcon size={14} />
      </button>
    </div>
  );
}

/**
 * スペクトルの面の舞台（M17 段 5）：上に細い波形の帯（曲の全体・選択・再生位置。`WaveStage` を帯の高さで描く）、下に面の
 * canvas（描くのは `SpectrumController`）と右上の札。面は `role="group"` で、点は飾り（読み上げとキーの道は右パネルの表）。
 */
export function SpectrumStage() {
  const rootRef = useRef<HTMLDivElement>(null);
  const faceRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const openId = useEditorStore((s) => s.openId);
  const phase = useEditorStore((s) => s.phase);
  const [stripHeight, setStripHeight] = useState(0);
  const [legendLeft, setLegendLeft] = useState<number>(AXIS.left);
  const [hover, setHover] = useState<{ x: number; y: number; lines: string[] } | null>(null);
  const pointer = useRef<number | null>(null);
  const announce = useAudioStore((s) => s.eqAnnounce);

  // 帯の高さ：上部の下から目盛りと 1 段
  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const measure = () => {
      const stage = root.parentElement;
      const top = stage?.querySelector('[data-editor-top]')?.getBoundingClientRect();
      const box = root.getBoundingClientRect();
      const metrics = readWaveMetrics(root);
      const lane =
        Number.parseFloat(getComputedStyle(root).getPropertyValue('--h-spec-strip-lane')) ||
        DEFAULT_LANE;
      const topInset = top && top.height > 0 ? Math.max(0, top.bottom - box.top + GAP) : 64;
      setStripHeight(Math.round(topInset + metrics.ruler + lane));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(root);
    return () => observer.disconnect();
  }, [openId, phase]);

  // 面の canvas をつなぐ
  useLayoutEffect(() => {
    const controller = spectrumController();
    const canvas = canvasRef.current;
    const host = faceRef.current;
    if (!controller || !canvas || !host) return;
    controller.colors = resolveSpectrumColors(host);
    controller.font = readWaveMetrics(host).font;
    controller.attach(canvas);
    const measure = () => {
      const r = host.getBoundingClientRect();
      const base = measureWaveInsets(host);
      // 凡例は面の中の左端（イコライザの dB の目盛りの右）に揃える
      setLegendLeft(base.left + AXIS.left);
      controller.setInsets({
        top: AXIS.top,
        left: base.left + AXIS.left,
        right: base.right + AXIS.right,
        bottom: base.bottom + AXIS.bottom,
      });
      const d = window.devicePixelRatio;
      controller.resize(r.width, r.height, Number.isFinite(d) && d > 0 ? d : 1);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    const capsule = host.closest('[data-editor]')?.querySelector('[data-editor-capsule]');
    if (capsule) observer.observe(capsule);
    window.addEventListener('resize', measure);
    return () => {
      observer.disconnect();
      window.removeEventListener('resize', measure);
      controller.detach();
    };
  }, [openId, phase, stripHeight]);

  // 選んだ点の上のホイールで Q（ページのスクロールを止めるため passive にしない）
  useEffect(() => {
    const host = faceRef.current;
    if (!host) return;
    const onWheel = (e: WheelEvent) => {
      const r = host.getBoundingClientRect();
      if (SPECTRUM_POINTER.wheel(e.deltaY, e.clientX - r.left, e.clientY - r.top))
        e.preventDefault();
    };
    host.addEventListener('wheel', onWheel, { passive: false });
    return () => host.removeEventListener('wheel', onWheel);
  }, []);

  /** 札の上の手（面の手にしない：札のダブルクリックでベルを足さない・札の右クリックでメニューを開かない）。 */
  const onChip = (e: { target: EventTarget }) =>
    e.target instanceof Element && e.target.closest('[data-spec-chip]') !== null;

  const local = (e: { clientX: number; clientY: number }) => {
    const r = faceRef.current?.getBoundingClientRect();
    return { x: e.clientX - (r?.left ?? 0), y: e.clientY - (r?.top ?? 0) };
  };

  /** ポインタを載せた所：バンドの曲線・山の印・吹き出し。 */
  const hoverAt = (x: number, y: number) => {
    const c = spectrumController();
    if (!c) return;
    const inner = c.inner();
    const inside = x >= inner.x && x <= inner.x + inner.w && y >= inner.y && y <= inner.y + inner.h;
    const hit = c.hitBand(x, y);
    setEqHover(hit?.band ?? null);
    const peak = !hit && inside ? c.peakNear(x) : null;
    c.setPeakMark(peak ? { x: peak.x, y: peak.y } : null);
    if (!inside) {
      setHover(null);
      return;
    }
    const f = c.freqAt(x);
    const note = noteOf(f);
    const spec = c.specDbAt(f);
    const curve = c.curveDbAt(f);
    setHover({
      x,
      y,
      lines: [
        note ? `${freqText(f)} · ${ts.hoverNote(note.name, note.cents)}` : freqText(f),
        ...(spec !== null ? [ts.hoverSpec(dbText(spec).replace(' dB', ' dBFS'))] : []),
        ...(curve !== null ? [ts.hoverCurve(dbText(curve))] : []),
      ],
    });
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 || onChip(e)) return;
    faceRef.current?.focus();
    const { x, y } = local(e);
    if (SPECTRUM_POINTER.down(e, x, y)) {
      e.currentTarget.setPointerCapture(e.pointerId);
      pointer.current = e.pointerId;
      setHover(null);
    }
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const { x, y } = local(e);
    if (pointer.current === e.pointerId) SPECTRUM_POINTER.move(e, x, y);
    else if (onChip(e)) {
      setHover(null);
      setEqHover(null);
      spectrumController()?.setPeakMark(null);
    } else hoverAt(x, y);
  };

  const finish = (e: PointerEvent<HTMLDivElement>, canceled: boolean) => {
    if (pointer.current !== e.pointerId) return;
    pointer.current = null;
    const { x, y } = local(e);
    SPECTRUM_POINTER.up(e, x, y, canceled);
  };

  return (
    <div ref={rootRef} className={styles.spectrumStage}>
      <div className={styles.specStrip} style={{ height: stripHeight }}>
        <WaveStage />
      </div>
      {/* 面はフォーカスを受けてキー（バンドを動かす・選ぶ）を取るまとまり（design.md の「読み上げとキーの道」：
          role="group" の面に、選んだバンドの読みを添える）。点は飾りで、キーと読み上げの道は右パネルの表 */}
      <div
        ref={faceRef}
        className={styles.specFace}
        style={{ top: stripHeight }}
        role="group"
        aria-label={ts.face}
        aria-describedby={SPECTRUM_READING_ID}
        data-spectrum-face=""
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- 面はキーを取るまとまり
        tabIndex={0}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={(e) => finish(e, false)}
        onPointerCancel={(e) => finish(e, true)}
        onPointerLeave={() => {
          setHover(null);
          setEqHover(null);
          spectrumController()?.setPeakMark(null);
        }}
        onDoubleClick={(e) => {
          if (onChip(e)) return;
          const { x, y } = local(e);
          SPECTRUM_POINTER.doubleClick(x, y);
        }}
        onContextMenu={(e) => {
          if (onChip(e)) return;
          e.preventDefault();
          const { x, y } = local(e);
          SPECTRUM_POINTER.contextMenu(e.clientX, e.clientY, x, y);
        }}
        onBlur={() => {
          sealEq();
          // Q を押したままフォーカスが外れた：ソロをやめる（離すキーが面に届かない）
          setSoloHeld(false);
        }}
      >
        <canvas ref={canvasRef} className={styles.wave} aria-hidden="true" />
        <span className={styles.srOnly} aria-live="polite">
          {announce}
        </span>
        {hover && (
          <div
            className={styles.specHover}
            style={{ transform: `translate(${hover.x + 12}px, ${hover.y + 12}px)` }}
            aria-hidden="true"
          >
            {hover.lines.map((line) => (
              <span key={line}>{line}</span>
            ))}
          </div>
        )}
        <SpectrumLegend left={legendLeft} />
        <SpectrumControls />
      </div>
    </div>
  );
}

/** 音声エディタの舞台：面が波形なら波形、スペクトルならスペクトルの面。 */
export function AudioStage() {
  const face = useAudioStore((s) => s.face);
  return face === 'spectrum' ? <SpectrumStage /> : <WaveStage />;
}
