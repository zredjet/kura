import { Icon, type IconProps } from './Icon';

/** 手動のプレイリスト（Sidebar.dc.html：行と音符）。 */
export function PlaylistIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4 6h11M4 11h11M4 16h6" />
      <path d="M15 19v-7l5-1.5v7" />
      <circle cx="13.5" cy="19" r="1.5" />
      <circle cx="18.5" cy="17.5" r="1.5" />
    </Icon>
  );
}
