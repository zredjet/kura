import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../../i18n/ja';
import type { EditNodeDto } from '../../../ipc/edit';
import type { ItemSummaryDto } from '../../../ipc/items';
import { Editor } from '../Editor';
import { useEditorStore } from '../editorStore';
import { OpsPanel } from '../OpsPanel';
import { summarizeAudioView, useAudioStore } from './audioStore';
import { targetDiff } from './LoudnessSection';
import { audioView } from './testAudioView';

vi.mock('../../library/inspector/ItemInspector', () => ({
  ItemInspector: ({ id }: { id: number }) => <p>インスペクタ {id}</p>,
}));

vi.mock('../../../ipc/items', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../ipc/items')>()),
  getItemDetail: vi.fn(async () => ({ originalName: 'loop.wav' })),
}));

const t = ja.editor;
const a = ja.editor.audio;
const TOTAL = 96_000;

const ITEM = {
  id: 9,
  hash: 'cd'.repeat(32),
  ext: 'wav',
  kind: 'audio',
  title: 'loop.wav',
  thumbState: 'none',
  thumbRev: 0,
  pixelArt: false,
} as unknown as ItemSummaryDto;

function node(id: number, op: string, params: Record<string, unknown> = {}): EditNodeDto {
  return { id, op, v: 1, enabled: true, params: params as EditNodeDto['params'], log: null };
}

function openState(nodes: EditNodeDto[] = [], outputs: unknown[] = []) {
  useEditorStore.setState({
    phase: 'open',
    kind: 'audio',
    item: ITEM,
    session: 'ab01',
    source: { kind: 'audio', width: 0, height: 0, frames: null, format: null, audio: null },
    status: {
      rev: 2,
      head: 2,
      provisional: false,
      canUndo: true,
      canRedo: false,
      dirty: true,
      saving: null,
      needsReopen: false,
    },
    doc: {
      order: nodes.map((n) => n.id),
      nodes: Object.fromEntries(nodes.map((n) => [n.id, n])),
      outputs: outputs as never,
    },
    failure: null,
    panel: 'ops',
    selected: null,
  });
  const v = audioView({
    frames: TOTAL,
    loop: { start: 1000, end: 2000 },
    markers: [{ pos: 10, name: 'a' }],
    nodes: nodes.map((n) => ({
      id: n.id,
      op: n.op,
      enabled: true,
      inRate: 48_000,
      inFrames: TOTAL,
      inChannels: 2,
      marks: null,
    })),
  });
  useAudioStore.setState({
    view: summarizeAudioView(v),
    source: {
      rate: 48_000,
      channels: 2,
      frames: TOTAL,
      bits: 16,
      float: false,
      loop: null,
      decoder: 'wav-kura-dec/1',
      lazy: true,
    },
    window: { start: 0, span: 48_000 * 10 },
    cursor: 48_000,
    selection: null,
    tool: 'select',
    trim: null,
    local: null,
    peaksPending: false,
    peaksBuilding: false,
    loudness: { type: 'idle' },
    loudnessFresh: false,
    loudnessTarget: -23,
  });
}

beforeEach(() => {
  useAudioStore.setState({ tool: 'select', trim: null, local: null, face: 'wave' });
});

afterEach(() => {
  useEditorStore.setState({
    phase: 'closed',
    kind: null,
    item: null,
    originalFormat: null,
    failure: null,
    panel: 'ops',
  });
});

