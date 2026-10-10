import { Icon, type IconProps } from './Icon';

/** 取り込み（Import.dc.html の見出し）。 */
export function ImportIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 3v11M7.5 9.5L12 14l4.5-4.5" />
      <path d="M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4" />
    </Icon>
  );
}
