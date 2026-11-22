import { render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { getAsset } from '../../ipc/asset';
import { AUDIO_FACTS, audioAsset, summary, withAuditionLoop } from '../asset/testAssets';
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

  it('ループのない音声は区間なし（読むまでも区間なし）', async () => {
    const plain = audioAsset();
    const facts = plain.facts && { ...plain.facts, audio: { ...AUDIO_FACTS, loopRange: null } };
    vi.mocked(getAsset).mockResolvedValue({ ...plain, facts });
    render(<AudioArt item={AUDIO} />);
    expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '');
    await waitFor(() => expect(getAsset).toHaveBeenCalledWith(2));
    expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '');
  });

  // M13：試聴のループ（素材の設定）は、ファイルにループ点がなくても先に使う（概要の hasLoop はファイルのループ点なので
  // 見ずに素材の欄を読む）
  it('試聴の設定があれば、ファイルのループ点の代わりにその区間を渡す', async () => {
    const plain = audioAsset();
    const facts = plain.facts && { ...plain.facts, audio: { ...AUDIO_FACTS, loopRange: null } };
    vi.mocked(getAsset).mockResolvedValue(withAuditionLoop({ ...plain, facts }, 100, 200));
    render(<AudioArt item={AUDIO} />);
    await waitFor(() =>
      expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '100-200'),
    );
  });

  it('試聴の設定が長さを超えれば、ファイルのループ点を渡す', async () => {
    vi.mocked(getAsset).mockResolvedValue(
      withAuditionLoop(audioAsset(), 0, AUDIO_FACTS.frames! + 1),
    );
    render(<AudioArt item={{ ...AUDIO, hasLoop: true }} />);
    await waitFor(() =>
      expect(screen.getByTestId('waveform')).toHaveAttribute('data-loop', '185220-4057200'),
    );
  });
});
