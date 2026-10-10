import { Icon, type IconProps } from './Icon';

/** 画像（Main.dc.html の種類のセグメント）。 */
export function ImageIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="5" width="18" height="14" rx="2" />
      <circle cx="9" cy="10" r="1.8" />
      <path d="M21 16l-5-5-8 8" />
    </Icon>
  );
}
