import { Icon, type IconProps } from './Icon';

/** ゴミ箱（Sidebar.dc.html）。 */
export function TrashIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M5 7h14M10 7V5h4v2M7 7l1 13h8l1-13" />
    </Icon>
  );
}
