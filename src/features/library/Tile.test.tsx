import { render, screen } from '@testing-library/react';
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
