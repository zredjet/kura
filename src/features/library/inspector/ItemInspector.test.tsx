import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { ja } from '../../../i18n/ja';
import type { ItemDetailDto } from '../../../ipc/items';
import { summary } from '../../asset/testAssets';
import { ItemInspector } from './ItemInspector';

const detail: ItemDetailDto = {
  item: summary({ id: 7, kind: 'image' }),
  mime: 'image/png',
  codecInfo: null,
  originalName: 'hero.png',
  originalPath: null,
  sourceUrl: null,
  note: null,
  trashedAt: null,
  tags: [],
  folders: [],
  units: [],
};

vi.mock('../../../ipc/items', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../ipc/items')>()),
  getItemDetail: vi.fn(async () => detail),
}));
vi.mock('../../asset/useAssetData', () => ({
  useAssetData: () => ({ asset: null, anim: { status: 'none' }, reload: vi.fn() }),
}));
vi.mock('../../asset/AssetStage', () => ({ useAssetStage: () => null }));
vi.mock('../../asset/AssetSection', () => ({ AssetSection: () => null }));
vi.mock('./ItemPreview', () => ({ ItemPreview: () => null }));
vi.mock('../../tags/TagField', () => ({ TagField: () => null }));

// design.md §3.1「画像エディタ」：エディタの右パネルの「情報」では、編集中のアイテムをエディタを通さずに開き直す・
// ゴミ箱に入れる操作を出さない
describe('アイテムのインスペクタ', () => {
  it('ふだんは「編集…」と「その他の操作」を出し、エディタの中（editable が偽）では出さない', async () => {
    const view = render(<ItemInspector id={7} />);
    expect(await screen.findByRole('button', { name: ja.inspector.more })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: ja.itemMenu.edit })).toBeInTheDocument();
    view.unmount();

    render(<ItemInspector id={7} editable={false} />);
    expect(await screen.findByText('sprite')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: ja.inspector.more })).toBeNull();
    expect(screen.queryByRole('button', { name: ja.itemMenu.edit })).toBeNull();
  });
});
