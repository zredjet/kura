import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { setAssetSettings } from '../../ipc/asset';
import { AuditionLoopSheet, checkAuditionLoop } from './AuditionLoopSheet';
import { AUDIO_FACTS, audioAsset, withAuditionLoop } from './testAssets';

vi.mock('../../ipc/asset', () => ({ setAssetSettings: vi.fn(async () => 1) }));

const t = ja.asset;
const R = 44_100;
/** 解析の長さ（100 秒）。 */
const FRAMES = AUDIO_FACTS.frames!;

/** ファイルにループ点のない音声。 */
function plainAsset() {
  const asset = audioAsset();
  return {
    ...asset,
    facts: asset.facts && {
      ...asset.facts,
      audio: { ...AUDIO_FACTS, hasLoop: false, loopRange: null },
    },
  };
}

function open(
  asset = audioAsset(),
  probe: (hash: string) => Promise<number | null> = async () => FRAMES,
) {
  const onClose = vi.fn();
  const onSaved = vi.fn();
  render(<AuditionLoopSheet asset={asset} onClose={onClose} onSaved={onSaved} probe={probe} />);
  return { onClose, onSaved };
}

const choice = (name: string) => screen.getByRole('button', { name });
const save = () => screen.getByRole('button', { name: ja.common.save });

/** 時刻の欄に打って欄を離れる（TimeField は離れたときに読む）。 */
function type(label: string, text: string) {
  const input = screen.getByLabelText(label);
  fireEvent.change(input, { target: { value: text } });
  fireEvent.blur(input);
}

describe('区間の確かめ', () => {
  it('0 ≤ 始め < 終わり ≤ 長さ（長さが分からなければ終わりは見ない）', () => {
    expect(checkAuditionLoop(0, FRAMES, FRAMES, R)).toBeNull();
    expect(checkAuditionLoop(10, 20, null, R)).toBeNull();
    expect(checkAuditionLoop(null, 20, FRAMES, R)).toBe(t.auditionLoopMissing);
    expect(checkAuditionLoop(0, null, FRAMES, R)).toBe(t.auditionLoopMissing);
    expect(checkAuditionLoop(20, 20, FRAMES, R)).toBe(t.auditionLoopOrder);
    expect(checkAuditionLoop(30, 20, FRAMES, R)).toBe(t.auditionLoopOrder);
    expect(checkAuditionLoop(0, FRAMES + 1, FRAMES, R)).toBe(t.auditionLoopPastEnd('1:40.000'));
  });
});

