import { Icon, type IconProps } from './Icon';

/** 手のひら（画像エディタの道具：画像を動かす）。 */
export function HandIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M8 13V5.5a1.5 1.5 0 0 1 3 0V12" />
      <path d="M11 11V4a1.5 1.5 0 0 1 3 0v7" />
      <path d="M14 11V5.5a1.5 1.5 0 0 1 3 0V13" />
      <path d="M17 9.5a1.5 1.5 0 0 1 3 0V15a6 6 0 0 1-6 6h-1.5a6 6 0 0 1-4.6-2.2L4.6 15.7a1.5 1.5 0 0 1 2.3-1.9L8 15" />
    </Icon>
  );
}
