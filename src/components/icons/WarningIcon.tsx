import { Icon, type IconProps } from './Icon';

/** 注意。 */
export function WarningIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 4l9 16H3z" />
      <path d="M12 10v4M12 17h.01" />
    </Icon>
  );
}
