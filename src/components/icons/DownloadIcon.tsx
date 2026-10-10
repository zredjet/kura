import { Icon, type IconProps } from './Icon';

/** ダウンロード（Main.dc.html のタイルの左上）。 */
export function DownloadIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 4v11M7 10l5 5 5-5M5 20h14" />
    </Icon>
  );
}
