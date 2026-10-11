import { Icon, type IconProps } from './Icon';

/** フルスクリーンを解除（ExpandIcon の矢印を内向きに）。 */
export function ShrinkIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" />
    </Icon>
  );
}
