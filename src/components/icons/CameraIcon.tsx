import { Icon, type IconProps } from './Icon';

/** キャプチャ（Main.dc.html のタイルの左上）。 */
export function CameraIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4 8h3l2-3h6l2 3h3v11H4z" />
      <circle cx="12" cy="13" r="3.5" />
    </Icon>
  );
}