describe('音声エディタの画面', () => {
  it('上部に名前と「● 未保存」の印・「48 kHz · 16 ビット · ステレオ · 0:02.000 · WAV」・道具、下に再生と選択の欄、左下にカーソル、右下に表示の幅', () => {
    openState();
    render(<Editor />);
    expect(screen.getByRole('region', { name: a.label })).toBeInTheDocument();
    expect(screen.getByText('loop.wav')).toBeInTheDocument();
    expect(screen.getByText(t.unsaved)).toBeInTheDocument();
    expect(screen.getByText('48 kHz · 16 ビット · ステレオ · 0:02.000 · WAV')).toBeInTheDocument();
    const select = screen.getByRole('button', { name: t.withKey(a.toolNames.select, 'S') });
    expect(select).toHaveAttribute('aria-pressed', 'true');
    // 下のカプセル：再生の操作と選択の欄
    const capsule = screen.getByRole('toolbar', { name: t.toolSettings });
    expect(within(capsule).getByRole('button', { name: a.play })).toBeInTheDocument();
    expect(within(capsule).getByRole('button', { name: a.toStart })).toBeInTheDocument();
    expect(within(capsule).getByRole('button', { name: a.loopAudition })).toBeEnabled();
    expect(within(capsule).getByRole('group', { name: a.abLabel })).toBeInTheDocument();
    expect(within(capsule).getByRole('button', { name: a.selectionToTrim })).toBeDisabled();
    expect(within(capsule).getByText(a.position('0:01.000', '0:02.000'))).toBeInTheDocument();
    // 左下・右下
    expect(screen.getByText(a.cursorAt('0:01.000', '48,000'))).toBeInTheDocument();
    expect(screen.getByText(a.width('0:10.000'))).toBeInTheDocument();
  });

  it('下のカプセルは 2 段：上の段に再生の操作と道具の切り替え、下の段に今の道具の欄（ボタンは短い文、読み上げは長い文）', () => {
    openState();
    render(<Editor />);
    const capsule = screen.getByRole('toolbar', { name: t.toolSettings });
    const [top, fields] = Array.from(capsule.children);
    if (!top || !fields) throw new Error('2 段になっていない');
    expect(
      within(top as HTMLElement).getByRole('group', { name: a.transport }),
    ).toBeInTheDocument();
    expect(within(top as HTMLElement).getByRole('group', { name: t.tools })).toBeInTheDocument();
    // 道具の切り替えは上部ではなくカプセルに 1 つだけ
    expect(screen.getAllByRole('group', { name: t.tools })).toHaveLength(1);
    const toTrim = within(fields as HTMLElement).getByRole('button', { name: a.selectionToTrim });
    expect(toTrim).toHaveTextContent(a.selectionToTrimShort);
    expect(toTrim).toHaveAttribute('title', a.selectionToTrim);
    expect(
      within(fields as HTMLElement).getByRole('button', { name: a.selectionToLoop }),
    ).toHaveTextContent(a.selectionToLoopShort);
  });

  it('道具を替えると、カプセルの欄が替わる（切り詰め・フェード・ループ）', () => {
    openState();
    render(<Editor />);
    fireEvent.click(screen.getByRole('button', { name: t.withKey(a.toolNames.loop, 'R') }));
    expect(useAudioStore.getState().tool).toBe('loop');
    expect(screen.getByRole('button', { name: a.loopClear })).toBeEnabled();
    expect(screen.getByRole('switch', { name: a.snapZero })).toBeChecked();
    expect(screen.getByDisplayValue('0:00.021')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.withKey(a.toolNames.fade, 'F') }));
    expect(screen.getByLabelText(a.fadeIn)).toBeInTheDocument();
    expect(screen.getByRole('group', { name: a.curve })).toBeInTheDocument();
    act(() => {
      useAudioStore.setState({
        tool: 'trim',
        trim: { node: null, start: 0, end: TOTAL, bounds: TOTAL, previewed: false },
      });
    });
    expect(screen.getByRole('button', { name: a.trimConfirm })).toBeEnabled();
    expect(screen.getByText(a.trimHint)).toBeInTheDocument();
  });

  it('波形の段を作り直している間は「波形を更新しています…」、選択があれば選択の時刻', () => {
    openState();
    useAudioStore.setState({ peaksPending: true, selection: { start: 0, end: 24_000 } });
    render(<Editor />);
    expect(screen.getByText(a.peaksPending)).toHaveAttribute('aria-live', 'polite');
    expect(screen.getByText(a.selectionAt('0:00.000', '0:00.500', '0:00.500'))).toBeInTheDocument();
  });

  it('流し解きの間は「変更を反映しています… n%」を波形の札より先に出す（M17 段 3）', () => {
    openState();
    const view = useAudioStore.getState().view;
    useAudioStore.setState({
      peaksPending: true,
      view: view ? { ...view, solving: 0.426 } : null,
    });
    render(<Editor />);
    expect(screen.getByText(a.solving('42%'))).toHaveAttribute('aria-live', 'polite');
    expect(screen.queryByText(a.peaksPending)).toBeNull();
  });
  it('右パネル：ラウドネス（値・目標との差・そろえる）・出力の形式・失うもの', () => {
    openState([], [{ port: { node: 0, port: 0 }, format: { type: 'to', format: 'mp3' } }]);
    useAudioStore.setState({
      loudness: {
        type: 'ready',
        samplesId: '00112233aabbccdd',
        loudness: {
          loudness: -14.2,
          loudnessReason: null,
          loudnessRange: 6.1,
          truePeak: -1,
          samplePeak: -1.2,
        },
      },
      loudnessFresh: true,
    });
    render(<Editor />);
    expect(screen.getByText('−14.2 LUFS（LRA 6.1 LU） · −1.0 dBTP')).toBeInTheDocument();
    expect(screen.getByText(a.diff('+8.8'))).toBeInTheDocument();
    expect(screen.getByRole('button', { name: a.alignToTarget })).toBeEnabled();
    expect(screen.getByText(a.lossLoop('MP3'))).toBeInTheDocument();
    expect(screen.getByText(a.lossMarkers('MP3'))).toBeInTheDocument();
    expect(screen.getByText(a.lossLossy('MP3'))).toBeInTheDocument();
  });

  it('計る前（開いてすぐ計りを頼むので、まだ値が届いていない）も「計っています…」で、そろえるボタンは押せない', () => {
    openState();
    render(<Editor />);
    const section = screen.getByRole('region', { name: a.loudnessTitle });
    expect(within(section).getByText(a.measuring)).toHaveAttribute('data-stale', 'true');
    expect(within(section).getByRole('button', { name: a.alignToTarget })).toBeDisabled();
  });

  it.each(['silent', 'tooShort'])(
    '計り終えても積分ラウドネスを計れなければ（%s）、理由を出して［この目標にそろえる］は押せない',
    (reason) => {
      openState();
      useAudioStore.setState({
        loudness: {
          type: 'ready',
          samplesId: '00112233aabbccdd',
          loudness: {
            loudness: null,
            loudnessReason: reason,
            loudnessRange: null,
            truePeak: null,
            samplePeak: null,
          },
        },
        loudnessFresh: true,
      });
      render(<Editor />);
      const text = ja.asset.loudnessReasons[reason];
      expect(text).toBeDefined();
      expect(screen.getByText(text ?? '')).toBeInTheDocument();
      expect(screen.getByRole('button', { name: a.alignToTarget })).toBeDisabled();
    },
  );

  it('「元と同じ」と失うものは元のファイルの形式（Rust の originalFormat）で決める（今の拡張子ではない）', () => {
    // MP3 の元を WAV で保存したあと（今の拡張子は wav）、出力は「元と同じ」
    openState();
    useEditorStore.setState({ originalFormat: 'mp3' });
    render(<Editor />);
    const select = screen.getByRole('combobox', { name: t.output });
    expect(select).toHaveValue('keep');
    expect(within(select).getByText(t.outputKeep('MP3'))).toBeInTheDocument();
    expect(screen.getByText(a.lossLossy('MP3'))).toBeInTheDocument();
    // 元のビット数は、元が非可逆なら出さない
    expect(screen.queryByText(/16 ビット/)).toBeNull();
  });

  it('正規化の操作があれば、その目標を出して選択を止める・計っている間は前の値に「計っています…」', () => {
    openState([node(4, 'audio.normalize', { target: -16, peakLimit: null })]);
    useAudioStore.setState({
      loudness: {
        type: 'ready',
        samplesId: 'old',
        loudness: {
          loudness: -20,
          loudnessReason: null,
          loudnessRange: null,
          truePeak: null,
          samplePeak: null,
        },
      },
      loudnessFresh: false,
    });
    render(<Editor />);
    expect(screen.getByText(a.normalizeTarget)).toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: a.normalizeTarget })).toBeDisabled();
    expect(screen.getByText('−20.0 LUFS')).toHaveAttribute('data-stale', 'true');
    expect(screen.getByRole('status')).toHaveTextContent(a.measuring);
    expect(screen.queryByRole('button', { name: a.alignToTarget })).toBeNull();
  });

  it('目標との差は符号つき（0 は符号なし）', () => {
    const ready = (l: number) =>
      ({
        type: 'ready',
        samplesId: 'x',
        loudness: {
          loudness: l,
          loudnessReason: null,
          loudnessRange: null,
          truePeak: null,
          samplePeak: null,
        },
      }) as const;
    expect(targetDiff(ready(-14.2), -23)).toBe('+8.8');
    expect(targetDiff(ready(-24), -23)).toBe('−1.0');
    expect(targetDiff(ready(-23.01), -23)).toBe('0.0');
    expect(targetDiff({ type: 'idle' }, -23)).toBeNull();
  });
});

