import { Icon, type IconProps } from './Icon';

/** 重なったコマ（アニメの画像のタイルの印。design.md §4。動画の ▶ と見分ける）。 */
export function FramesIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="10" width="11" height="11" rx="1.5" />
      <path d="M6.5 10V8.5A1.5 1.5 0 0 1 8 7h8.5A1.5 1.5 0 0 1 18 8.5V17a1.5 1.5 0 0 1-1.5 1.5H14" />
      <path d="M10 7V5.5A1.5 1.5 0 0 1 11.5 4h8A1.5 1.5 0 0 1 21 5.5V14a1.5 1.5 0 0 1-1.5 1.5H18" />
    </Icon>
  );
}
