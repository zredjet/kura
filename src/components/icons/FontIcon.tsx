import { Icon, type IconProps } from './Icon';

/** フォント（「Aa」の字形。design.md §3.1 のフィルタバー・§4 のタイルのバッジ）。 */
export function FontIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M2 19L7 5l5 14" />
      <path d="M3.8 14h6.4" />
      <circle cx="17.25" cy="15.75" r="3.25" />
      <path d="M20.5 12.5V19" />
    </Icon>
  );
}