describe('引数の時刻の欄', () => {
  it('標本の引数は時刻の欄（下に標本の位置）、区間は始め・終わりの 2 つ', () => {
    openState([
      node(5, 'audio.trim', { start: 48_000, end: null }),
      node(6, 'audio.set_loop', { loop: { start: 0, end: 4800 } }),
    ]);
    useEditorStore.setState({ selected: 5 });
    const time = { rate: () => 48_000, frames: () => TOTAL };
    const { rerender } = render(<OpsPanel time={time} />);
    expect(screen.getByLabelText(ja.editor.ops['audio.trim']?.params.start ?? '')).toHaveValue(
      '0:01.000',
    );
    expect(screen.getByText(a.samples('48,000'))).toBeInTheDocument();
    useEditorStore.setState({ selected: 6 });
    rerender(<OpsPanel time={time} />);
    expect(screen.getByLabelText(a.start)).toHaveValue('0:00.000');
    expect(screen.getByLabelText(a.end)).toHaveValue('0:00.100');
  });
});

describe('スペクトルの面（M17 段 5）', () => {
  it('上部の中央に「波形｜スペクトル」（キーを添えた説明）。押すとスペクトルの面で、右下は周波数の範囲（− ＋ なし）、左下にアナライザが出しているもの', () => {
    openState();
    render(<Editor />);
    const faces = screen.getByRole('group', { name: a.faces });
    const wave = within(faces).getByRole('button', { name: a.faceNames.wave });
    const spectrum = within(faces).getByRole('button', { name: a.faceNames.spectrum });
    expect(wave).toHaveAttribute('aria-pressed', 'true');
    expect(spectrum).toHaveAttribute('title', t.withKey(a.faceNames.spectrum, '⌘2'));
    fireEvent.click(spectrum);
    expect(useAudioStore.getState().face).toBe('spectrum');
    expect(screen.getByRole('group', { name: a.spectrum.face })).toBeInTheDocument();
    expect(screen.getByText(a.spectrum.freqRange('20 Hz', '24 kHz'))).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: a.zoomIn })).toBeNull();
    expect(screen.getByText(a.spectrum.analyzerCursor)).toBeInTheDocument();
    // 凡例と表示の設定（押すと欄が出る）
    expect(screen.getByText(a.spectrum.legend.output)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: a.spectrum.settings }));
    expect(screen.getByLabelText(a.spectrum.fall)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: a.spectrum.resetHold })).toBeInTheDocument();
  });
});

