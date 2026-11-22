import { Icon, type IconProps } from './Icon';

/** スポイト（画像エディタの道具：画素の色を拾う）。 */
export function EyedropperIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M14.5 6.5l3 3" />
      <path d="M16 4.5a2.1 2.1 0 0 1 3 0l.5.5a2.1 2.1 0 0 1 0 3L18 9.5 14.5 6z" />
      <path d="M15.5 8.5L7 17l-3 1 1-3 8.5-8.5" />
    </Icon>
  );
}
