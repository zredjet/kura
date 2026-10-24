import { Icon, type IconProps } from './Icon';

/** 減らす（画像エディタの縮小・筆を小さく）。 */
export function MinusIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M5 12h14" />
    </Icon>
  );
}
