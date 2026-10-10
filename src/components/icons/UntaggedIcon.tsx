import { Icon, type IconProps } from './Icon';

/** タグなし（Sidebar.dc.html）。 */
export function UntaggedIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M3 12V4h8l10 10-8 8z" />
      <path d="M4 20L20 4" />
    </Icon>
  );
}
