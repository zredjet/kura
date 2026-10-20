import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { setAssetSettings } from '../../ipc/asset';
import { checkGrid, formOf, GridSheet, parseGridForm } from './GridSheet';
import { imageAsset } from './testAssets';

vi.mock('../../ipc/asset', () => ({ setAssetSettings: vi.fn(async () => 1) }));

const t = ja.asset;
const GRID = { cellW: 16, cellH: 16, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0, count: null };

describe('格子のシート', () => {
  beforeEach(() => vi.clearAllMocks());

  it('入力を読む（空のコマの数はすべて、空のコマの長さは自動 = 0。数でないものは断る）', () => {
    const form = formOf(imageAsset());
    expect(form).toMatchObject({ cellW: '16', cellH: '16', offsetX: '0', count: '', frameMs: '' });
    expect(parseGridForm(form)).toEqual({ grid: GRID, frameMs: 0 });
    expect(parseGridForm({ ...form, count: '12', frameMs: '80' })).toEqual({
      grid: { ...GRID, count: 12 },
      frameMs: 80,
    });
    expect(parseGridForm({ ...form, cellW: '0' })).toBeNull();
    expect(parseGridForm({ ...form, gapX: '-1' })).toBeNull();
    expect(parseGridForm({ ...form, count: 'abc' })).toBeNull();
    expect(parseGridForm({ ...form, frameMs: '1.5' })).toBeNull();
  });

  it('セルが 1 つもできない・コマの数がセルより多い格子を断る（エンジンと同じ数え方）', () => {
    expect(checkGrid(GRID, 64, 32).cells).toHaveLength(8);
    expect(checkGrid({ ...GRID, count: 3 }, 64, 32).cells).toHaveLength(3);
    expect(checkGrid({ ...GRID, cellW: 65 }, 64, 32).error).toBe(t.gridNoCells);
    expect(checkGrid({ ...GRID, count: 9 }, 64, 32).error).toBe(t.gridTooMany('8'));
    expect(checkGrid(null, 64, 32).error).toBe(t.gridNoCells);
    // 大きさが分からなければセルの大きさだけ
    expect(checkGrid(GRID, null, null)).toEqual({ error: null, cells: [] });
  });

  it('保存で格子とコマの長さを送り、閉じて読み直す', async () => {
    const onClose = vi.fn();
    const onSaved = vi.fn();
    render(
      <GridSheet
        asset={imageAsset({ width: 64, height: 32 })}
        onClose={onClose}
        onSaved={onSaved}
        draw={() => undefined}
        overlay={() => undefined}
        load={async () => ({}) as CanvasImageSource}
      />,
    );
    expect(screen.getByRole('heading', { name: t.gridTitle })).toBeInTheDocument();
    expect(screen.getByText(t.gridCells('8'))).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(t.count), { target: { value: '6' } });
    fireEvent.change(screen.getByLabelText(t.frameMs), { target: { value: '120' } });
    fireEvent.click(screen.getByRole('button', { name: ja.common.save }));
    expect(setAssetSettings).toHaveBeenCalledWith([1], {
      grid: { set: { ...GRID, count: 6 } },
      frameMs: 120,
    });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(onSaved).toHaveBeenCalled();
    // 格子がないときは「格子を外す」を出さない
    expect(screen.queryByRole('button', { name: t.gridClear })).toBeNull();
  });

  it('合わない格子は保存できず理由を出す。格子があれば「格子を外す」で clear を送る', () => {
    const asset = imageAsset({
      width: 64,
      height: 32,
      settings: { ...imageAsset().settings, grid: GRID },
    });
    render(
      <GridSheet
        asset={asset}
        onClose={vi.fn()}
        onSaved={vi.fn()}
        draw={() => undefined}
        overlay={() => undefined}
        load={async () => ({}) as CanvasImageSource}
      />,
    );
    fireEvent.change(screen.getByLabelText(t.count), { target: { value: '9' } });
    expect(screen.getByRole('alert')).toHaveTextContent(t.gridTooMany('8'));
    expect(screen.getByRole('button', { name: ja.common.save })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: t.gridClear }));
    expect(setAssetSettings).toHaveBeenCalledWith([1], { grid: 'clear' });
  });
});
