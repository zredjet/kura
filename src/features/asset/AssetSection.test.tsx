import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { retryAnalysis, setAssetSettings } from '../../ipc/asset';
import { invalidateItems } from '../../stores/itemCache';
import { AssetSection } from './AssetSection';
import { startAudition, stopAudition, useAuditionStore } from './auditionStore';
import { audioAsset, AUDIO_FACTS, imageAsset, summary } from './testAssets';
import type { AssetData } from './useAssetData';

vi.mock('../../ipc/asset', () => ({
  setAssetSettings: vi.fn(async () => 1),
  retryAnalysis: vi.fn(async () => 1),
  getAsset: vi.fn(),
}));
vi.mock('../../stores/itemCache', () => ({ invalidateItems: vi.fn() }));
vi.mock('../player/playerActions', () => ({ playerController: vi.fn() }));
vi.mock('./auditionStore', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./auditionStore')>()),
  startAudition: vi.fn(),
  stopAudition: vi.fn(),
}));
vi.mock('./WaveformView', () => ({
  WaveformView: (p: { hash: string; loop: readonly [number, number] | null }) => (
    <div data-testid="waveform" data-hash={p.hash} data-loop={p.loop?.join('-') ?? ''} />
  ),
}));

const t = ja.asset;

function data(asset: AssetData['asset'], patch: Partial<AssetData> = {}): AssetData {
  return { asset, anim: { status: 'none' }, reload: vi.fn(), ...patch };
}

/** 素材の欄の表で、ラベルの次の値。 */
function valueOf(label: string): string {
  const dt = screen.getAllByText(label, { selector: 'dt' })[0];
  return dt?.nextElementSibling?.textContent ?? '';
}

