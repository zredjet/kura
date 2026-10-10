import { Icon, type IconProps } from './Icon';

/** サムネイルを大きく（Main.dc.html のスライダーの右）。 */
export function ThumbLargeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="3" width="18" height="18" rx="2.5" />
    </Icon>
  );
}
