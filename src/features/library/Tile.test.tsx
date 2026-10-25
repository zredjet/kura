import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { ItemSummaryDto } from '../../ipc/items';
import type { UnitSummaryDto } from '../../ipc/units';
import { Tile, type TileHandlers, UnitTile } from './Tile';

const handlers: TileHandlers = {
  onClick: vi.fn(),
  onOpen: vi.fn(),
  onContextMenu: vi.fn(),
  onPointerDown: vi.fn(),
};

function item(over: Partial<ItemSummaryDto>): ItemSummaryDto {
  return {
    id: 1,
    hash: 'ab',
    ext: 'png',
    kind: 'image',
    width: 64,
    height: 64,
    durationMs: null,
    sizeBytes: 1,
    title: 'スプライト',
    rating: 0,
    sourceSite: null,
    importedAt: 0,
    capturedAt: null,
    thumbState: 'ready',
    proxyState: 'not_needed',
    lastPlayedAt: null,
    thumbRev: 0,
    analysisState: 'ready',
    alpha: null,
    pixelArt: false,
    pixelArtSetting: null,
    animated: false,
    frameCount: null,
    hasLoop: false,
    ...over,
  };
}

function tile(over: Partial<ItemSummaryDto>) {
  return render(
    <Tile id={1} item={item(over)} width={180} height={180} selected={false} handlers={handlers} />,
  );
}

const m = ja.mediaView;

