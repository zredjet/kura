import { act, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../../i18n/ja';
import type { EditEqCurveDto } from '../../../ipc/edit';
import { useEditorStore } from '../editorStore';
import { useAudioStore } from './audioStore';
import { EQ_PHASE_NOTE_WAIT_MS, EqPhaseNote, ringingTime } from './EqPhaseNote';

const curves: { params: unknown; points: number }[] = [];
let fir: EditEqCurveDto['fir'] = { taps: 32_769, delayFrames: 16_384, capped: false };
/** 真なら頼みを断る。 */
let refuse = false;
/** 頼みの応えを返すまでの待ち（ms。偽の時計）。 */
let delay = 0;

vi.mock('../../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../ipc/edit')>()),
  eqCurve: vi.fn(async (params: unknown, _rate: number, _channels: number, points: number) => {
    curves.push({ params, points });
    const answer = fir;
    const rejected = refuse;
    if (delay > 0) await new Promise((r) => setTimeout(r, delay));
    if (rejected) throw new Error('断った');
    return { freqs: [20, 24_000], total: [0, 0], bands: [], fir: answer } satisfies EditEqCurveDto;
  }),
}));

const t = ja.editor.audio.eqPhase;

function setEq(phase: 'minimum' | 'linear', q = 2) {
  useEditorStore.setState({
    doc: {
      ...useEditorStore.getState().doc,
      nodes: {
        5: {
          id: 5,
          op: 'audio.eq',
          v: 1,
          enabled: true,
          params: {
            bands: [
              { type: 'bell', freq: 100, gainDb: 3, q, slope: 12, channel: 'all', enabled: true },
            ],
            phase,
            gainDb: 0,
          },
          log: null,
        },
      },
    } as never,
  });
}

describe('位相の欄の下の一言（M17 段 7）', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    curves.length = 0;
    fir = { taps: 32_769, delayFrames: 16_384, capped: false };
    refuse = false;
    delay = 0;
    useAudioStore.setState({
      view: { nodes: [{ id: 5, inRate: 48_000, inChannels: 2 }] } as never,
    });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('長さの書き方：0.1 秒以上は秒、短ければミリ秒', () => {
    expect(ringingTime(16_384, 48_000)).toBe(t.seconds('0.34'));
    expect(ringingTime(512, 48_000)).toBe(t.millis(11));
    expect(ringingTime(1, 48_000)).toBe(t.millis(1));
  });

  it('直線位相：止まって 150 ms で点 2 つの曲線を頼み、前のリンギングの秒を出す。上限に届けばその一言も', async () => {
    setEq('linear');
    render(<EqPhaseNote node={5} op="audio.eq" param="phase" />);
    expect(curves).toHaveLength(0);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EQ_PHASE_NOTE_WAIT_MS);
    });
    expect(curves.map((c) => c.points)).toEqual([2]);
    expect(screen.getByText(t.ringing(t.seconds('0.34')))).toBeInTheDocument();
    expect(screen.queryByText(t.capped)).toBeNull();
    // 引数が変わった：次が届くまで前の一言を残し、届いたら替える
    fir = { taps: 65_537, delayFrames: 32_768, capped: true };
    act(() => setEq('linear', 10));
    expect(screen.getByText(t.ringing(t.seconds('0.34')))).toBeInTheDocument();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EQ_PHASE_NOTE_WAIT_MS);
    });
    expect(screen.getByText(t.ringing(t.seconds('0.68')))).toBeInTheDocument();
    expect(screen.getByText(t.capped)).toBeInTheDocument();
  });

  it('断られたら一言を消す。引数が替わったあとに届いた古い応えは使わない。レートが分からない間は出さない', async () => {
    setEq('linear');
    render(<EqPhaseNote node={5} op="audio.eq" param="phase" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EQ_PHASE_NOTE_WAIT_MS);
    });
    expect(screen.getByText(t.ringing(t.seconds('0.34')))).toBeInTheDocument();
    refuse = true;
    act(() => setEq('linear', 3));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EQ_PHASE_NOTE_WAIT_MS);
    });
    expect(screen.queryByText(/前のリンギング/)).toBeNull();
    // 遅い応え（0.68 秒）が届く前に引数が替わる：その応えは使わず、次の応え（0.17 秒）を出す
    refuse = false;
    delay = 1_000;
    fir = { taps: 65_537, delayFrames: 32_768, capped: false };
    act(() => setEq('linear', 4));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EQ_PHASE_NOTE_WAIT_MS);
    });
    delay = 0;
    fir = { taps: 16_385, delayFrames: 8_192, capped: false };
    act(() => setEq('linear', 5));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000 + EQ_PHASE_NOTE_WAIT_MS);
    });
    expect(screen.getByText(t.ringing(t.seconds('0.17')))).toBeInTheDocument();
    expect(screen.queryByText(t.ringing(t.seconds('0.68')))).toBeNull();
    // 表示の前（レートが分からない）
    act(() => useAudioStore.setState({ view: null }));
    expect(screen.queryByText(/前のリンギング/)).toBeNull();
  });

  it('最小位相・ほかの欄・ほかの操作では出さず、頼まない', async () => {
    setEq('minimum');
    const { rerender } = render(<EqPhaseNote node={5} op="audio.eq" param="phase" />);
    setEq('linear');
    rerender(<EqPhaseNote node={5} op="audio.eq" param="gainDb" />);
    rerender(<EqPhaseNote node={5} op="audio.gain" param="phase" />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(EQ_PHASE_NOTE_WAIT_MS * 2);
    });
    expect(curves).toHaveLength(0);
    expect(screen.queryByText(/前のリンギング/)).toBeNull();
  });
});
