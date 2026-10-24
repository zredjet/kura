import { Icon, type IconProps } from './Icon';

/** ペン（画像エディタの道具）。 */
export function PenIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L8 18l-4 1 1-4z" />
      <path d="M14.5 5.5l3 3" />
    </Icon>
  );
}
