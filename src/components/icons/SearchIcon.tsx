import { Icon, type IconProps } from './Icon';

/** 検索（Main.dc.html・Search.dc.html）。 */
export function SearchIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <circle cx="11" cy="11" r="7" />
      <path d="M20 20l-4-4" />
    </Icon>
  );
}
