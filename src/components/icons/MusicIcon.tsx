import { Icon, type IconProps } from './Icon';

/** 音楽（Main.dc.html のタイルのバッジ）。 */
export function MusicIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M9 18V6l11-2v12" />
      <circle cx="6.5" cy="18" r="2.5" />
      <circle cx="17.5" cy="16" r="2.5" />
    </Icon>
  );
}
