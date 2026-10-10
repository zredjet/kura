import { Icon, type IconProps } from './Icon';

/** 再生中の印（Player.dc.html のキューの行。色は使う側の color）。 */
export function NowPlayingIcon(props: IconProps) {
  return (
    <Icon {...props} filled>
      <rect x="4" y="9" width="3" height="10" rx="1" />
      <rect x="10.5" y="5" width="3" height="14" rx="1" />
      <rect x="17" y="11" width="3" height="8" rx="1" />
    </Icon>
  );
}
