import { Icon, type IconProps } from './Icon';

/** 切り抜き（画像エディタの道具）。 */
export function CropIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6 2v14a2 2 0 0 0 2 2h14" />
      <path d="M18 22V8a2 2 0 0 0-2-2H2" />
    </Icon>
  );
}
