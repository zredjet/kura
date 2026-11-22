/**
 * 画像エディタのモジュール（核のアダプタの作り方・画面の部品・キー。`../editorKinds.ts` の表に載せる）。
 */

import type { EditorKindModule } from '../editorKinds';
import { EditorCanvas } from './EditorCanvas';
import { ImageMeta, ImageOps, ImageStatus, ImageTools } from './ImageParts';
import { IMAGE_KEYS } from './imageKeys';
import { IMAGE_ADAPTER } from './imageStore';
import { ToolSettings } from './ToolSettings';

export const IMAGE_EDITOR: EditorKindModule = {
  adapter: IMAGE_ADAPTER,
  ui: {
    Stage: EditorCanvas,
    Tools: ImageTools,
    Meta: ImageMeta,
    Capsule: ToolSettings,
    Status: ImageStatus,
    Ops: ImageOps,
  },
  keys: IMAGE_KEYS,
};