describe('Tile（素材の表示）', () => {
  it('作り直し中（stale）のサムネイルも出し、URL に rev を付ける', () => {
    const { container } = tile({ thumbState: 'stale', thumbRev: 5 });
    expect(container.querySelector('img')?.getAttribute('src')).toBe(
      'kura://localhost/thumb/ab/s?r=5',
    );
  });

  it('透過と分かった画像には市松模様を敷く', () => {
    const { container } = tile({ alpha: 'partial' });
    expect(container.querySelector('img')).toHaveAttribute('data-checker');
  });

  it('アニメの画像は右下に「重なったコマ」の印とコマの数', () => {
    tile({ kind: 'image', ext: 'gif', animated: true, frameCount: 8 });
    expect(screen.getByText(m.frames('8'))).toBeInTheDocument();
  });

  it('コマの数が分からないアニメは印だけ（説明は「アニメ」）', () => {
    const { container } = tile({ animated: true, frameCount: null });
    expect(container.querySelector(`[title="${m.animated}"]`)).not.toBeNull();
  });

  it('アニメでない画像には印を出さない', () => {
    tile({ animated: false, frameCount: 1 });
    expect(screen.queryByText(m.frames('1'))).toBeNull();
  });

  it('ループ点のある音声は長さの前に繰り返しの印。読み上げは「ループあり」', () => {
    const { container } = tile({
      kind: 'audio',
      ext: 'ogg',
      thumbState: 'failed',
      durationMs: 4_000,
      hasLoop: true,
    });
    expect(container.querySelector(`[title="${m.loop}"]`)).not.toBeNull();
    expect(screen.getByRole('button').getAttribute('aria-label')).toContain(m.loop);
  });

  it('ループのない音声には印を出さない', () => {
    const { container } = tile({ kind: 'audio', ext: 'wav', durationMs: 4_000 });
    expect(container.querySelector(`[title="${m.loop}"]`)).toBeNull();
    expect(screen.getByRole('button').getAttribute('aria-label')).not.toContain(m.loop);
  });

  it('フォントのサムネイルは mask で色を付け、左下に「フォント」のバッジ', () => {
    const { container } = tile({ kind: 'font', ext: 'ttf', width: null, height: null });
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('[data-render="mask"]')).not.toBeNull();
    expect(screen.getByText(ja.kinds.font)).toBeInTheDocument();
  });

  it('サムネイルのない 3D は種類の絵', () => {
    const { container } = tile({ kind: 'model', ext: 'glb', thumbState: 'failed' });
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('[data-kind="model"][aria-hidden="true"]')).not.toBeNull();
    expect(screen.getByText(ja.kinds.model)).toBeInTheDocument();
  });

  describe('音声の波形（M13 段 5）', () => {
    // 確かめた結果は URL ごとに覚えるので、場合ごとに別のハッシュにする
    const hashOf = (n: number) => n.toString(16).padStart(64, '0');
    const waveOf = (hash: string) => `kura://localhost/wave/${hash}/1`;
    const decorative = (c: HTMLElement) => c.querySelector('svg[viewBox="0 0 120 24"]');
    const mask = (c: HTMLElement) => c.querySelector('[data-render="mask"]');
    const probe = (c: HTMLElement) => c.querySelector('img[hidden]');
    const song = (hash: string, over: Partial<ItemSummaryDto> = {}) =>
      tile({ kind: 'audio', ext: 'mp3', hash, thumbState: 'failed', durationMs: 4_000, ...over });

    it('ジャケットのない解析済みの音声は実波形を mask で出し、最初の 1 回だけ同じ URL を確かめる', () => {
      const hash = hashOf(1);
      const { container } = song(hash);
      expect(mask(container)?.getAttribute('style')).toContain(`url("${waveOf(hash)}")`);
      expect(decorative(container)).toBeNull();
      const img = probe(container);
      expect(img?.getAttribute('src')).toBe(waveOf(hash));
      expect(img?.getAttribute('crossorigin')).toBe('anonymous');
      expect(screen.getByText(ja.kinds.audio)).toBeInTheDocument();
    });

    it('読めたら覚えて、作り直したタイルは mask だけ（確かめの要求を出さない）', () => {
      const hash = hashOf(2);
      const first = song(hash);
      const img = probe(first.container);
      expect(img).not.toBeNull();
      fireEvent.load(img as HTMLImageElement);
      first.unmount();
      const again = song(hash);
      expect(mask(again.container)).not.toBeNull();
      expect(probe(again.container)).toBeNull();
    });

    it('読めなければ（段のファイルがない）飾りの波形に戻り、作り直したタイルも要求を出さない', () => {
      const hash = hashOf(3);
      const first = song(hash);
      fireEvent.error(probe(first.container) as HTMLImageElement);
      expect(decorative(first.container)).not.toBeNull();
      expect(mask(first.container)).toBeNull();
      expect(probe(first.container)).toBeNull();
      first.unmount();
      const again = song(hash);
      expect(decorative(again.container)).not.toBeNull();
      expect(again.container.querySelector('img')).toBeNull();
      expect(mask(again.container)).toBeNull();
    });

    it('解析の前・解析できなかった音声は飾りの波形で、波形を求めない', () => {
      for (const [n, analysisState] of [
        [4, 'pending'],
        [5, 'failed'],
      ] as const) {
        const { container, unmount } = song(hashOf(n), { analysisState });
        expect(decorative(container)).not.toBeNull();
        expect(container.querySelector('img')).toBeNull();
        expect(mask(container)).toBeNull();
        unmount();
      }
    });

    it('ジャケットがあればジャケット（実波形は出さない）', () => {
      const hash = hashOf(6);
      const { container } = song(hash, { thumbState: 'ready', thumbRev: 1 });
      const imgs = container.querySelectorAll('img');
      expect(imgs).toHaveLength(1);
      expect(imgs[0]?.getAttribute('src')).toBe(`kura://localhost/thumb/${hash}/s?r=1`);
      expect(mask(container)).toBeNull();
      expect(decorative(container)).toBeNull();
    });

    it('ジャケットが読めなければ実波形に替わる', () => {
      const hash = hashOf(7);
      const { container } = song(hash, { thumbState: 'ready' });
      fireEvent.error(container.querySelector('img') as HTMLImageElement);
      expect(mask(container)).not.toBeNull();
      expect(probe(container)?.getAttribute('src')).toBe(waveOf(hash));
    });

    it('ユニットの表紙の音声も同じ（ジャケットがなく解析済みなら実波形）', () => {
      const hash = hashOf(8);
      const unit = {
        id: 4,
        kind: 'set',
        name: '効果音',
        cover: {
          itemId: 2,
          hash,
          kind: 'audio',
          thumbState: 'failed',
          width: null,
          height: null,
          thumbRev: 0,
          analysisState: 'ready',
          alpha: null,
          pixelArt: false,
        },
        count: 3,
        durationMs: 0,
        kinds: ['audio'],
        rating: 0,
        ownRating: 0,
        origin: 'user',
        createdAt: 0,
        lastPlayedAt: null,
      } as UnitSummaryDto;
      const { container } = render(
        <UnitTile
          id={9}
          unit={unit}
          width={180}
          height={180}
          selected={false}
          handlers={handlers}
        />,
      );
      expect(mask(container)).not.toBeNull();
      expect(probe(container)?.getAttribute('src')).toBe(waveOf(hash));
    });
  });

  it('ユニットの表紙も作り直し中の絵を rev 付きで出し、透過なら市松模様', () => {
    const unit = {
      id: 3,
      kind: 'set',
      name: '歩き',
      cover: {
        itemId: 1,
        hash: 'cd',
        kind: 'image',
        thumbState: 'stale',
        width: 64,
        height: 64,
        thumbRev: 2,
        analysisState: 'ready',
        alpha: 'binary',
        pixelArt: true,
      },
      count: 4,
      durationMs: 0,
      kinds: ['image'],
      rating: 0,
      ownRating: 0,
      origin: 'user',
      createdAt: 0,
      lastPlayedAt: null,
    } as UnitSummaryDto;
    const { container } = render(
      <UnitTile id={9} unit={unit} width={180} height={180} selected={false} handlers={handlers} />,
    );
    const img = container.querySelector('img');
    expect(img?.getAttribute('src')).toBe('kura://localhost/thumb/cd/s?r=2');
    expect(img).toHaveAttribute('data-checker');
  });
});
