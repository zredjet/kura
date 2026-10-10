import { Icon, type IconProps } from './Icon';

/** サムネイルを小さく（Main.dc.html のスライダーの左）。 */
export function ThumbSmallIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="5" y="5" width="14" height="14" rx="2" />
    </Icon>
  );
}
