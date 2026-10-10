import { Icon, type IconProps } from './Icon';

/** すべて（Sidebar.dc.html）。 */
export function AllItemsIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="4" y="7" width="13" height="13" rx="2" />
      <path d="M8 4h10a2 2 0 0 1 2 2v10" />
    </Icon>
  );
}
