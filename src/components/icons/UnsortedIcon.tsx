import { Icon, type IconProps } from './Icon';

/** 未分類＝点線のフォルダ（Sidebar.dc.html）。 */
export function UnsortedIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path
        d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"
        strokeDasharray="3 2.5"
      />
    </Icon>
  );
}
