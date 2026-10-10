import { Icon, type IconProps } from './Icon';

/** 並び順（Main.dc.html の「追加日」）。 */
export function SortIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M7 4v16M4 17l3 3 3-3M17 20V4M14 7l3-3 3 3" />
    </Icon>
  );
}
