/** テストの見本（素材の欄・試聴・効果音のテストで共有する。本番のコードからは読まない）。 */

import type {
  AssetAudioFactsDto,
  AssetDto,
  AssetFactsDto,
  AssetImageFactsDto,
} from '../../ipc/asset';
import type { ItemSummaryDto } from '../../ipc/items';

export const IMAGE_FACTS: AssetImageFactsDto = {
  alpha: 'partial',
  colors: 257,
  visibleColors: 300,
  pixelKx: 4,
  pixelKy: 4,
  pixelLossless: true,
  pixelArt: true,
  pixelRule: 1,
  frameCount: 8,
};

export const AUDIO_FACTS: AssetAudioFactsDto = {
  loudness: -14.24,
  loudnessReason: null,
  loudnessRange: 6.08,
  truePeak: -1.02,
  samplePeak: -1.2,
  hasLoop: true,
  loopRange: { start: 185_220, end: 4_057_200 },
  sampleRate: 44_100,
  channels: 2,
  bits: 24,
  clipRuns: 0,
};

const NO_IMAGE: AssetImageFactsDto = {
  alpha: null,
  colors: null,
  visibleColors: null,
  pixelKx: null,
  pixelKy: null,
  pixelLossless: null,
  pixelArt: null,
  pixelRule: null,
  frameCount: null,
};

const NO_AUDIO: AssetAudioFactsDto = {
  loudness: null,
  loudnessReason: null,
  loudnessRange: null,
  truePeak: null,
  samplePeak: null,
  hasLoop: null,
  loopRange: null,
  sampleRate: null,
  channels: null,
  bits: null,
  clipRuns: null,
};

function facts(patch: Partial<AssetFactsDto>): AssetFactsDto {
  return {
    ok: true,
    errorCode: null,
    decoder: 'png-rs-dec/1',
    factsVersion: 1,
    analyzedAt: 1_700_000_000_000,
    image: NO_IMAGE,
    audio: NO_AUDIO,
    ...patch,
  };
}

const BASE: AssetDto = {
  itemId: 1,
  hash: 'ab'.repeat(32),
  kind: 'image',
  ext: 'png',
  width: 64,
  height: 64,
  analysisState: 'ready',
  facts: null,
  settings: {
    pixelArt: null,
    pixelScale: null,
    grid: null,
    frameMs: null,
    pivot: null,
    loopRange: null,
    animTag: null,
    updatedAt: null,
  },
  effective: { pixelArt: true, animated: false, frameCount: 1 },
  font: null,
  model: null,
};

/** 画像の素材の欄（半透明・256 色より多い・自動でドット絵 4 倍・8 コマ）。 */
export function imageAsset(patch: Partial<AssetDto> = {}): AssetDto {
  return {
    ...BASE,
    facts: facts({ image: IMAGE_FACTS }),
    effective: { pixelArt: true, animated: true, frameCount: 8 },
    ...patch,
  };
}

/** 音声の素材の欄（44.1kHz・24 ビット・ステレオ・ループあり）。 */
export function audioAsset(patch: Partial<AssetDto> = {}): AssetDto {
  return {
    ...BASE,
    itemId: 2,
    hash: 'cd'.repeat(32),
    kind: 'audio',
    ext: 'wav',
    width: null,
    height: null,
    facts: facts({ decoder: 'wav-kura-dec/1', audio: AUDIO_FACTS }),
    effective: { pixelArt: false, animated: false, frameCount: null },
    ...patch,
  };
}

/** 一覧の概要（素材の欄のテストで使う所だけ）。 */
export function summary(patch: Partial<ItemSummaryDto> = {}): ItemSummaryDto {
  return {
    id: 1,
    hash: 'ab'.repeat(32),
    ext: 'png',
    kind: 'image',
    width: 64,
    height: 64,
    durationMs: null,
    sizeBytes: 1000,
    title: 'sprite',
    rating: 0,
    sourceSite: null,
    importedAt: 0,
    capturedAt: null,
    thumbState: 'ready',
    proxyState: 'not_needed',
    lastPlayedAt: null,
    ...patch,
  } as ItemSummaryDto;
}
