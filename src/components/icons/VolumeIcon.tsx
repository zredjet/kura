import { Icon, type IconProps } from './Icon';

/** 音量（Player.dc.html の再生バー）。 */
export function VolumeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4 10v4h4l5 4V6L8 10z" />
      <path d="M16 9a4 4 0 0 1 0 6" />
    </Icon>
  );
}
