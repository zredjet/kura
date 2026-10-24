import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { useMenuStore } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import type { EditNodeDto, OutputSpec } from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import {
  addOp,
  moveNode,
  removeNode,
  selectNode,
  sendParam,
  setNodeEnabled,
  setOutputFormat,
  useEditorStore,
} from './editorStore';
import { OpsPanel } from './OpsPanel';

vi.mock('./editorStore', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./editorStore')>();
  return {
    ...actual,
    addOp: vi.fn(),
    moveNode: vi.fn(),
    removeNode: vi.fn(),
    selectNode: vi.fn((id: number | null) => actual.useEditorStore.setState({ selected: id })),
    sendParam: vi.fn(),
    setNodeEnabled: vi.fn(),
    setOutputFormat: vi.fn(),
  };
});

vi.mock('../../ipc/items', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/items')>()),
  getItemDetail: vi.fn(async () => ({ originalName: 'hero.png' })),
}));

const t = ja.editor;

function node(
  id: number,
  op: string,
  params: Record<string, unknown>,
  over: Partial<EditNodeDto> = {},
) {
  return { id, op, v: 1, enabled: true, params, log: null, ...over } as EditNodeDto;
}

const OUTPUT_KEEP = [
  { id: 0, from: { from: 'input', id: 0 }, format: { type: 'keep' }, expand: { type: 'one' } },
] as unknown as OutputSpec[];