describe('イコライザのバンドの表とキー（M17 段 5）', () => {
  const eqNode = (bands: unknown[]): EditNodeDto =>
    ({
      id: 5,
      op: 'audio.eq',
      v: 1,
      enabled: true,
      params: { bands, phase: 'minimum', gainDb: 0 },
      log: null,
    }) as unknown as EditNodeDto;
  const bell = (freq: number, gainDb: number, enabled = true) => ({
    type: 'bell',
    freq,
    gainDb,
    q: 1,
    slope: 12,
    channel: 'all',
    enabled,
  });

  beforeEach(() => {
    useAudioStore.setState({ eqBand: null, eqLocal: null, eqAnnounce: '' });
  });

  it('右パネル：「n バンドのうち m が有効」・行（押すと選ぶ）・オンのスイッチ・バンドを足す・選んだバンドの欄。位相の欄は出さない', () => {
    openState([eqNode([bell(1_000, -5), bell(4_000, 3, false)])]);
    useEditorStore.setState({ selected: 5 });
    render(<Editor />);
    const s = a.spectrum;
    expect(screen.getByText(s.bandCount(2, 1))).toBeInTheDocument();
    expect(screen.queryByText(t.ops['audio.eq']?.params.phase ?? '位相')).toBeNull();
    const rows = within(screen.getByRole('list', { name: s.bands })).getAllByRole('button');
    expect(rows).toHaveLength(2);
    fireEvent.click(rows[1] as HTMLElement);
    expect(useAudioStore.getState().eqBand).toBe(1);
    expect(screen.getByRole('group', { name: /バンド 2/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('switch', { name: s.bandEnabled(2) }));
    expect(useAudioStore.getState().eqLocal?.bands[1]?.enabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: s.addBand }));
    expect(useAudioStore.getState().eqLocal?.bands).toHaveLength(3);
    expect(useAudioStore.getState().eqBand).toBe(2);
  });

  it('面のキー：選んだバンドを ←→ で周波数・↑↓ で利得・⌫ で消す、[ ] で前・次。選んでいなければ何もしない。行の ↑↓ でバンドを移る', () => {
    openState([eqNode([bell(1_000, 0), bell(4_000, 0)])]);
    useEditorStore.setState({ selected: 5 });
    useAudioStore.setState({ face: 'spectrum' });
    render(<Editor />);
    const face = screen.getByRole('group', { name: a.spectrum.face });
    const key = (target: HTMLElement, k: string, init: KeyboardEventInit = {}) =>
      fireEvent.keyDown(target, { key: k, bubbles: true, ...init });
    key(face, 'ArrowRight');
    expect(useAudioStore.getState().eqLocal).toBeNull();
    key(face, ']');
    expect(useAudioStore.getState().eqBand).toBe(0);
    key(face, 'ArrowRight');
    expect(useAudioStore.getState().eqLocal?.bands[0]?.freq).toBeCloseTo(1_000 * 2 ** (1 / 12), 6);
    key(face, 'ArrowUp', { shiftKey: true });
    expect(useAudioStore.getState().eqLocal?.bands[0]?.gainDb).toBeCloseTo(2, 9);
    expect(useAudioStore.getState().eqAnnounce).toContain('バンド 1');
    key(face, 'Backspace');
    expect(useAudioStore.getState().eqLocal?.bands).toHaveLength(1);
    // 行のキー
    useAudioStore.setState({ eqLocal: null, eqBand: null });
    const rows = within(screen.getByRole('list', { name: a.spectrum.bands })).getAllByRole(
      'button',
    );
    key(rows[0] as HTMLElement, 'ArrowDown');
    expect(useAudioStore.getState().eqBand).toBe(1);
  });

  it('⌫ はバンドだけを消し、イコライザの操作は消さない。行の ⌫ のあとフォーカスは残りの行（なければ［＋ バンドを足す］）へ', async () => {
    openState([eqNode([bell(1_000, 0), bell(4_000, 0)])]);
    useEditorStore.setState({ selected: 5 });
    useAudioStore.setState({ face: 'spectrum' });
    render(<Editor />);
    const face = screen.getByRole('group', { name: a.spectrum.face });
    const key = (target: Element, k: string) =>
      fireEvent.keyDown(target, { key: k, bubbles: true });
    // バンドを選んでいない面の ⌫：何もしない（操作も消さない）
    key(face, 'Backspace');
    expect(useEditorStore.getState().doc.order).toContain(5);
    expect(useAudioStore.getState().eqLocal).toBeNull();
    const list = screen.getByRole('list', { name: a.spectrum.bands });
    const last = within(list).getAllByRole('button')[1] as HTMLElement;
    last.focus();
    key(last, 'Backspace');
    expect(useAudioStore.getState().eqLocal?.bands).toHaveLength(1);
    await waitFor(() =>
      expect(document.activeElement).toBe(within(list).getAllByRole('button')[0]),
    );
    key(document.activeElement as Element, 'Backspace');
    expect(useAudioStore.getState().eqLocal?.bands).toHaveLength(0);
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole('button', { name: a.spectrum.addBand })),
    );
    expect(useEditorStore.getState().doc.order).toContain(5);
  });

  it('行を押して ⌫ を押すと、そのバンドだけ消える（押した行にフォーカスを移す。WebKit はボタンを押してもフォーカスを移さない）', () => {
    openState([eqNode([bell(1_000, 0), bell(4_000, 0)])]);
    useEditorStore.setState({ selected: 5 });
    render(<Editor />);
    const list = screen.getByRole('list', { name: a.spectrum.bands });
    const row = within(list).getAllByRole('button')[0] as HTMLElement;
    // jsdom の click もフォーカスを移さない（WebKit と同じ）
    fireEvent.click(row);
    expect(document.activeElement).toBe(row);
    fireEvent.keyDown(document.activeElement as Element, { key: 'Backspace', bubbles: true });
    expect(useAudioStore.getState().eqLocal?.bands).toHaveLength(1);
    expect(useEditorStore.getState().doc.order).toContain(5);
  });

  it('スペクトルのときのカプセルの下の段：選んだバンドの読みと使い方の一言', () => {
    openState([eqNode([bell(2_800, -5)])]);
    useEditorStore.setState({ selected: 5 });
    useAudioStore.setState({ face: 'spectrum', eqBand: 0 });
    render(<Editor />);
    const capsule = screen.getByRole('toolbar', { name: t.toolSettings });
    expect(capsule.textContent).toContain(
      a.spectrum.bandReading(1, 'ベル · 2.80 kHz · −5.0 dB · Q 1.00'),
    );
    expect(screen.getByText(a.spectrum.usage)).toBeInTheDocument();
  });
});
