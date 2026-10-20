import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { MediaImage, type MediaSource } from './MediaImage';

const base: MediaSource = {
  hash: 'ab',
  kind: 'image',
  thumbState: 'ready',
  thumbRev: 0,
  alpha: null,
  pixelArt: false,
};

/** 描いた <img>（なければ null）。 */
function image(container: HTMLElement): HTMLImageElement | null {
  return container.querySelector('img');
}

/** 絵が読めたことにする（jsdom は画像を読まないので、大きさを付けて load を送る）。 */
function load(img: HTMLImageElement, w: number, h: number) {
  Object.defineProperty(img, 'naturalWidth', { configurable: true, value: w });
  Object.defineProperty(img, 'naturalHeight', { configurable: true, value: h });
  fireEvent.load(img);
}

function setDpr(value: number) {
  Object.defineProperty(window, 'devicePixelRatio', { configurable: true, value });
}

afterEach(() => setDpr(1));

describe('MediaImage', () => {
  it('市松模様は解析で透過と分かった画像だけ（二値・半透明）', () => {
    const box = { w: 200, h: 150 };
    const checker = (source: Partial<MediaSource>) => {
      const { container, unmount } = render(
        <MediaImage source={{ ...base, ...source }} size="s" fit="cover" box={box} />,
      );
      const on = image(container)?.hasAttribute('data-checker') ?? false;
      unmount();
      return on;
    };
    expect(checker({ alpha: 'binary' })).toBe(true);
    expect(checker({ alpha: 'partial' })).toBe(true);
    expect(checker({ alpha: 'none' })).toBe(false);
    expect(checker({ alpha: null })).toBe(false);
    // フォント・3D・動画には敷かない
    expect(checker({ alpha: 'partial', kind: 'model' })).toBe(false);
    expect(checker({ alpha: 'partial', kind: 'video' })).toBe(false);
  });

  it('表示の短い辺が 48px 未満（リストの 24px・キューの 44px）には敷かない', () => {
    const at = (w: number, h: number) => {
      const { container, unmount } = render(
        <MediaImage source={{ ...base, alpha: 'binary' }} size="s" fit="cover" box={{ w, h }} />,
      );
      const on = image(container)?.hasAttribute('data-checker') ?? false;
      unmount();
      return on;
    };
    expect(at(24, 24)).toBe(false);
    expect(at(44, 44)).toBe(false);
    expect(at(48, 200)).toBe(true);
  });

  it('大きさの分からない枠では、測れるまで敷かない', () => {
    const { container } = render(
      <MediaImage source={{ ...base, alpha: 'binary' }} size="s" fit="cover" />,
    );
    expect(image(container)).not.toHaveAttribute('data-checker');
  });

  it('ドット絵は広げるときだけ最近傍（タイルの cover）', () => {
    const tile = (source: Partial<MediaSource>, natural: number) => {
      const { container, unmount } = render(
        <MediaImage
          source={{ ...base, ...source }}
          size="s"
          fit="cover"
          box={{ w: 160, h: 160 }}
        />,
      );
      const img = image(container);
      if (!img) throw new Error('img がない');
      expect(img).toHaveAttribute('data-render', 'auto');
      load(img, natural, natural);
      const value = img.getAttribute('data-render');
      unmount();
      return value;
    };
    expect(tile({ pixelArt: true }, 16)).toBe('pixelated');
    expect(tile({ pixelArt: true }, 160)).toBe('pixelated');
    expect(tile({ pixelArt: true }, 512)).toBe('auto');
    expect(tile({ pixelArt: false }, 16)).toBe('auto');
  });

  it('装置の画素で数える（DPR 2 の 160px は 320 画素）', () => {
    setDpr(2);
    const { container } = render(
      <MediaImage
        source={{ ...base, pixelArt: true }}
        size="s"
        fit="cover"
        box={{ w: 160, h: 160 }}
      />,
    );
    const img = image(container);
    if (!img) throw new Error('img がない');
    load(img, 256, 256);
    expect(img).toHaveAttribute('data-render', 'pixelated');
  });

  it('プレビュー（contain）のドット絵は、箱に収まる最大の整数倍に置く', () => {
    const { container } = render(
      <MediaImage
        source={{ ...base, pixelArt: true, alpha: 'binary' }}
        size="l"
        fit="contain"
        box={{ w: 272, h: 220 }}
      />,
    );
    const img = image(container);
    if (!img) throw new Error('img がない');
    load(img, 16, 16);
    // min(272/16, 220/16) = 13.75 → 13 倍
    expect(img.style.width).toBe('208px');
    expect(img.style.height).toBe('208px');
    expect(img).toHaveAttribute('data-render', 'pixelated');
    expect(img).toHaveAttribute('data-checker');
  });

  it('DPR 2 では CSS の 0.5px 刻みの整数倍', () => {
    setDpr(2);
    const { container } = render(
      <MediaImage
        source={{ ...base, pixelArt: true }}
        size="l"
        fit="contain"
        box={{ w: 272, h: 220 }}
      />,
    );
    const img = image(container);
    if (!img) throw new Error('img がない');
    load(img, 16, 16);
    // 装置の画素で min(544/16, 440/16) = 27.5 → 27 倍 = 432 画素 = 216px
    expect(img.style.width).toBe('216px');
  });

  it('箱より大きいドット絵は縮めて補間する。ドット絵でなければ大きさを変えない', () => {
    const big = render(
      <MediaImage
        source={{ ...base, pixelArt: true }}
        size="l"
        fit="contain"
        box={{ w: 200, h: 200 }}
      />,
    );
    const img = image(big.container);
    if (!img) throw new Error('img がない');
    load(img, 400, 400);
    expect(img).toHaveAttribute('data-render', 'auto');
    expect(img.style.width).toBe('200px');
    big.unmount();

    const photo = render(
      <MediaImage source={base} size="l" fit="contain" box={{ w: 200, h: 200 }} />,
    );
    const plain = image(photo.container);
    if (!plain) throw new Error('img がない');
    load(plain, 16, 16);
    expect(plain.style.width).toBe('');
    expect(plain).toHaveAttribute('data-render', 'auto');
  });

  it('プレビューで小さく出る透過の画像には敷かない（表示の短い辺で決める）', () => {
    const { container } = render(
      <MediaImage
        source={{ ...base, alpha: 'binary' }}
        size="l"
        fit="contain"
        box={{ w: 272, h: 220 }}
      />,
    );
    const img = image(container);
    if (!img) throw new Error('img がない');
    expect(img).toHaveAttribute('data-checker');
    load(img, 32, 32);
    expect(img).not.toHaveAttribute('data-checker');
  });

  it('作り直し中（stale）も今の絵を出し、rev を URL に付ける。まだなければ代わりのもの', () => {
    const stale = render(
      <MediaImage source={{ ...base, thumbState: 'stale', thumbRev: 4 }} size="s" fit="cover" />,
    );
    expect(image(stale.container)?.getAttribute('src')).toBe('kura://localhost/thumb/ab/s?r=4');
    stale.unmount();

    render(
      <MediaImage
        source={{ ...base, thumbState: 'pending' }}
        size="s"
        fit="cover"
        fallback={<span>代わり</span>}
      />,
    );
    expect(screen.getByText('代わり')).toBeInTheDocument();
    expect(document.querySelector('img')).toBeNull();
  });

  it('フォントは字の形を mask にして色を付ける（<img> にしない）', () => {
    const { container } = render(
      <MediaImage
        source={{ ...base, kind: 'font', thumbRev: 2, alpha: 'binary' }}
        size="s"
        fit="cover"
        box={{ w: 160, h: 160 }}
      />,
    );
    expect(image(container)).toBeNull();
    const mask = container.querySelector('[data-render="mask"]');
    expect(mask).not.toBeNull();
    expect(mask?.getAttribute('style') ?? '').toContain('kura://localhost/thumb/ab/s?r=2');
    expect(mask).not.toHaveAttribute('data-checker');
  });

  it('元のファイルを先に試し、読めなければサムネイル、それも読めなければ代わりのもの', () => {
    const { container } = render(
      <MediaImage
        source={{ ...base, thumbRev: 1 }}
        size="l"
        fit="contain"
        original="kura://localhost/file/ab"
        fallback={<span>代わり</span>}
      />,
    );
    const first = image(container);
    expect(first?.getAttribute('src')).toBe('kura://localhost/file/ab');
    if (first) fireEvent.error(first);
    const second = image(container);
    expect(second?.getAttribute('src')).toBe('kura://localhost/thumb/ab/l?r=1');
    if (second) fireEvent.error(second);
    expect(image(container)).toBeNull();
    expect(screen.getByText('代わり')).toBeInTheDocument();
  });
});
