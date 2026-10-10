import { Icon, type IconProps } from './Icon';

/** 選択肢を開く（▾）。 */
export function ChevronDownIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6 9l6 6 6-6" />
    </Icon>
  );
}
