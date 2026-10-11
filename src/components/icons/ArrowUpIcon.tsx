import { Icon, type IconProps } from './Icon';

/** 上向きの矢印（Downloads.dc.html の「先に取得する」）。 */
export function ArrowUpIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 19V5M6 11l6-6 6 6" />
    </Icon>
  );
}
