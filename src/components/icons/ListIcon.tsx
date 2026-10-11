import { Icon, type IconProps } from './Icon';

/** リスト表示（Main.dc.html の表示形式）。 */
export function ListIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M9 6h11M9 12h11M9 18h11" />
      <path d="M4.5 6h.01M4.5 12h.01M4.5 18h.01" strokeWidth={2.6} />
    </Icon>
  );
}
