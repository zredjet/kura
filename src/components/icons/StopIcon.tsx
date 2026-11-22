import { Icon, type IconProps } from './Icon';

/** 止める（音声エディタの再生）：塗りの四角。 */
export function StopIcon(props: IconProps) {
  return (
    <Icon {...props} filled>
      <rect x="6" y="6" width="12" height="12" rx="1.5" />
    </Icon>
  );
}
