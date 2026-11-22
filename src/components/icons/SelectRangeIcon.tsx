import { Icon, type IconProps } from './Icon';

/** 範囲を選ぶ（音声エディタの道具）：両端の括弧と中の線。 */
export function SelectRangeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M7 4H4v16h3M17 4h3v16h-3M9 12h6" />
    </Icon>
  );
}
