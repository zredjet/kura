import { Icon, type IconProps } from './Icon';

/** 並べ替えのつまみ（Player.dc.html のキューの行）。 */
export function GripIcon(props: IconProps) {
  return (
    <Icon {...props} filled>
      <circle cx="9" cy="6" r="1.5" />
      <circle cx="15" cy="6" r="1.5" />
      <circle cx="9" cy="12" r="1.5" />
      <circle cx="15" cy="12" r="1.5" />
      <circle cx="9" cy="18" r="1.5" />
      <circle cx="15" cy="18" r="1.5" />
    </Icon>
  );
}
