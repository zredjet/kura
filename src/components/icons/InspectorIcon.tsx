import { Icon, type IconProps } from './Icon';

/** インスペクタの表示切り替え（Main.dc.html のツールバーの右端）。 */
export function InspectorIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="3" y="5" width="18" height="14" rx="2.5" />
      <path d="M15 5v14" />
    </Icon>
  );
}
