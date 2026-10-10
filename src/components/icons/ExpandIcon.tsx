import { Icon, type IconProps } from './Icon';

/** シアター表示で開く（Main.dc.html のミニプレーヤー）。 */
export function ExpandIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />
    </Icon>
  );
}
