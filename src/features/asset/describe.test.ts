import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import type { AnimDto } from '../../ipc/asset';
import {
  alphaText,
  animText,
  audioFormatText,
  clockText,
  colorsText,
  fontFormatText,
  gridText,
  khz,
  loopTexts,
  loudnessText,
  MINUS,
  modelSizeText,
  peakText,
  pixelAutoText,
  secondsText,
  signed,
  sizeText,
  unsupportedText,
} from './describe';
import { AUDIO_FACTS, IMAGE_FACTS } from './testAssets';

const t = ja.asset;

const IMAGE = IMAGE_FACTS;
const AUDIO = AUDIO_FACTS;

// design.md §3.1「素材の欄」の書き方
describe('素材の欄の値', () => {
  it('負の数は U+2212 で書き、丸めて 0 なら符号を付けない', () => {
    expect(MINUS).toBe('−');
    expect(signed(-14.24, 1)).toBe('−14.2');
    expect(signed(6.08, 1)).toBe('6.1');
    expect(signed(-0.04, 1)).toBe('0.0');
    expect(signed(0, 1)).toBe('0.0');
  });

  it('大きさは 2 のべき乗を添え、分からなければ —', () => {
    expect(sizeText(64, 64)).toBe(`64 × 64${t.powerOfTwo}`);
    expect(sizeText(1920, 1080)).toBe('1,920 × 1,080');
    expect(sizeText(null, 10)).toBe('—');
    expect(sizeText(0, 0)).toBe('—');
  });

  it('透過・色数（257 は 256 色より多い、計らない JPEG は —）', () => {
    expect(alphaText(IMAGE)).toBe('半透明あり');
    expect(alphaText({ ...IMAGE, alpha: 'binary' })).toBe('あり（二値）');
    expect(alphaText({ ...IMAGE, alpha: 'none' })).toBe('なし');
    expect(alphaText({ ...IMAGE, alpha: null })).toBe('—');
    expect(colorsText(IMAGE)).toBe('256 色より多い');
    expect(colorsText({ ...IMAGE, colors: 16 })).toBe('16 色');
    expect(colorsText({ ...IMAGE, colors: 256 })).toBe('256 色');
    expect(colorsText({ ...IMAGE, colors: null })).toBe('—');
  });

  it('ドット絵の自動の判定（倍率を添える）', () => {
    expect(pixelAutoText(IMAGE)).toBe('自動の判定：はい（4 倍）');
    expect(pixelAutoText({ ...IMAGE, pixelKy: 2 })).toBe('自動の判定：はい（4 × 2 倍）');
    expect(pixelAutoText({ ...IMAGE, pixelKx: 1, pixelKy: 1 })).toBe('自動の判定：はい（等倍）');
    expect(pixelAutoText({ ...IMAGE, pixelArt: false })).toBe('自動の判定：いいえ');
    expect(pixelAutoText({ ...IMAGE, pixelArt: null })).toBe('自動の判定：まだ判定していません');
    expect(pixelAutoText(undefined)).toBe('自動の判定：まだ判定していません');
  });

  it('アニメは帯があれば 1 周の秒も（0ms のコマは 100ms）', () => {
    const anim = { frameCount: 8, durations: [100, 100, 100, 100, 100, 100, 100, 0] } as AnimDto;
    expect(animText(8, anim)).toBe('8 コマ · 0.8 秒');
    expect(animText(8, null)).toBe('8 コマ');
    expect(animText(1, null)).toBe('なし');
    expect(animText(null, null)).toBe('—');
    expect(secondsText(1250)).toBe('1.25');
    expect(secondsText(65_000)).toBe('65');
  });

  it('格子「16 × 16 · 12 コマ」、なければ「なし」', () => {
    const grid = { cellW: 16, cellH: 16, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0, count: 12 };
    expect(gridText(grid, 16)).toBe('16 × 16 · 12 コマ');
    expect(gridText({ ...grid, count: null }, 16)).toBe('16 × 16 · 16 コマ');
    expect(gridText(null, 8)).toBe('なし');
  });

  it('音声の形式「44.1 kHz · 24 ビット · ステレオ」（非可逆はビットなし）', () => {
    expect(khz(44_100)).toBe('44.1');
    expect(khz(48_000)).toBe('48');
    expect(khz(22_050)).toBe('22.05');
    expect(audioFormatText(AUDIO)).toBe('44.1 kHz · 24 ビット · ステレオ');
    expect(audioFormatText({ ...AUDIO, bits: null, channels: 1 })).toBe('44.1 kHz · モノラル');
    expect(audioFormatText({ ...AUDIO, channels: 6 })).toBe('44.1 kHz · 24 ビット · 6 チャンネル');
    expect(audioFormatText(undefined)).toBe('—');
  });

  it('ラウドネス・ピーク（計れなければ理由）', () => {
    expect(loudnessText(AUDIO)).toBe('−14.2 LUFS（LRA 6.1 LU）');
    expect(loudnessText({ ...AUDIO, loudnessRange: null })).toBe('−14.2 LUFS');
    expect(loudnessText({ ...AUDIO, loudness: null, loudnessReason: 'tooShort' })).toBe(
      '短くて計れません',
    );
    expect(loudnessText({ ...AUDIO, loudness: null, loudnessReason: 'odd' })).toBe('計れません');
    expect(loudnessText({ ...AUDIO, loudness: null, loudnessReason: null })).toBe('—');
    expect(peakText(AUDIO)).toBe('−1.0 dBTP');
    expect(peakText({ ...AUDIO, truePeak: null })).toBe('—');
  });

  it('ループの時刻と標本の位置', () => {
    expect(clockText(185_220, 44_100)).toBe('0:04.200');
    expect(clockText(4_057_200, 44_100)).toBe('1:32.000');
    expect(clockText(44_100 * 3723.5, 44_100)).toBe('1:02:03.500');
    expect(loopTexts(AUDIO)).toEqual({
      time: '0:04.200 – 1:32.000',
      frames: '185,220 – 4,057,200',
    });
    expect(loopTexts({ ...AUDIO, loopRange: null })).toBeNull();
  });

  it('フォントの形式・3D の大きさ・描けない拡張', () => {
    expect(fontFormatText(null, 'otf')).toBe('OpenType（CFF）');
    expect(
      fontFormatText(
        {
          family: 'A',
          style: null,
          fullName: null,
          postscriptName: null,
          glyphs: 1,
          faces: 1,
          variable: false,
          format: 'woff2',
        },
        'woff2',
      ),
    ).toBe('WOFF2');
    expect(fontFormatText(null, 'xyz')).toBe('XYZ');
    expect(modelSizeText([1.2, 0.8, 0.45])).toBe('1.20 × 0.80 × 0.45');
    expect(modelSizeText([1, null, 2])).toBe('1.00 × — × 2.00');
    expect(modelSizeText(null)).toBe('—');
    expect(unsupportedText([])).toBeNull();
    expect(unsupportedText(['KHR_draco_mesh_compression', 'skin'])).toBe(
      '未対応：Draco 圧縮・スキン',
    );
    expect(unsupportedText(['EXT_foo'])).toBe('未対応：EXT_foo');
  });
});
