import { Icon, type IconProps } from './Icon';

/** シャッフル（Player.dc.html の再生バー）。 */
export function ShuffleIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M16 4h4v4M4 20L20 4M20 16v4h-4M15 15l5 5M4 4l5 5" />
    </Icon>
  );
}
