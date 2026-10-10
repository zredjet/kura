import { Icon, type IconProps } from './Icon';

/** 動画（Main.dc.html の種類のセグメント）。 */
export function VideoIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="6" width="13" height="12" rx="2" />
      <path d="M16 10l5-3v10l-5-3z" />
    </Icon>
  );
}
