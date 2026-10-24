import { Icon, type IconProps } from './Icon';

/** 塗りつぶし（画像エディタの道具）。 */
export function BucketIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M5 11.5L11.5 5l7 7-6.5 6.5a2 2 0 0 1-2.8 0L5 14.3a2 2 0 0 1 0-2.8z" />
      <path d="M5 12h13.5" />
      <path d="M8.5 7.5L6 5" />
      <path d="M20 15.5s1.5 2 1.5 3a1.5 1.5 0 0 1-3 0c0-1 1.5-3 1.5-3z" />
    </Icon>
  );
}
