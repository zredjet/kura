import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { ItemSummaryDto } from '../../ipc/items';
import { audioRuntime } from '../asset/audioRuntime';
import { stopAudition } from '../asset/auditionStore';
import { openEditor } from './editorKinds';
import { openEditorAs } from './editorStore';

const sfxStop = vi.hoisted(() => vi.fn());

vi.mock('../asset/auditionStore', () => ({ stopAudition: vi.fn() }));
vi.mock('../asset/audioRuntime', () => ({
  audioRuntime: vi.fn(() => ({ sfx: { stop: sfxStop } })),
}));
vi.mock('./editorStore', () => ({ openEditorAs: vi.fn(async () => {}) }));
vi.mock('./image/imageKind', () => ({ IMAGE_EDITOR: { adapter: 'image-adapter' } }));
vi.mock('./audio/audioKind', () => ({ AUDIO_EDITOR: { adapter: 'audio-adapter' } }));

const item = (kind: ItemSummaryDto['kind']) => ({ id: 3, kind }) as ItemSummaryDto;

describe('openEditor（開く口。M13 の段 6 の G9）', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it('音声は、素材の試聴と効果音を止めてから音声のエディタで開く', async () => {
    await openEditor(item('audio'), { fromDraft: true });
    expect(stopAudition).toHaveBeenCalledOnce();
    expect(audioRuntime).toHaveBeenCalled();
    expect(sfxStop).toHaveBeenCalledOnce();
    expect(openEditorAs).toHaveBeenCalledWith('audio-adapter', item('audio'), { fromDraft: true });
    const [stopOrder] = vi.mocked(stopAudition).mock.invocationCallOrder;
    const [openOrder] = vi.mocked(openEditorAs).mock.invocationCallOrder;
    expect(stopOrder).toBeLessThan(openOrder ?? 0);
  });

  it('画像は試聴に触れずに画像のエディタで開き、開けない種類は何もしない', async () => {
    await openEditor(item('image'));
    expect(openEditorAs).toHaveBeenCalledWith('image-adapter', item('image'), {});
    expect(stopAudition).not.toHaveBeenCalled();
    expect(sfxStop).not.toHaveBeenCalled();
    vi.mocked(openEditorAs).mockClear();
    await openEditor(item('video'));
    await openEditor(item('font'));
    expect(openEditorAs).not.toHaveBeenCalled();
  });
});
