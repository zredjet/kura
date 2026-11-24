/**
 * 音声エディタのモジュール（核のアダプタの作り方・画面の部品・キー。`../editorKinds.ts` の表に載せる。M13 の段 6）。
 * 利用者が開ける種類（`kindTraits.ts` の `OPENABLE`）には G9 で足す。
 */

import type { EditorKindModule } from '../editorKinds';
import {
  AudioCapsule,
  AudioMeta,
  AudioOps,
  AudioStatus,
  AudioTopTools,
  FormatLosses,
} from './AudioParts';
import { AUDIO_KEYS } from './audioKeys';
import { AUDIO_ADAPTER } from './audioStore';
import { AudioStage } from './SpectrumStage';

export const AUDIO_EDITOR: EditorKindModule = {
  adapter: AUDIO_ADAPTER,
  ui: {
    Stage: AudioStage,
    Tools: AudioTopTools,
    Meta: AudioMeta,
    Capsule: AudioCapsule,
    Status: AudioStatus,
    Ops: AudioOps,
    FormatLosses,
  },
  keys: AUDIO_KEYS,
};
