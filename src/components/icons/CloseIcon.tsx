import { Icon, type IconProps } from './Icon';

/** 閉じる・外す・中止（×）。 */
export function CloseIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M6 6l12 12M18 6L6 18" />
    </Icon>
  );
}
