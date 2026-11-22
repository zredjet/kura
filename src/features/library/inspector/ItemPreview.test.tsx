import { act, render } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { thumbUrl } from '../../../ipc/media';
import { getCached, useItemCacheVersion } from '../../../stores/itemCache';
import { summary } from '../../asset/testAssets';
import { ItemPreview } from './ItemPreview';

vi.mock('../../../stores/itemCache', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../stores/itemCache')>()),
  getCached: vi.fn(),
}));

// design.md §3.1「表示の部品」：インスペクタのプレビューは一覧のキャッシュの新しい概要を使い、作り直し・解析が届いたら替わる。
// インスペクタが読んだ詳細（item）は版（revision）が変わらないと読み直さないので、サムネイルのジョブの更新はキャッシュにしか届かない
describe('インスペクタのプレビュー', () => {
  it('一覧のキャッシュに新しい概要が届いたら、そちらの絵（作り直したサムネイルの rev）に替わる', () => {
    const detail = summary({ id: 1, thumbState: 'stale', thumbRev: 0 });
    vi.mocked(getCached).mockReturnValue(undefined);
    const { container } = render(<ItemPreview item={detail} />);
    expect(container.querySelector('img')).toHaveAttribute('src', thumbUrl(detail.hash, 'l', 0));

    vi.mocked(getCached).mockReturnValue({ ...detail, thumbState: 'ready', thumbRev: 3 });
    act(() => useItemCacheVersion.setState((s) => ({ version: s.version + 1 })));
    expect(container.querySelector('img')).toHaveAttribute('src', thumbUrl(detail.hash, 'l', 3));
  });

  it('サムネイルを作れなかったと届いたら、キャッシュの概要で種類の絵に替わる（フォント・3D）', () => {
    const detail = summary({ id: 4, kind: 'model', thumbState: 'ready', thumbRev: 1 });
    vi.mocked(getCached).mockReturnValue(undefined);
    const { container } = render(<ItemPreview item={detail} />);
    expect(container.querySelector('img')).not.toBeNull();
    vi.mocked(getCached).mockReturnValue({ ...detail, thumbState: 'failed' });
    act(() => useItemCacheVersion.setState((s) => ({ version: s.version + 1 })));
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('svg')).not.toBeNull();
  });
});
