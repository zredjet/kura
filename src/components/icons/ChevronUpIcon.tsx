import { Icon, type IconProps } from './Icon';

/** 上へ（操作の並べ替え）。 */
export function ChevronUpIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6 15l6-6 6 6" />
    </Icon>
  );
}
