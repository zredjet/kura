import { beforeEach, describe, expect, it, vi } from 'vitest';

import { isSubmenu, type MenuEntry, type MenuSubmenu } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { setPixelArt } from '../../ipc/assetSettings';
import { type ItemSummaryDto, unitEntry } from '../../ipc/items';
import { getCached, invalidateItems, invalidateUnits } from '../../stores/itemCache';
import { notify } from '../../stores/noticeStore';
import { itemMenu } from './itemMenu';
import { pixelArtMenu } from './pixelArtMenu';

vi.mock('../../ipc/assetSettings', () => ({ setPixelArt: vi.fn(async () => 1) }));
vi.mock('../../stores/noticeStore', () => ({ notify: vi.fn() }));
vi.mock('../../stores/itemCache', () => ({
  getCached: vi.fn(),
  getCachedUnit: vi.fn(),
  invalidateItems: vi.fn(),
  invalidateUnits: vi.fn(),
}));

const t = ja.mediaView;

function item(id: number, over: Partial<ItemSummaryDto>): ItemSummaryDto {
  return {
    id,
    hash: `h${id}`,
    ext: 'png',
    kind: 'image',
    width: 16,
    height: 16,
    durationMs: null,
    sizeBytes: 1,
    title: `t${id}`,
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

const cache = new Map<number, ItemSummaryDto>();

beforeEach(() => {
  cache.clear();
  vi.mocked(getCached).mockImplementation((id) => cache.get(id));
  vi.mocked(setPixelArt).mockClear();
  vi.mocked(invalidateItems).mockClear();
  vi.mocked(invalidateUnits).mockClear();
  vi.mocked(notify).mockClear();
});

/** 選択肢のラベルとチェック。 */
function checks(menu: MenuSubmenu | null) {
  return menu?.submenu.map((c) => [c.label, c.checked]);
}

describe('ドット絵として表示 ▸', () => {
  it('画像の設定がそろっていればその値にチェック、ばらばらならチェックなし', () => {
    cache.set(1, item(1, { pixelArtSetting: true }));
    cache.set(2, item(2, { pixelArtSetting: true }));
    cache.set(3, item(3, { pixelArtSetting: null }));
    cache.set(4, item(4, { pixelArtSetting: false }));
    expect(checks(pixelArtMenu([1, 2]))).toEqual([
      [t.pixelArtAuto, false],
      [t.pixelArtYes, true],
      [t.pixelArtNo, false],
    ]);
    expect(checks(pixelArtMenu([3]))).toEqual([
      [t.pixelArtAuto, true],
      [t.pixelArtYes, false],
      [t.pixelArtNo, false],
    ]);
    expect(pixelArtMenu([4])?.submenu.map((c) => c.checked)).toEqual([false, false, true]);
    expect(pixelArtMenu([1, 3, 4])?.submenu.map((c) => c.checked)).toEqual([false, false, false]);
  });

  it('画像だけに付ける（ほかの種類・ユニットのタイルは飛ばす）。画像がなければ出さない', async () => {
    cache.set(1, item(1, {}));
    cache.set(2, item(2, { kind: 'audio', ext: 'wav' }));
    cache.set(3, item(3, { pixelArtSetting: false }));
    const menu = pixelArtMenu([1, 2, unitEntry(5), 3]);
    expect(menu?.label).toBe(t.pixelArt);
    menu?.submenu[1]?.onSelect();
    await vi.waitFor(() => expect(invalidateItems).toHaveBeenCalled());
    expect(setPixelArt).toHaveBeenCalledWith([1, 3], 'yes');
    expect(invalidateItems).toHaveBeenCalledWith([1, 3]);
    expect(invalidateUnits).toHaveBeenCalled();

    expect(pixelArtMenu([2, unitEntry(5)])).toBeNull();
    expect(pixelArtMenu([99])).toBeNull();
  });

  it('変えられなければ知らせる', async () => {
    cache.set(1, item(1, {}));
    vi.mocked(setPixelArt).mockRejectedValueOnce(new Error('だめでした'));
    pixelArtMenu([1])?.submenu[0]?.onSelect();
    await vi.waitFor(() => expect(notify).toHaveBeenCalled());
    expect(vi.mocked(notify).mock.calls[0]?.[0]).toContain('だめでした');
    expect(invalidateItems).not.toHaveBeenCalled();
  });

  it('右クリックのメニューに、画像を選んでいるときだけ出る', () => {
    cache.set(1, item(1, {}));
    cache.set(2, item(2, { kind: 'video', ext: 'mp4' }));
    const find = (entries: MenuEntry[]) =>
      entries.find((e): e is MenuSubmenu => isSubmenu(e) && e.label === t.pixelArt);
    expect(find(itemMenu([1, 2]))?.submenu).toHaveLength(3);
    expect(find(itemMenu([2]))).toBeUndefined();
  });
});
