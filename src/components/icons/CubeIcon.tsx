import { Icon, type IconProps } from './Icon';

/** 3D（立方体。design.md §3.1 のフィルタバー・§4 のタイルのバッジ）。 */
export function CubeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z" />
      <path d="M4 7.5l8 4.5 8-4.5" />
      <path d="M12 12v9" />
    </Icon>
  );
}
