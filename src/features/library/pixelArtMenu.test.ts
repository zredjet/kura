import { beforeEach, describe, expect, it, vi } from 'vitest';

import { isSubmenu, type MenuEntry, type MenuSubmenu } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { setAssetSettings } from '../../ipc/asset';
import { type ItemSummaryDto, unitEntry } from '../../ipc/items';
import { getCached, invalidateItems, invalidateUnits } from '../../stores/itemCache';
import { notify } from '../../stores/noticeStore';
import { useViewStore } from '../../stores/viewStore';
import { itemMenu } from './itemMenu';
import { applyPixelArtToSelection, pixelArtMenu, pixelArtMenuState } from './pixelArtMenu';
import { EMPTY_SELECTION } from './selection';

vi.mock('../../ipc/asset', () => ({ setAssetSettings: vi.fn(async () => 1) }));
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
  vi.mocked(setAssetSettings).mockClear();
  vi.mocked(invalidateItems).mockClear();
  vi.mocked(invalidateUnits).mockClear();
  vi.mocked(notify).mockClear();
  useViewStore.setState({ view: { kind: 'all' }, selection: EMPTY_SELECTION });
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
    expect(setAssetSettings).toHaveBeenCalledWith([1, 3], { pixelArt: 'yes' });
    expect(invalidateItems).toHaveBeenCalledWith([1, 3]);
    expect(invalidateUnits).toHaveBeenCalled();

    expect(pixelArtMenu([2, unitEntry(5)])).toBeNull();
    expect(pixelArtMenu([99])).toBeNull();
  });

  it('変えられなければ知らせる', async () => {
    cache.set(1, item(1, {}));
    vi.mocked(setAssetSettings).mockRejectedValueOnce(new Error('だめでした'));
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

describe('メニューバーの「項目 › ドット絵として表示 ▸」', () => {
  /** 一覧で選ぶ。 */
  function select(ids: number[]) {
    useViewStore.setState({
      selection: { ids, anchor: ids[0] ?? null, focus: ids[ids.length - 1] ?? null },
    });
  }

  it('一覧で選んでいる画像があれば使え、設定がそろっていればその値にチェック', () => {
    cache.set(1, item(1, { pixelArtSetting: true }));
    cache.set(2, item(2, { kind: 'audio', ext: 'wav' }));
    cache.set(3, item(3, { pixelArtSetting: true }));
    cache.set(4, item(4, { pixelArtSetting: null }));
    select([1, 2, unitEntry(5), 3]);
    expect(pixelArtMenuState()).toEqual({ enabled: true, current: 'yes' });
    select([1, 4]);
    expect(pixelArtMenuState()).toEqual({ enabled: true, current: null });
    select([4]);
    expect(pixelArtMenuState()).toEqual({ enabled: true, current: 'auto' });
  });

  it('画像を選んでいない・ゴミ箱を開いているときは使えない', () => {
    cache.set(1, item(1, {}));
    cache.set(2, item(2, { kind: 'video', ext: 'mp4' }));
    expect(pixelArtMenuState()).toEqual({ enabled: false, current: null });
    select([2, unitEntry(5)]);
    expect(pixelArtMenuState()).toEqual({ enabled: false, current: null });
    select([1]);
    useViewStore.setState({ view: { kind: 'trash' } });
    expect(pixelArtMenuState()).toEqual({ enabled: false, current: null });
  });

  it('選んでいる画像（複数）に右クリックと同じく当て、読み直す', async () => {
    cache.set(1, item(1, {}));
    cache.set(2, item(2, { kind: 'audio', ext: 'wav' }));
    cache.set(3, item(3, { pixelArtSetting: true }));
    select([3, 2, 1]);
    applyPixelArtToSelection('no');
    await vi.waitFor(() => expect(invalidateItems).toHaveBeenCalled());
    expect(setAssetSettings).toHaveBeenCalledWith([3, 1], { pixelArt: 'no' });
    expect(invalidateItems).toHaveBeenCalledWith([3, 1]);
    expect(invalidateUnits).toHaveBeenCalled();
  });

  it('画像を選んでいなければ何もしない（ゴミ箱でも）', () => {
    cache.set(1, item(1, {}));
    cache.set(2, item(2, { kind: 'audio', ext: 'wav' }));
    select([2]);
    applyPixelArtToSelection('yes');
    select([1]);
    useViewStore.setState({ view: { kind: 'trash' } });
    applyPixelArtToSelection('yes');
    expect(setAssetSettings).not.toHaveBeenCalled();
  });

  it('キャッシュにない（見えていない）ものも送り、そのときはチェックを付けない（⌘A で選んだとき）', async () => {
    cache.set(1, item(1, { pixelArtSetting: false }));
    cache.set(2, item(2, { kind: 'audio', ext: 'wav' }));
    select([1, 2, 77, unitEntry(5)]);
    expect(pixelArtMenuState()).toEqual({ enabled: true, current: null });
    expect(pixelArtMenu([1, 2, 77])?.submenu.map((c) => c.checked)).toEqual([false, false, false]);
    applyPixelArtToSelection('yes');
    await vi.waitFor(() => expect(invalidateItems).toHaveBeenCalled());
    // 画像でないものは Rust の item_asset_settings_set が飛ばす
    expect(setAssetSettings).toHaveBeenCalledWith([1, 77], { pixelArt: 'yes' });
  });
});
