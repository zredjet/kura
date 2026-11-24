import { useLayoutEffect, useRef, useState } from 'react';

import { Select } from '../../../components/Select/Select';
import { ja } from '../../../i18n/ja';
import type { EditSpecChannelDto } from '../../../ipc/edit';
import { useEditorStore } from '../editorStore';
import styles from './Audio.module.css';
import {
  setSpectrumSettings,
  spectrumController,
  useAudioStore,
  usableChannel,
} from './audioStore';
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

/** 面の右上の札：線の見本つきの凡例と、表示の設定（表示だけ。文書に入らない）。 */
function SpectrumChip() {
  const settings = useAudioStore((s) => s.spectrum);
  const channels = useAudioStore((s) => s.view?.channels ?? 2);
  const doc = useEditorStore((s) => s.doc);
  const selected = useEditorStore((s) => s.selected);
  const [open, setOpen] = useState(false);
  // 見るイコライザがなければ「出力」だけ
  const hasTarget = targetNode(doc, selected, 'audio.eq') !== null;
  const channel = usableChannel(settings.channel, channels);
  return (
    <div className={styles.specChip}>
      <ul className={styles.specLegend} aria-label={ts.settings}>
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
      <button
        type="button"
        className={styles.specSettingsButton}
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {ts.settings}
      </button>
      {open && (
        <div className={styles.specSettings} role="group" aria-label={ts.settings}>
          <label>
            <span>{ts.fall}</span>
            <Select
              value={String(settings.fall)}
              options={SPECTRUM_CHOICES.fall.map((v) => ({
                value: String(v),
                label: ts.fallValue(v),
              }))}
              onChange={(v) => setSpectrumSettings({ fall: Number(v) })}
            />
          </label>
          <label>
            <span>{ts.tilt}</span>
            <Select
              value={String(settings.tilt)}
              options={SPECTRUM_CHOICES.tilt.map((v) => ({
                value: String(v),
                label: ts.tiltValue(v),
              }))}
              onChange={(v) => setSpectrumSettings({ tilt: Number(v) })}
            />
          </label>
          <label>
            <span>{ts.channel}</span>
            <Select
              value={channelValue(channel)}
              options={channelOptions(channels)}
              onChange={(v) => setSpectrumSettings({ channel: channelOf(v) })}
            />
          </label>
          <label>
            <span>{ts.range}</span>
            <Select
              value={String(settings.range)}
              options={SPECTRUM_CHOICES.range.map((v) => ({
                value: String(v),
                label: ts.rangeValue(v),
              }))}
              onChange={(v) => setSpectrumSettings({ range: Number(v) })}
            />
          </label>
          <label>
            <span>{ts.eqScale}</span>
            <Select
              value={String(settings.eqScale)}
              options={SPECTRUM_CHOICES.eqScale.map((v) => ({
                value: String(v),
                label: ts.eqScaleValue(v),
              }))}
              onChange={(v) => setSpectrumSettings({ eqScale: Number(v) })}
            />
          </label>
          <button
            type="button"
            className={styles.specReset}
            onClick={() => spectrumController()?.resetHold()}
          >
            {ts.resetHold}
          </button>
        </div>
      )}
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
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- 面はキーを取るまとまり
        tabIndex={0}
      >
        <canvas ref={canvasRef} className={styles.wave} aria-hidden="true" />
        <SpectrumChip />
      </div>
    </div>
  );
}

/** 音声エディタの舞台：面が波形なら波形、スペクトルならスペクトルの面。 */
export function AudioStage() {
  const face = useAudioStore((s) => s.face);
  return face === 'spectrum' ? <SpectrumStage /> : <WaveStage />;
}
