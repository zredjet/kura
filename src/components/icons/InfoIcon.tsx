import { Icon, type IconProps } from './Icon';

/** 補足（Import.dc.html の注記）。 */
export function InfoIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 11v5M12 8h.01" />
    </Icon>
  );
}