describe('AssetSection（インスペクタの素材の欄）', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useAuditionStore.setState({ itemId: null, status: 'idle', mode: null, fromStart: false });
  });

  it('画像：大きさ・透過・色数・自動の判定・アニメ・格子を書く', () => {
    render(
      <AssetSection
        data={data(imageAsset(), {
          anim: {
            status: 'ready',
            anim: {
              hash: 'ab',
              version: 1,
              rev: 0,
              canvasW: 64,
              canvasH: 64,
              cols: 8,
              frameCount: 8,
              durations: [100, 100, 100, 100, 100, 100, 100, 100],
              tags: [],
            },
          },
        })}
        item={summary()}
      />,
    );
    expect(screen.getByRole('heading', { name: t.section })).toBeInTheDocument();
    expect(valueOf(t.size)).toBe('64 × 64（2 のべき乗）');
    expect(valueOf(t.alpha)).toBe('半透明あり');
    expect(valueOf(t.colors)).toBe('256 色より多い');
    expect(screen.getByText('自動の判定：はい（4 倍）')).toBeInTheDocument();
    expect(valueOf(t.anim)).toBe('8 コマ · 0.8 秒');
    expect(valueOf(t.grid)).toContain('なし');
    // 解析が済んでいれば状態の行は出さない
    expect(screen.queryByText(t.analyzing)).toBeNull();
  });

  it('ドット絵の 3 択：押したものを patch で送り、タイルと欄を読み直す', async () => {
    const reload = vi.fn();
    render(<AssetSection data={data(imageAsset(), { reload })} item={summary()} />);
    const group = screen.getByRole('group', { name: t.pixelArtGroup });
    expect(screen.getByRole('button', { name: t.pixelChoices.auto })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    fireEvent.click(screen.getByRole('button', { name: t.pixelChoices.no }));
    expect(setAssetSettings).toHaveBeenCalledWith([1], { pixelArt: 'no' });
    await waitFor(() => expect(reload).toHaveBeenCalled());
    expect(invalidateItems).toHaveBeenCalledWith([1]);
    expect(group).toBeInTheDocument();
  });

  it('設定が「はい」なら「はい」が押されていて、自動に戻すと auto を送る', () => {
    const asset = imageAsset({ settings: { ...imageAsset().settings, pixelArt: true } });
    render(<AssetSection data={data(asset)} item={summary()} />);
    expect(screen.getByRole('button', { name: t.pixelChoices.yes })).toHaveAttribute(
      'aria-pressed',
      'true',
    );
    fireEvent.click(screen.getByRole('button', { name: t.pixelChoices.auto }));
    expect(setAssetSettings).toHaveBeenCalledWith([1], { pixelArt: 'auto' });
  });

  it('大きすぎて帯を作れないものは理由を添える', () => {
    render(
      <AssetSection data={data(imageAsset(), { anim: { status: 'tooLarge' } })} item={summary()} />,
    );
    expect(screen.getByText(t.animTooLarge)).toBeInTheDocument();
    expect(valueOf(t.anim)).toContain('8 コマ');
  });

  it('音声：形式・ラウドネス（U+2212）・ピーク・ループと標本の位置、波形と試聴のボタン', () => {
    render(
      <AssetSection
        data={data(audioAsset())}
        item={summary({ id: 2, kind: 'audio', durationMs: 92_000 })}
      />,
    );
    expect(valueOf(t.format)).toBe('44.1 kHz · 24 ビット · ステレオ');
    expect(valueOf(t.loudness)).toBe('−14.2 LUFS（LRA 6.1 LU）');
    expect(valueOf(t.peak)).toBe('−1.0 dBTP');
    expect(valueOf(t.loop)).toBe('0:04.200 – 1:32.000185,220 – 4,057,200');
    expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '185220-4057200');
    const audition = screen.getByRole('button', { name: t.audition });
    expect(audition).toHaveAttribute('aria-pressed', 'false');
    fireEvent.click(audition);
    expect(startAudition).toHaveBeenCalledWith(
      { itemId: 2, hash: 'cd'.repeat(32), rate: 44_100, channels: 2 },
      false,
    );
    fireEvent.click(screen.getByRole('button', { name: t.fromStart }));
    expect(startAudition).toHaveBeenLastCalledWith(expect.objectContaining({ itemId: 2 }), true);
  });

  it('試聴中は［試聴を止める］（aria-pressed）で止める', () => {
    render(
      <AssetSection
        data={data(audioAsset())}
        item={summary({ id: 2, kind: 'audio', durationMs: 92_000 })}
      />,
    );
    act(() => {
      useAuditionStore.setState({ itemId: 2, status: 'playing', mode: 'loop', fromStart: false });
    });
    const stop = screen.getByRole('button', { name: t.auditionStop });
    expect(stop).toHaveAttribute('aria-pressed', 'true');
    fireEvent.click(stop);
    expect(stopAudition).toHaveBeenCalled();
    expect(startAudition).not.toHaveBeenCalled();
  });

  it('ループ点のない音声でも［ループを試聴］を出す（全体を回す）。計れないラウドネスは理由', () => {
    const asset = audioAsset({
      facts: {
        ...audioAsset().facts!,
        audio: {
          ...AUDIO_FACTS,
          hasLoop: false,
          loopRange: null,
          loudness: null,
          loudnessRange: null,
          loudnessReason: 'tooShort',
        },
      },
    });
    render(
      <AssetSection data={data(asset)} item={summary({ id: 2, kind: 'audio', durationMs: 300 })} />,
    );
    expect(valueOf(t.loop)).toBe(t.loopNone);
    expect(valueOf(t.loudness)).toBe('短くて計れません');
    expect(screen.getByRole('button', { name: t.audition })).toBeEnabled();
    expect(screen.queryByText(t.seamOnly)).toBeNull();
  });

  it('本体が長すぎるものは「継ぎ目の前後だけを試聴します」', () => {
    const asset = audioAsset({
      facts: {
        ...audioAsset().facts!,
        audio: { ...AUDIO_FACTS, loopRange: null, hasLoop: false },
      },
    });
    // 44.1kHz ステレオの浮動小数で 64MiB を超える長さ（約 3 分 10 秒より長い）
    render(
      <AssetSection
        data={data(asset)}
        item={summary({ id: 2, kind: 'audio', durationMs: 600_000 })}
      />,
    );
    expect(screen.getByText(t.seamOnly)).toBeInTheDocument();
  });

  it('解析を待っている間は「解析しています…」で、試聴のボタンは理由つきで押せない', () => {
    const asset = audioAsset({ analysisState: 'pending', facts: null });
    render(<AssetSection data={data(asset)} item={summary({ id: 2, kind: 'audio' })} />);
    expect(screen.getByRole('status')).toHaveTextContent(t.analyzing);
    expect(screen.getByRole('button', { name: t.audition })).toBeDisabled();
    expect(screen.getByText(t.auditionNoFacts)).toBeInTheDocument();
    expect(valueOf(t.format)).toBe('—');
  });

  it('解析できなかったら理由と「もう一度解析する」（retry_analysis のあと読み直す）', async () => {
    const reload = vi.fn();
    const asset = imageAsset({
      analysisState: 'failed',
      facts: { ...imageAsset().facts!, ok: false, errorCode: 'decode' },
    });
    render(<AssetSection data={data(asset, { reload })} item={summary()} />);
    expect(screen.getByText(t.analysisFailed('中身を読めません'))).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.retryAnalysis }));
    expect(retryAnalysis).toHaveBeenCalledWith([1]);
    await waitFor(() => expect(reload).toHaveBeenCalled());
  });

  it('フォント：書体名・スタイル・字の数・形式', () => {
    const asset = imageAsset({
      kind: 'font',
      ext: 'otf',
      width: null,
      height: null,
      analysisState: 'not_needed',
      facts: null,
      font: {
        family: 'Kura Sans',
        style: 'Bold',
        fullName: 'Kura Sans Bold',
        postscriptName: 'KuraSans-Bold',
        glyphs: 2340,
        faces: 1,
        variable: false,
        format: 'otf',
      },
    });
    render(<AssetSection data={data(asset)} item={summary({ kind: 'font' })} />);
    expect(valueOf(t.family)).toBe('Kura Sans');
    expect(valueOf(t.style)).toBe('Bold');
    expect(valueOf(t.glyphs)).toBe('2,340 字');
    expect(valueOf(t.fontFormat)).toBe('OpenType（CFF）');
    expect(screen.queryByText(t.analyzing)).toBeNull();
  });

  it('3D：頂点・三角形・大きさ・材質と、描けない拡張の印', () => {
    const asset = imageAsset({
      kind: 'model',
      ext: 'glb',
      width: null,
      height: null,
      analysisState: 'not_needed',
      facts: null,
      model: {
        vertices: 12_480,
        triangles: 8_320,
        materials: 3,
        textures: 1,
        meshes: 2,
        size: [1.2, 0.8, 0.45],
        unsupported: ['KHR_draco_mesh_compression'],
      },
    });
    render(<AssetSection data={data(asset)} item={summary({ kind: 'model' })} />);
    expect(valueOf(t.vertices)).toBe('12,480');
    expect(valueOf(t.triangles)).toBe('8,320');
    expect(valueOf(t.modelSize)).toBe('1.20 × 0.80 × 0.45');
    expect(valueOf(t.materials)).toBe('3');
    expect(screen.getByText('未対応：Draco 圧縮')).toBeInTheDocument();
  });

  it('動画・読む前・別のアイテムの中身では何も出さない', () => {
    const { container, rerender } = render(<AssetSection data={data(null)} item={summary()} />);
    expect(container).toBeEmptyDOMElement();
    rerender(<AssetSection data={data(imageAsset({ kind: 'video' }))} item={summary()} />);
    expect(container).toBeEmptyDOMElement();
    rerender(<AssetSection data={data(imageAsset({ itemId: 9 }))} item={summary()} />);
    expect(container).toBeEmptyDOMElement();
  });
});
