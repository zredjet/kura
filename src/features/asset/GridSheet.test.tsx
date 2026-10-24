import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { setAssetSettings } from '../../ipc/asset';
import { checkGrid, formOf, GridSheet, parseGridForm, withEstimate } from './GridSheet';
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

  it('推定があれば「推定を使う（14 × 14）」で入力欄を埋めるだけ（保存しない）。保存で推定の格子を送る', async () => {
    const onClose = vi.fn();
    // 64 × 32 に 14 × 14・ずれ 1・間隔 2 で 4 × 2 = 8 セル、中身のあるセル 6（末尾の 2 つが空）
    const estimate = { cellW: 14, cellH: 14, offsetX: 1, offsetY: 1, gapX: 2, gapY: 2, count: 6 };
    const asset = imageAsset({
      width: 64,
      height: 32,
      settings: { ...imageAsset().settings, frameMs: 120 },
      gridEstimate: { grid: estimate, cols: 4, rows: 2, filled: 6 },
    });
    render(
      <GridSheet
        asset={asset}
        onClose={onClose}
        onSaved={vi.fn()}
        draw={() => undefined}
        overlay={() => undefined}
        load={async () => ({}) as CanvasImageSource}
      />,
    );
    expect(screen.getByLabelText(t.cellW)).toHaveValue('16');
    fireEvent.click(screen.getByRole('button', { name: t.gridUseEstimate('14', '14') }));
    expect(screen.getByLabelText(t.cellW)).toHaveValue('14');
    expect(screen.getByLabelText(t.cellH)).toHaveValue('14');
    expect(screen.getByLabelText(t.offsetX)).toHaveValue('1');
    expect(screen.getByLabelText(t.offsetY)).toHaveValue('1');
    expect(screen.getByLabelText(t.gapX)).toHaveValue('2');
    expect(screen.getByLabelText(t.gapY)).toHaveValue('2');
    expect(screen.getByLabelText(t.count)).toHaveValue('6');
    // コマの長さは推定に入らないので今の入力のまま
    expect(screen.getByLabelText(t.frameMs)).toHaveValue('120');
    expect(screen.getByText(t.gridCells('6'))).toBeInTheDocument();
    // 埋めるだけで保存しない（閉じない）
    expect(setAssetSettings).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: ja.common.save }));
    expect(setAssetSettings).toHaveBeenCalledWith([1], {
      grid: { set: estimate },
      frameMs: 120,
    });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
  });

  it('推定がなければ「推定を使う」を出さない。すべてのセルに中身がある推定は使うコマの数を空（すべて）にする', () => {
    render(
      <GridSheet
        asset={imageAsset({ width: 64, height: 32 })}
        onClose={vi.fn()}
        onSaved={vi.fn()}
        draw={() => undefined}
        overlay={() => undefined}
        load={async () => ({}) as CanvasImageSource}
      />,
    );
    // プレビューのアニメの操作のほかは「キャンセル」「保存」だけ（格子もないので「格子を外す」もない）
    const preview = screen.getByRole('group', { name: t.gridPreview });
    const buttons = screen.getAllByRole('button').filter((b) => !preview.contains(b));
    expect(buttons.map((b) => b.textContent)).toEqual([ja.common.cancel, ja.common.save]);
    const form = { ...formOf(imageAsset()), count: '3', frameMs: '80' };
    expect(withEstimate(form, { ...GRID, cellW: 32, count: null })).toEqual({
      ...form,
      cellW: '32',
      count: '',
    });
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
