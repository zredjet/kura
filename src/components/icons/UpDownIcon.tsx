import { Icon, type IconProps } from './Icon';

/** 上下の矢印（ライブラリ切り替えのメニュー）。 */
export function UpDownIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M8 10l4-4 4 4M8 14l4 4 4-4" />
    </Icon>
  );
}
