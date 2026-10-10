import { Icon, type IconProps } from './Icon';

/** タグ（Search.dc.html の候補）。 */
export function TagIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M3 12V4h8l10 10-8 8z" />
      <circle cx="7.5" cy="7.5" r="1.4" />
    </Icon>
  );
}