function setDoc(nodes: EditNodeDto[], selected: number | null = null, outputs = OUTPUT_KEEP) {
  useEditorStore.setState({
    phase: 'open',
    item: { id: 7, title: 'hero', ext: 'png', kind: 'image' } as ItemSummaryDto,
    source: { kind: 'image', width: 64, height: 32, frames: null, format: 'png' },
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
    view: {
      rev: 2,
      level: 0,
      fullWidth: 64,
      fullHeight: 32,
      frames: [],
      format: 'rgba8',
      fallback: null,
    },
    doc: {
      order: nodes.map((n) => n.id),
      nodes: Object.fromEntries(nodes.map((n) => [n.id, n])),
      outputs,
    },
    selected,
    issues: [],
    warnings: [],
    acked: 0,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(() => {
  useEditorStore.setState({ phase: 'closed', selected: null, item: null });
  useMenuStore.setState({ menu: null });
});

describe('操作の並び', () => {
  it('1 行に名前・要約・有効のスイッチ・並べ替え・消す', () => {
    setDoc([
      node(1, 'image.crop', { rect: { x: 0, y: 0, w: 32, h: 16 } }),
      node(2, 'image.flip', { axis: 'vertical' }, { enabled: false }),
    ]);
    render(<OpsPanel />);
    const rows = within(screen.getByRole('list', { name: t.stack })).getAllByRole('listitem');
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent(`切り抜き${t.size('32', '16')}`);
    expect(screen.getByRole('switch', { name: t.enableOp('反転') })).not.toBeChecked();
    // 先頭は上へ動かせない
    expect(within(rows[0] as HTMLElement).getByRole('button', { name: t.moveUp })).toBeDisabled();
    fireEvent.click(within(rows[0] as HTMLElement).getByRole('button', { name: t.moveDown }));
    expect(moveNode).toHaveBeenCalledWith(1, 1);
    fireEvent.click(screen.getByRole('switch', { name: t.enableOp('反転') }));
    expect(setNodeEnabled).toHaveBeenCalledWith(2, true);
    fireEvent.click(screen.getByRole('button', { name: t.removeOp('切り抜き') }));
    expect(removeNode).toHaveBeenCalledWith(1);
    fireEvent.click(screen.getByRole('button', { name: t.selectOp('反転') }));
    expect(selectNode).toHaveBeenCalledWith(2);
  });

  it('点線の「操作を追加」は分類ごとのメニュー（描画・音声は出さない）', () => {
    setDoc([]);
    render(<OpsPanel />);
    expect(screen.getByText(t.opsEmpty)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.addOp }));
    const menu = useMenuStore.getState().menu;
    const labels = menu?.entries.map((e) => (e === 'separator' ? '-' : e.label));
    expect(labels).toEqual([
      t.opCategories.geometry,
      t.opCategories.convert,
      t.opCategories.sprite,
      t.opCategories.meta,
    ]);
    const geometry = menu?.entries[0];
    const crop =
      geometry && geometry !== 'separator' && 'submenu' in geometry
        ? geometry.submenu.find((a) => a.label === '切り抜き')
        : undefined;
    crop?.onSelect();
    expect(addOp).toHaveBeenCalledWith('image.crop');
  });
});

describe('引数パネル', () => {
  it('選んでいなければ案内、選べば ParamSchema から欄を組む（表示の条件に従う）', () => {
    const convert = node(3, 'image.convert', {
      format: 'rgba8',
      colors: 256,
      dither: 'none',
      matte: [255, 255, 255],
    });
    setDoc([convert]);
    const { rerender } = render(<OpsPanel />);
    expect(screen.getByText(t.opsHint)).toBeInTheDocument();

    setDoc([convert], 3);
    rerender(<OpsPanel />);
    expect(screen.getByRole('heading', { name: '画素の形式' })).toBeInTheDocument();
    // 列挙が 5 個以上なので選択
    expect(screen.getByRole('combobox', { name: '形式' })).toHaveValue('rgba8');
    expect(screen.queryByText('色の数')).toBeNull();
    expect(screen.queryByText('ディザ')).toBeNull();

    // パレットにすると色の数（数の欄とスライダー）・ディザ（3 個なのでセグメント）が出る
    setDoc([{ ...convert, params: { ...(convert.params as object), format: 'indexed8' } }], 3);
    rerender(<OpsPanel />);
    expect(screen.getByRole('slider', { name: '色の数' })).toHaveValue('256');
    expect(screen.getByRole('spinbutton', { name: '色の数' })).toHaveValue(256);
    expect(screen.getByRole('group', { name: 'ディザ' })).toBeInTheDocument();
    fireEvent.change(screen.getByRole('combobox', { name: '形式' }), { target: { value: 'rgb8' } });
    expect(sendParam).toHaveBeenCalledWith(3, 'format', { kind: 'set', value: 'rgb8' });
  });

  it('送り方：ドラッグ中は preview、離したら確定、矢印・数の入力は merge、Enter・離れたら seal', () => {
    setDoc(
      [
        node(3, 'image.convert', {
          format: 'indexed8',
          colors: 256,
          dither: 'none',
          matte: [0, 0, 0],
        }),
      ],
      3,
    );
    render(<OpsPanel />);
    const slider = screen.getByRole('slider', { name: '色の数' });
    fireEvent.pointerDown(slider);
    fireEvent.change(slider, { target: { value: '16' } });
    fireEvent.pointerUp(slider);
    expect(vi.mocked(sendParam).mock.calls).toEqual([
      [3, 'colors', { kind: 'drag', value: 16 }],
      [3, 'colors', { kind: 'release' }],
    ]);
    vi.mocked(sendParam).mockClear();
    // キーボード（ドラッグでない）の変更は merge
    fireEvent.change(slider, { target: { value: '15' } });
    expect(sendParam).toHaveBeenLastCalledWith(3, 'colors', { kind: 'nudge', value: 15 });
    vi.mocked(sendParam).mockClear();
    const number = screen.getByRole('spinbutton', { name: '色の数' });
    fireEvent.change(number, { target: { value: '12' } });
    fireEvent.keyDown(number, { key: 'Enter' });
    expect(vi.mocked(sendParam).mock.calls).toEqual([
      [3, 'colors', { kind: 'nudge', value: 12 }],
      [3, 'colors', { kind: 'seal' }],
    ]);
    vi.mocked(sendParam).mockClear();
    // 範囲の外は打っている間は送らず、Enter で収めて送る
    fireEvent.change(number, { target: { value: '999' } });
    expect(sendParam).not.toHaveBeenCalled();
    fireEvent.blur(number);
    expect(vi.mocked(sendParam).mock.calls).toEqual([
      [3, 'colors', { kind: 'nudge', value: 256 }],
      [3, 'colors', { kind: 'seal' }],
    ]);
  });

  it('矩形は X・Y・幅・高さ、長さは px｜%｜自動。指摘はその欄の下に role="alert"', () => {
    setDoc(
      [
        node(4, 'image.crop', { rect: { x: 1, y: 2, w: 30, h: 20 } }),
        node(5, 'image.resize', { width: { px: 32 }, height: 'auto', filter: 'nearest' }),
      ],
      4,
    );
    useEditorStore.setState({
      issues: [
        { node: 4, param: 'rect', code: 'outOfRange', severity: 'error', message: '入力の外です' },
      ],
    });
    const { rerender } = render(<OpsPanel />);
    const rect = screen.getByRole('group', { name: '範囲' });
    fireEvent.change(within(rect).getByLabelText(t.rect.w), { target: { value: '31' } });
    expect(sendParam).toHaveBeenLastCalledWith(4, 'rect', {
      kind: 'nudge',
      value: { x: 1, y: 2, w: 31, h: 20 },
    });
    expect(screen.getByRole('alert')).toHaveTextContent(`${t.issues.outOfRange}（入力の外です）`);

    useEditorStore.setState({ selected: 5, issues: [] });
    rerender(<OpsPanel />);
    const width = screen.getByRole('group', { name: t.lengthMode('幅') });
    expect(within(width).getByRole('button', { name: 'px' })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    fireEvent.click(within(width).getByRole('button', { name: '%' }));
    expect(sendParam).toHaveBeenLastCalledWith(5, 'width', {
      kind: 'set',
      value: { scale: { num: 100, den: 100 } },
    });
    const height = screen.getByRole('group', { name: t.lengthMode('高さ') });
    expect(within(height).getByRole('button', { name: t.lengthModes.auto })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
  });

  it('前の操作を選んで後ろに描画があれば「後ろの描画の位置はそのままです」', () => {
    setDoc(
      [
        node(4, 'image.crop', { rect: { x: 0, y: 0, w: 8, h: 8 } }),
        node(6, 'paint.actions', { actions: [] }, { log: { field: 'actions', count: 2 } }),
      ],
      4,
    );
    const { rerender } = render(<OpsPanel />);
    expect(screen.getByText(t.laterPaint)).toBeInTheDocument();
    // 描画は描いたものの数（引数の値は [] で届き、数はノードの log）
    useEditorStore.setState({ selected: 6 });
    rerender(<OpsPanel />);
    expect(screen.getByText('描いたもの').nextSibling).toHaveTextContent('2');
    expect(screen.queryByText(t.laterPaint)).toBeNull();
  });

  it('既定値が null の引数は「指定する」のスイッチ（キャンバスの広げた部分の色）', () => {
    setDoc([node(7, 'image.canvas', { rect: { x: 0, y: 0, w: 64, h: 32 }, fill: null })], 7);
    render(<OpsPanel />);
    const toggle = screen.getByRole('switch', { name: t.specifyParam('広げた部分の色') });
    expect(toggle).not.toBeChecked();
    fireEvent.click(toggle);
    expect(sendParam).toHaveBeenLastCalledWith(7, 'fill', { kind: 'set', value: [0, 0, 0, 255] });
  });
});

describe('引数の欄の手元の値', () => {
  it('別の操作を選ぶと欄を作り直し、前の操作の手元の値を持ち越さない（送るのも新しい操作の値）', () => {
    setDoc(
      [
        node(1, 'image.crop', { rect: { x: 0, y: 0, w: 15, h: 80 } }),
        node(2, 'image.canvas', { rect: { x: 0, y: 0, w: 64, h: 32 }, fill: null }),
      ],
      1,
    );
    const { rerender } = render(<OpsPanel />);
    const crop = screen.getByRole('group', { name: '範囲' });
    const w = within(crop).getByLabelText(t.rect.w);
    fireEvent.change(w, { target: { value: '150' } });
    fireEvent.blur(w);
    // 応えがまだなので、手元の値のまま
    expect(w).toHaveValue(150);

    act(() => useEditorStore.setState({ selected: 2 }));
    rerender(<OpsPanel />);
    const canvas = screen.getByRole('group', { name: '新しいキャンバス' });
    expect(within(canvas).getByLabelText(t.rect.w)).toHaveValue(64);
    vi.mocked(sendParam).mockClear();
    fireEvent.change(within(canvas).getByLabelText(t.rect.x), { target: { value: '5' } });
    expect(sendParam).toHaveBeenLastCalledWith(2, 'rect', {
      kind: 'nudge',
      value: { x: 5, y: 0, w: 64, h: 32 },
    });
  });

  it('断られた値は欄に残さず、隣の欄を変えても送り直さない', () => {
    vi.mocked(sendParam).mockReturnValue(1);
    setDoc([node(1, 'image.crop', { rect: { x: 0, y: 0, w: 15, h: 80 } })], 1);
    render(<OpsPanel />);
    const group = screen.getByRole('group', { name: '範囲' });
    const w = within(group).getByLabelText(t.rect.w);
    fireEvent.change(w, { target: { value: '150' } });
    expect(sendParam).toHaveBeenLastCalledWith(1, 'rect', {
      kind: 'nudge',
      value: { x: 0, y: 0, w: 150, h: 80 },
    });
    // 断られた（文書は w=15 のまま）：送った手の結果が確定
    act(() => useEditorStore.setState({ acked: 1 }));
    fireEvent.blur(w);
    expect(w).toHaveValue(15);
    expect(sendParam).toHaveBeenLastCalledWith(1, 'rect', { kind: 'seal' });
    fireEvent.change(within(group).getByLabelText(t.rect.x), { target: { value: '5' } });
    expect(sendParam).toHaveBeenLastCalledWith(1, 'rect', {
      kind: 'nudge',
      value: { x: 5, y: 0, w: 15, h: 80 },
    });
  });

  it('色の引数：応えの前に続けて変えても前の変更を土台にし、見本を押して閉じたら seal', () => {
    vi.mocked(sendParam).mockReturnValue(1);
    setDoc(
      [node(7, 'image.canvas', { rect: { x: 0, y: 0, w: 64, h: 32 }, fill: [255, 0, 0, 255] })],
      7,
    );
    render(<OpsPanel />);
    const swatch = screen.getByRole('button', { name: t.paramColor('広げた部分の色', '#FF0000') });
    fireEvent.click(swatch);
    fireEvent.change(screen.getByRole('slider', { name: t.hue }), { target: { value: '240' } });
    expect(sendParam).toHaveBeenLastCalledWith(7, 'fill', {
      kind: 'nudge',
      value: [0, 0, 255, 255],
    });
    // 応えの前に不透明度を変えても、色相の変更が残る
    fireEvent.change(screen.getByRole('slider', { name: t.opacity }), { target: { value: '128' } });
    expect(sendParam).toHaveBeenLastCalledWith(7, 'fill', {
      kind: 'nudge',
      value: [0, 0, 255, 128],
    });
    fireEvent.click(
      screen.getByRole('button', { name: t.paramColor('広げた部分の色', '#0000FF80') }),
    );
    expect(sendParam).toHaveBeenLastCalledWith(7, 'fill', { kind: 'seal' });
    expect(screen.queryByRole('slider', { name: t.hue })).toBeNull();
  });

  it('長さの % は整数から刻む（下限 0.01 を刻みの起点にしない）', () => {
    setDoc(
      [
        node(5, 'image.resize', {
          width: { scale: { num: 100, den: 100 } },
          height: 'auto',
          filter: 'nearest',
        }),
      ],
      5,
    );
    render(<OpsPanel />);
    const input = screen.getByRole('spinbutton', { name: '幅' }) as HTMLInputElement;
    expect(input).toHaveValue(100);
    input.stepUp();
    expect(input.value).toBe('101');
    expect(input.validity.stepMismatch).toBe(false);
  });
});

describe('出力の形式', () => {
  it('形式を変えると「書き出すときの名前：hero.webp」', async () => {
    setDoc([], null, [
      {
        id: 0,
        from: { from: 'input', id: 0 },
        format: { type: 'to', format: 'webp' },
        expand: { type: 'one' },
      },
    ] as unknown as OutputSpec[]);
    render(<OpsPanel />);
    expect(await screen.findByText(t.exportName('hero.webp'))).toBeInTheDocument();
    const select = screen.getByRole('combobox', { name: t.output });
    expect(select).toHaveValue('webp');
    fireEvent.change(select, { target: { value: 'keep' } });
    expect(setOutputFormat).toHaveBeenCalledWith({ type: 'keep' });
  });

  it('元の形式に書けなければ（PSD）「元と同じ」を出さず、PNG で保存すると添える', async () => {
    setDoc([]);
    useEditorStore.setState({
      source: { kind: 'image', width: 64, height: 32, frames: null, format: 'psd' },
      item: { id: 7, title: 'hero', ext: 'psd', kind: 'image' } as ItemSummaryDto,
    });
    render(<OpsPanel />);
    const select = screen.getByRole('combobox', { name: t.output });
    expect(select).toHaveValue('png');
    expect(within(select).queryByText(t.outputKeep('PSD'))).toBeNull();
    expect(screen.getByText(t.outputUnwritable('PSD', 'PNG'))).toBeInTheDocument();
  });
});
