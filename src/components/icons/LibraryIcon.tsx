import { Icon, type IconProps } from './Icon';

/** ライブラリ（Sidebar.dc.html のライブラリ切り替え）。 */
export function LibraryIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4 10l8-5 8 5" />
      <path d="M6 10v9h12v-9" />
      <path d="M10 19v-5h4v5" />
    </Icon>
  );
}
