import { Icon, type IconProps } from './Icon';

/** 一時停止（Downloads.dc.html）。 */
export function PauseIcon(props: IconProps) {
  return (
    <Icon {...props} filled>
      <rect x="6" y="5" width="4" height="14" rx="1" />
      <rect x="14" y="5" width="4" height="14" rx="1" />
    </Icon>
  );
}
