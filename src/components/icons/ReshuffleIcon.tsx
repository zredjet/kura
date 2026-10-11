import { Icon, type IconProps } from './Icon';

/** 並べ直す・作り直す（Player.dc.html・Playlist.dc.html）。 */
export function ReshuffleIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v4h-4" />
    </Icon>
  );
}
