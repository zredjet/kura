import { Icon, type IconProps } from './Icon';

/** 前へ（Player.dc.html の再生バー）。 */
export function SkipBackIcon(props: IconProps) {
  return (
    <Icon {...props} filled>
      <path d="M6 5h2v14H6zM19 5v14L9 12z" />
    </Icon>
  );
}
