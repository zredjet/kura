import { Icon, type IconProps } from './Icon';

/** 次へ（Player.dc.html の再生バー）。 */
export function SkipForwardIcon(props: IconProps) {
  return (
    <Icon {...props} filled>
      <path d="M16 5h2v14h-2zM5 5v14l10-7z" />
    </Icon>
  );
}
