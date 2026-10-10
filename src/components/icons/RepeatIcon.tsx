import { Icon, type IconProps } from './Icon';

/** リピート（Player.dc.html の再生バー）。 */
export function RepeatIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M17 3l3 3-3 3M4 11V9a3 3 0 0 1 3-3h13M7 21l-3-3 3-3M20 13v2a3 3 0 0 1-3 3H4" />
    </Icon>
  );
}
