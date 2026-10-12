import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { Overlays } from '../../components/Overlay/Overlays';
import { ja } from '../../i18n/ja';
import {
  type DetectionDto,
  type DownloadOptionsDto,
  detectDownload,
  enqueueDownload,
} from '../../ipc/downloads';
import { AddPanel } from './AddPanel';

const options: DownloadOptionsDto = {
  quality: 'best',
  audioOnly: false,
  destFolderId: null,
  tagUploader: true,
  tagSiteTags: true,
  playlistUnit: true,
};

const detection = (url: string, previous: DetectionDto['previous']): DetectionDto => ({
  url,
  site: 'youtube',
  siteLabel: 'YouTube',
  tool: 'ytDlp',
  rule: null,
  playlist: false,
  toolFound: true,
  capture: null,
  previous,
});

vi.mock('../../ipc/downloads', () => ({
  detectDownload: vi.fn(),
  downloadDefaults: vi.fn(async () => options),
  enqueueDownload: vi.fn(async () => 7),
  listDownloads: vi.fn(async () => []),
  toolCommand: () => 'yt-dlp',
}));
vi.mock('../../ipc/capture', () => ({ runCapture: vi.fn() }));

const t = ja.downloads;
const URL = 'https://www.youtube.com/watch?v=vid1';

/** URL を入れて、判定が終わってから「追加」を押す。 */
async function add() {
  render(
    <>
      <AddPanel />
      <Overlays />
    </>,
  );
  const input = screen.getByRole('textbox', { name: t.urlLabel });
  fireEvent.change(input, { target: { value: URL } });
  await screen.findByText(new RegExp(t.detected('YouTube')));
  const button = screen.getByRole('button', { name: t.add });
  await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
  await act(async () => {
    fireEvent.click(button);
  });
  return input as HTMLInputElement;
}

describe('AddPanel', () => {
  beforeEach(() => {
    vi.mocked(enqueueDownload).mockClear();
  });

  it('adds a new URL without asking', async () => {
    vi.mocked(detectDownload).mockImplementation(async (url) => detection(url, null));
    await add();
    await waitFor(() => expect(enqueueDownload).toHaveBeenCalledWith(URL, options, false));
    expect(screen.queryByText(t.previous.title)).toBeNull();
  });

  it('offers to download again when the URL was downloaded before', async () => {
    vi.mocked(detectDownload).mockImplementation(async (url) =>
      detection(url, { jobId: 3, title: '動画', at: Date.now(), items: 1, inLibrary: 0 }),
    );
    const input = await add();
    expect(await screen.findByText(t.previous.title)).toBeTruthy();
    expect(screen.getByText(/「動画」は .* にダウンロードしています。/)).toBeTruthy();
    expect(enqueueDownload).not.toHaveBeenCalled();

    // キャンセルなら積まず、URL は残す
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: ja.common.cancel }));
    });
    expect(screen.queryByText(t.previous.title)).toBeNull();
    expect(enqueueDownload).not.toHaveBeenCalled();
    expect(input.value).toBe(URL);

    // 再ダウンロード
    const button = screen.getByRole('button', { name: t.add });
    await waitFor(() => expect((button as HTMLButtonElement).disabled).toBe(false));
    await act(async () => {
      fireEvent.click(button);
    });
    await act(async () => {
      fireEvent.click(await screen.findByRole('button', { name: t.previous.redownload }));
    });
    await waitFor(() => expect(enqueueDownload).toHaveBeenCalledWith(URL, options, true));
    await waitFor(() => expect(input.value).toBe(''));
  });

  it('can add it as usual and skip what was downloaded', async () => {
    vi.mocked(detectDownload).mockImplementation(async (url) =>
      detection(url, { jobId: 3, title: null, at: Date.now(), items: 0, inLibrary: 0 }),
    );
    await add();
    await act(async () => {
      fireEvent.click(await screen.findByRole('button', { name: t.previous.skip }));
    });
    await waitFor(() => expect(enqueueDownload).toHaveBeenCalledWith(URL, options, false));
  });
});
