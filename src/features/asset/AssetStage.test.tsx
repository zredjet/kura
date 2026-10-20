import { render, renderHook, screen, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { fileUrl } from '../../ipc/media';
import { type FontLoader, specimenFamily, useAssetStage, useFontFace } from './AssetStage';
import { imageAsset } from './testAssets';
import type { AssetData } from './useAssetData';

vi.mock('../player/playerActions', () => ({ playerController: vi.fn() }));

function loader(ok = true) {
  const release = vi.fn();
  const load = vi.fn(async () => {
    if (!ok) throw new Error('読めない');
    return { release };
  });
  return { loader: { load } as FontLoader, load, release };
}

const FONT = imageAsset({
  kind: 'font',
  ext: 'ttf',
  analysisState: 'not_needed',
  facts: null,
  font: {
    family: 'Kura Sans',
    style: 'Regular',
    fullName: null,
    postscriptName: null,
    glyphs: 10,
    faces: 1,
    variable: false,
    format: 'ttf',
  },
});

describe('インスペクタのプレビューを置き換えるもの', () => {
  it('ライブラリのフォントを kura:// の file から FontFace で読み、見本を出す。離れたら登録を外す', async () => {
    const f = loader();
    const { result, unmount } = renderHook(() => useFontFace(FONT.hash, f.loader));
    await waitFor(() => expect(result.current).toBe(specimenFamily(FONT.hash)));
    expect(f.load).toHaveBeenCalledWith(specimenFamily(FONT.hash), fileUrl(FONT.hash));
    unmount();
    expect(f.release).toHaveBeenCalled();
  });

  it('フォントの見本は「永あAa」と「いろはにほへと ABCabc 0123」を、その書体で', async () => {
    const f = loader();
    const data: AssetData = { asset: FONT, anim: { status: 'none' }, reload: vi.fn() };
    const { result } = renderHook(() => useAssetStage(data, f.loader));
    await waitFor(() => expect(result.current).not.toBeNull());
    render(<>{result.current as ReactNode}</>);
    const specimen = screen.getByRole('img', { name: ja.asset.specimenLabel('Kura Sans') });
    expect(specimen).toHaveStyle({ fontFamily: `"${specimenFamily(FONT.hash)}"` });
    expect(screen.getByText(ja.asset.specimenLarge)).toBeInTheDocument();
    expect(screen.getByText(ja.asset.specimenSmall)).toBeInTheDocument();
  });

  it('フォントを読めなければ置き換えない（サムネイルのまま）。アニメでない画像も置き換えない', async () => {
    const f = loader(false);
    const data: AssetData = { asset: FONT, anim: { status: 'none' }, reload: vi.fn() };
    const { result } = renderHook(() => useAssetStage(data, f.loader));
    await waitFor(() => expect(f.load).toHaveBeenCalled());
    expect(result.current).toBeNull();
    const still: AssetData = { asset: imageAsset(), anim: { status: 'tooLarge' }, reload: vi.fn() };
    expect(renderHook(() => useAssetStage(still, f.loader)).result.current).toBeNull();
  });
});
