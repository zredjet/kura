import { render } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { playUrl, thumbUrl } from '../../ipc/media';
import { summary } from '../asset/testAssets';
import { ImageLayer } from './Theater';

function srcOf(container: HTMLElement): string | null | undefined {
  return container.querySelector('img')?.getAttribute('src');
}

// design.md §3.1「アニメのプレビュー」：シアターでは先頭のコマで止めたまま（M11）
describe('シアターの画像', () => {
  it('アニメの画像（GIF・APNG・アニメの WebP）は先頭のコマ（大きいサムネイル）で止める', () => {
    const gif = summary({ ext: 'gif', animated: true, frameCount: 8, thumbRev: 2 });
    const { container } = render(<ImageLayer item={gif} />);
    expect(srcOf(container)).toBe(thumbUrl(gif.hash, 'l', 2));
  });

  it('動かない画像は元のファイル。サムネイルがまだないアニメも元のファイル', () => {
    const png = summary({ ext: 'png' });
    const still = render(<ImageLayer item={png} />);
    expect(srcOf(still.container)).toBe(playUrl(png.hash));
    still.unmount();
    const pending = summary({ ext: 'gif', animated: true, thumbState: 'pending' });
    const { container } = render(<ImageLayer item={pending} />);
    expect(srcOf(container)).toBe(playUrl(pending.hash));
  });
});
