import { Icon, type IconProps } from './Icon';

/** 消しゴム（画像エディタの道具）。 */
export function EraserIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M7 20h13" />
      <path d="M4.6 14.6l9.9-9.9a2 2 0 0 1 2.8 0l2 2a2 2 0 0 1 0 2.8L11 18H8.4z" />
      <path d="M9 10.2l4.8 4.8" />
    </Icon>
  );
}
