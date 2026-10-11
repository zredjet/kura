import { Icon, type IconProps } from './Icon';

/** ジャスティファイ表示（Main.dc.html の表示形式）。 */
export function JustifyIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="5" width="10" height="6" rx="1.5" />
      <rect x="15" y="5" width="6" height="6" rx="1.5" />
      <rect x="3" y="13" width="6" height="6" rx="1.5" />
      <rect x="11" y="13" width="10" height="6" rx="1.5" />
    </Icon>
  );
}
