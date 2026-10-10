import { Icon, type IconProps } from './Icon';

/** ユニット（重なった板）。 */
export function UnitIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 4l8 4-8 4-8-4z" />
      <path d="M4 12l8 4 8-4" />
      <path d="M4 16l8 4 8-4" />
    </Icon>
  );
}