// design.md §3.1「試聴のループのシート」
describe('試聴のループのシート', () => {
  beforeEach(() => vi.clearAllMocks());

  it('見出し・セグメント・一言。設定がなければ「ファイルのループ点」が押されていて、保存しても送らずに閉じる', async () => {
    const { onClose, onSaved } = open();
    expect(screen.getByRole('heading', { name: t.auditionLoopTitle })).toBeInTheDocument();
    expect(screen.getByRole('group', { name: t.auditionLoopGroup })).toBeInTheDocument();
    expect(choice(t.auditionLoopFile)).toHaveAttribute('aria-pressed', 'true');
    expect(choice(t.auditionLoopCustom)).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByText(t.auditionLoopNote)).toBeInTheDocument();
    // ファイルのループ点を見せる（時刻の欄は出さない）
    expect(screen.getByText('0:04.200 – 1:32.000')).toBeInTheDocument();
    expect(screen.queryByLabelText(t.auditionLoopStart)).toBeNull();
    fireEvent.click(save());
    expect(setAssetSettings).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalled();
    expect(onSaved).not.toHaveBeenCalled();
  });

  it('ファイルにループ点がなければ左は「全体」', async () => {
    open(plainAsset());
    expect(choice(t.auditionLoopWhole)).toHaveAttribute('aria-pressed', 'true');
    expect(screen.queryByRole('button', { name: t.auditionLoopFile })).toBeNull();
    await waitFor(() => expect(screen.getByText('0:00.000 – 1:40.000')).toBeInTheDocument());
  });

  it('区間を指定：始め・終わりを m:ss.mmm で出し、標本の位置を添える（初めはファイルのループ点）', () => {
    open();
    fireEvent.click(choice(t.auditionLoopCustom));
    expect(screen.getByLabelText(t.auditionLoopStart)).toHaveValue('0:04.200');
    expect(screen.getByLabelText(t.auditionLoopEnd)).toHaveValue('1:32.000');
    expect(screen.getByText(ja.editor.audio.samples('185,220'))).toBeInTheDocument();
    expect(screen.getByText(ja.editor.audio.samples('4,057,200'))).toBeInTheDocument();
  });

  it('ループ点のない音声の区間の初めは全体（長さは /pcm の応答で知る）', async () => {
    open(plainAsset(), async () => R * 3);
    fireEvent.click(choice(t.auditionLoopCustom));
    expect(screen.getByLabelText(t.auditionLoopStart)).toHaveValue('0:00.000');
    await waitFor(() => expect(screen.getByLabelText(t.auditionLoopEnd)).toHaveValue('0:03.000'));
    expect(screen.getByText(t.auditionLoopLength('0:03.000'))).toBeInTheDocument();
  });

  it('時刻を入れると標本の位置が変わり、保存で区間を送って閉じ、読み直す', async () => {
    const { onClose, onSaved } = open();
    fireEvent.click(choice(t.auditionLoopCustom));
    type(t.auditionLoopStart, '0:10');
    type(t.auditionLoopEnd, '1:20.5');
    expect(screen.getByText(ja.editor.audio.samples('441,000'))).toBeInTheDocument();
    expect(screen.getByText(ja.editor.audio.samples('3,550,050'))).toBeInTheDocument();
    fireEvent.click(save());
    expect(setAssetSettings).toHaveBeenCalledWith([2], {
      loopRange: { set: { start: 441_000, end: 3_550_050 } },
    });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(onSaved).toHaveBeenCalled();
  });

  it('始め ≥ 終わり・長さを超える終わりは理由を出し、保存を押せない', async () => {
    open();
    await act(async () => undefined);
    fireEvent.click(choice(t.auditionLoopCustom));
    type(t.auditionLoopStart, '1:32');
    expect(screen.getByRole('alert')).toHaveTextContent(t.auditionLoopOrder);
    expect(save()).toBeDisabled();
    type(t.auditionLoopStart, '0:00');
    type(t.auditionLoopEnd, '1:41');
    expect(screen.getByRole('alert')).toHaveTextContent(t.auditionLoopPastEnd('1:40.000'));
    expect(save()).toBeDisabled();
    type(t.auditionLoopEnd, '1:40');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(save()).toBeEnabled();
  });

  it('画面に出ている長さをそのまま終わりに打つと、ちょうど長さになる（ミリ秒への丸めで長さを超えて断られない）', async () => {
    // 4,410,030 フレームは 100.00068 秒で「1:40.001」と出る。打つと 4,410,044 と読むが、同じ時刻の文字なので長さに寄せる
    const length = 4_410_030;
    open(plainAsset(), async () => length);
    fireEvent.click(choice(t.auditionLoopCustom));
    await waitFor(() => expect(screen.getByLabelText(t.auditionLoopEnd)).toHaveValue('1:40.001'));
    type(t.auditionLoopEnd, '0:50');
    expect(screen.getByLabelText(t.auditionLoopEnd)).toHaveValue('0:50.000');
    type(t.auditionLoopEnd, '1:40.001');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText(ja.editor.audio.samples('4,410,030'))).toBeInTheDocument();
    fireEvent.click(save());
    expect(setAssetSettings).toHaveBeenCalledWith([2], {
      loopRange: { set: { start: 0, end: length } },
    });
  });

  it('切り下がって見える長さを打っても長さになり、違う時刻は今どおり（長くても短くても）そのまま読む', async () => {
    // 4,410,020 フレームは 100.00045 秒で「1:40.000」と出る。そのまま読むと 20 フレーム足りない
    open(plainAsset(), async () => 4_410_020);
    fireEvent.click(choice(t.auditionLoopCustom));
    await waitFor(() => expect(screen.getByLabelText(t.auditionLoopEnd)).toHaveValue('1:40.000'));
    type(t.auditionLoopEnd, '0:50');
    type(t.auditionLoopEnd, '1:40');
    expect(screen.getByText(ja.editor.audio.samples('4,410,020'))).toBeInTheDocument();
    // 1 ミリ秒でも違えば寄せない
    type(t.auditionLoopEnd, '1:40.001');
    expect(screen.getByRole('alert')).toHaveTextContent(t.auditionLoopPastEnd('1:40.000'));
    expect(save()).toBeDisabled();
    type(t.auditionLoopEnd, '1:39.999');
    expect(screen.getByText(ja.editor.audio.samples('4,409,956'))).toBeInTheDocument();
    expect(save()).toBeEnabled();
  });

  it('長さは /pcm の応答を正とする（解析の長さより短ければ、応答の長さで確かめる）', async () => {
    const probe = vi.fn(async () => R * 60);
    open(audioAsset(), probe);
    expect(probe).toHaveBeenCalledWith('cd'.repeat(32));
    fireEvent.click(choice(t.auditionLoopCustom));
    await waitFor(() =>
      expect(screen.getByRole('alert')).toHaveTextContent(t.auditionLoopPastEnd('1:00.000')),
    );
    expect(save()).toBeDisabled();
  });

  it('応答を読めなければ解析の長さで確かめる', async () => {
    open(audioAsset(), async () => {
      throw new Error('503');
    });
    fireEvent.click(choice(t.auditionLoopCustom));
    await act(async () => undefined);
    expect(screen.getByText(t.auditionLoopLength('1:40.000'))).toBeInTheDocument();
  });

  it('設定があれば「区間を指定」でその区間。「ファイルのループ点」で保存すると設定を消す', async () => {
    const { onClose, onSaved } = open(withAuditionLoop(audioAsset(), R, R * 2));
    expect(choice(t.auditionLoopCustom)).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByLabelText(t.auditionLoopStart)).toHaveValue('0:01.000');
    expect(screen.getByLabelText(t.auditionLoopEnd)).toHaveValue('0:02.000');
    fireEvent.click(choice(t.auditionLoopFile));
    fireEvent.click(save());
    expect(setAssetSettings).toHaveBeenCalledWith([2], { loopRange: 'clear' });
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(onSaved).toHaveBeenCalled();
  });

  it('キャンセルは何も送らずに閉じる', () => {
    const { onClose } = open(withAuditionLoop(audioAsset(), R, R * 2));
    type(t.auditionLoopStart, '0:00.5');
    fireEvent.click(screen.getByRole('button', { name: ja.common.cancel }));
    expect(onClose).toHaveBeenCalled();
    expect(setAssetSettings).not.toHaveBeenCalled();
  });

  it('保存に失敗したら理由を出して閉じない', async () => {
    vi.mocked(setAssetSettings).mockRejectedValueOnce(new Error('長さを超えています'));
    const { onClose } = open(withAuditionLoop(audioAsset(), R, R * 2));
    fireEvent.click(save());
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('長さを超えています'));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('保存に失敗したあと時刻の欄を変えたら前の理由を消し、今の状態を出す', async () => {
    vi.mocked(setAssetSettings).mockRejectedValueOnce(new Error('長さを超えています'));
    open(withAuditionLoop(audioAsset(), R, R * 2));
    await act(async () => undefined);
    fireEvent.click(save());
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('長さを超えています'));

    // 問題のない値：理由を消して長さを出す
    type(t.auditionLoopStart, '0:00.5');
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText(t.auditionLoopLength('1:40.000'))).toBeInTheDocument();
    expect(save()).toBeEnabled();

    // もう一度断られたあと、終わりの欄で問題のある値にする：今の理由を出す
    vi.mocked(setAssetSettings).mockRejectedValueOnce(new Error('長さを超えています'));
    fireEvent.click(save());
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('長さを超えています'));
    type(t.auditionLoopEnd, '0:00.1');
    expect(screen.getByRole('alert')).toHaveTextContent(t.auditionLoopOrder);
    expect(save()).toBeDisabled();
  });

  it('断られたあと、打ち直しても値が変わらなければ理由は残る', async () => {
    vi.mocked(setAssetSettings).mockRejectedValueOnce(new Error('長さを超えています'));
    open(withAuditionLoop(audioAsset(), R, R * 2));
    await act(async () => undefined);
    fireEvent.click(save());
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('長さを超えています'));
    type(t.auditionLoopStart, '0:01');
    expect(screen.getByRole('alert')).toHaveTextContent('長さを超えています');
  });
});
