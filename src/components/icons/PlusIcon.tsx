import { Icon, type IconProps } from './Icon';

/** 追加（Main.dc.html の取り込みボタン）。 */
export function PlusIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 5v14M5 12h14" />
    </Icon>
  );
}
