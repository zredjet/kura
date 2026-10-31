import { Icon, type IconProps } from './Icon';

/** 切り詰め（音声エディタの道具）：はさみ。 */
export function ScissorsIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="6" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M20 4L8.1 15.9M14.5 14.5L20 20M8.1 8.1L12 12" />
    </Icon>
  );
}
