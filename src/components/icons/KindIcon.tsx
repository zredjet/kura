import type { ComponentType } from 'react';

import { isItemKind, type ItemKind } from '../../ipc/kinds';
import { CubeIcon } from './CubeIcon';
import { FontIcon } from './FontIcon';
import type { IconProps } from './Icon';
import { ImageIcon } from './ImageIcon';
import { MusicIcon } from './MusicIcon';
import { VideoIcon } from './VideoIcon';

/** 種類ごとのアイコン（種類を足して書き忘れると型で落ちる。kinds.test.ts でも確かめる）。 */
export const KIND_ICONS: Readonly<Record<ItemKind, ComponentType<IconProps>>> = {
  image: ImageIcon,
  video: VideoIcon,
  audio: MusicIcon,
  font: FontIcon,
  model: CubeIcon,
};

/**
 * 種類のアイコン（サムネイルがないときの絵・フィルタバー・バッジ）。
 * 知らない種類（新しい版の Kura のライブラリなど）は画像の絵にする。
 */
export function KindIcon({ kind, ...props }: IconProps & { kind: string | null | undefined }) {
  const Glyph = isItemKind(kind) ? KIND_ICONS[kind] : ImageIcon;
  return <Glyph {...props} />;
}
