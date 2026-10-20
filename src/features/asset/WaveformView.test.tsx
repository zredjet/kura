import { act, render, screen, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import validText from '../../../crates/kura-engine/tests/vectors/peaks/valid.json?raw';
import { ja } from '../../i18n/ja';
import { peaksUrl } from '../../ipc/media';
import { type FrameClock, WaveformView } from './WaveformView';

interface ValidCase {
  name: string;
  frames: number;
  hex: string;
}
const cases = (JSON.parse(validText) as { cases: ValidCase[] }).cases;

/** エンジンと共有のベクタ（kura-peaks 版 1）のバイト列。 */
function vector(name: string): { buf: ArrayBuffer; frames: number } {
  const c = cases.find((x) => x.name === name);
  if (!c) throw new Error(`ベクタ ${name} がない`);
  const out = new Uint8Array(c.hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(c.hex.slice(i * 2, i * 2 + 2), 16);
  return { buf: out.buffer, frames: c.frames };
}

/** 偽の描く時刻（step で 1 回分回す）。 */
function fakeFrames() {
  const queue = new Map<number, () => void>();
  let next = 1;
  const clock: FrameClock = {
    request(cb) {
      const id = next++;
      queue.set(id, cb);
      return id;
    },
    cancel(id) {
      queue.delete(id);
    },
  };
  return {
    clock,
    pending: () => queue.size,
    step() {
      const run = [...queue.values()];
      queue.clear();
      run.forEach((cb) => cb());
    },
  };
}

const HASH = 'ef'.repeat(32);

describe('WaveformView（/peaks の波形）', () => {
  it('段のファイルを kura:// の peaks から読み、列ごとの線を描く（1 倍の画素で 272 列）', async () => {
    const { buf } = vector('default-five-levels');
    const load = vi.fn(async () => buf);
    const { container } = render(<WaveformView hash={HASH} loop={null} load={load} dpr={1} />);
    expect(load).toHaveBeenCalledWith(peaksUrl(HASH));
    const img = screen.getByRole('img', { name: ja.asset.waveform });
    await waitFor(() => expect(img).toHaveAttribute('data-loaded', 'true'));
    const svg = container.querySelector('svg');
    expect(svg?.getAttribute('viewBox')).toBe('0 0 272 48');
    const d = container.querySelector('path')?.getAttribute('d') ?? '';
    expect(d.match(/M/g)).toHaveLength(272);
    expect(container.querySelector('rect')).toBeNull();
  });

  it('装置の画素の比が 2 なら列も 2 倍（線は 1 装置画素）', async () => {
    const { buf } = vector('default-five-levels');
    const { container } = render(
      <WaveformView hash={HASH} loop={null} load={async () => buf} dpr={2} />,
    );
    await waitFor(() => expect(container.querySelector('svg')).not.toBeNull());
    expect(container.querySelector('svg')?.getAttribute('viewBox')).toBe('0 0 544 48');
  });

  it('ループの区間を矩形と両端の線で描く（位置は長さの割合）', async () => {
    const { buf, frames } = vector('default-five-levels');
    const { container } = render(
      <WaveformView hash={HASH} loop={[frames / 4, frames / 2]} load={async () => buf} dpr={1} />,
    );
    await waitFor(() => expect(container.querySelector('rect')).not.toBeNull());
    const rect = container.querySelector('rect');
    expect(Number(rect?.getAttribute('x'))).toBeCloseTo(68, 6);
    expect(Number(rect?.getAttribute('width'))).toBeCloseTo(68, 6);
    expect(container.querySelector('[data-edge="start"]')?.getAttribute('x1')).toBe('68');
    expect(container.querySelector('[data-edge="end"]')?.getAttribute('x1')).toBe('136');
  });

  it('試聴中は描く時刻ごとに位置を読み、変わったときだけ線を動かす。止めたら消す', async () => {
    const { buf, frames } = vector('default-five-levels');
    const f = fakeFrames();
    let pos: number | null = frames / 2;
    const position = vi.fn(() => pos);
    const { container, rerender } = render(
      <WaveformView
        hash={HASH}
        loop={null}
        position={position}
        load={async () => buf}
        frames={f.clock}
        dpr={1}
      />,
    );
    await waitFor(() => expect(container.querySelector('svg')).not.toBeNull());
    act(() => f.step());
    const head = () => container.querySelector('[data-playhead]');
    expect(head()?.getAttribute('x1')).toBe('136');
    pos = (frames * 3) / 4;
    act(() => f.step());
    expect(head()?.getAttribute('x1')).toBe('204');
    expect(position).toHaveBeenCalledTimes(2);
    rerender(
      <WaveformView
        hash={HASH}
        loop={null}
        position={null}
        load={async () => buf}
        frames={f.clock}
        dpr={1}
      />,
    );
    expect(head()).toBeNull();
    expect(f.pending()).toBe(0);
  });

  it('段のファイルがない（404・壊れた）ときは何も描かない', async () => {
    const load = vi.fn(async () => null);
    const { container } = render(<WaveformView hash={HASH} loop={[1, 2]} load={load} />);
    await waitFor(() => expect(load).toHaveBeenCalled());
    expect(container.querySelector('svg')).toBeNull();
    const broken = render(
      <WaveformView hash={HASH} loop={null} load={async () => new ArrayBuffer(8)} />,
    );
    await waitFor(() =>
      expect(broken.container.querySelector('[data-loaded]')).toHaveAttribute(
        'data-loaded',
        'false',
      ),
    );
    expect(broken.container.querySelector('svg')).toBeNull();
  });

  it('解析が新しくなったら読み直す（version）', async () => {
    const { buf } = vector('mono-three-levels');
    const load = vi.fn(async () => buf);
    const { rerender } = render(<WaveformView hash={HASH} version={1} loop={null} load={load} />);
    await waitFor(() => expect(load).toHaveBeenCalledTimes(1));
    rerender(<WaveformView hash={HASH} version={2} loop={null} load={load} />);
    await waitFor(() => expect(load).toHaveBeenCalledTimes(2));
  });

  it('再生の位置は ms（シアター）か秒（クイックルックの <audio>）でも受け、段のレートでフレームにする', async () => {
    // default-five-levels：48kHz・70,000 フレーム
    const { buf } = vector('default-five-levels');
    const ms = render(
      <WaveformView hash={HASH} loop={null} positionMs={729.1666} load={async () => buf} dpr={1} />,
    );
    await waitFor(() => expect(ms.container.querySelector('[data-playhead]')).not.toBeNull());
    expect(Number(ms.container.querySelector('[data-playhead]')?.getAttribute('x1'))).toBeCloseTo(
      136,
      1,
    );
    ms.unmount();
    const f = fakeFrames();
    const seconds = render(
      <WaveformView
        hash={HASH}
        loop={null}
        positionSeconds={() => 70_000 / 48_000 / 4}
        load={async () => buf}
        frames={f.clock}
        dpr={1}
      />,
    );
    await waitFor(() => expect(seconds.container.querySelector('svg')).not.toBeNull());
    act(() => f.step());
    expect(
      Number(seconds.container.querySelector('[data-playhead]')?.getAttribute('x1')),
    ).toBeCloseTo(68, 6);
  });

  it('段のファイルがなければ代わりのもの（シアターの飾りの波形）を出す', async () => {
    const load = vi.fn(async () => null);
    render(<WaveformView hash={HASH} loop={null} load={load} fallback={<span>飾りの波形</span>} />);
    expect(screen.getByText('飾りの波形')).toBeInTheDocument();
    await waitFor(() => expect(load).toHaveBeenCalled());
    expect(screen.getByText('飾りの波形')).toBeInTheDocument();
  });
});
