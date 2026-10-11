import { Icon, type IconProps } from './Icon';

/** 書き出し（取り込みの矢印を上へ）。 */
export function ExportIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 14V3M7.5 7.5L12 3l4.5 4.5" />
      <path d="M4 14v4a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-4" />
    </Icon>
  );
}
