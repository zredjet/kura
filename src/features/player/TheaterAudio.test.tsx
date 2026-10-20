import { render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { getAsset } from '../../ipc/asset';
import { audioAsset, summary } from '../asset/testAssets';
import { AudioArt } from './Theater';

vi.mock('../../ipc/asset', () => ({ getAsset: vi.fn() }));
vi.mock('../asset/WaveformView', () => ({
  WaveformView: (p: { hash: string; loop: readonly [number, number] | null }) => (
    <div data-testid="waveform" data-hash={p.hash} data-loop={p.loop?.join('-') ?? ''} />
  ),
}));

const AUDIO = summary({
  id: 2,
  hash: 'cd'.repeat(32),
  kind: 'audio',
  ext: 'wav',
  width: null,
  height: null,
});

beforeEach(() => {
  vi.mocked(getAsset).mockReset();
});

// design.md §3.1「波形」：シアターの音声の波形にも、インスペクタと同じループの区間を出す
describe('シアターの音声の波形（M11）', () => {
  it('ループ点のある音声は、波形にループの区間を渡す', async () => {
    vi.mocked(getAsset).mockResolvedValue(audioAsset());
    render(<AudioArt item={{ ...AUDIO, hasLoop: true }} />);
    await waitFor(() =>
      expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '185220-4057200'),
    );
    expect(getAsset).toHaveBeenCalledWith(2);
  });

  it('ループのない音声は区間なしで、素材の欄を読まない', () => {
    render(<AudioArt item={AUDIO} />);
    expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '');
    expect(getAsset).not.toHaveBeenCalled();
  });
});
