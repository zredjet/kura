import { Icon, type IconProps } from './Icon';

/** 最近の取り込み（Sidebar.dc.html）。 */
export function ClockIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="12" cy="12" r="8" />
      <path d="M12 8v4l3 2" />
    </Icon>
  );
}
