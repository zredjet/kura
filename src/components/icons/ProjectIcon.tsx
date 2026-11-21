import { Icon, type IconProps } from './Icon';

/** ゲームのプロジェクト（コントローラーの形。M15 段 6）。 */
export function ProjectIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M7 7h10a5 5 0 0 1 0 10 3.5 3.5 0 0 1-2.8-1.4L13 14h-2l-1.2 1.6A3.5 3.5 0 0 1 7 17 5 5 0 0 1 7 7z" />
      <path d="M8 10.5v3M6.5 12h3M15.5 11h.01M17 13h.01" />
    </Icon>
  );
}
