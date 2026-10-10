import { Icon, type IconProps } from './Icon';

/** 選んでいる項目の印。 */
export function CheckIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M5 12l5 5 9-10" />
    </Icon>
  );
}
