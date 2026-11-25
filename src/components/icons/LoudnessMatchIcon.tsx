import { Icon, type IconProps } from './Icon';

/** 音量を合わせて聞く（スピーカーと 2 本の線。docs/design/mockups/AudioSpectrum.html の `match`）。 */
export function LoudnessMatchIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M3 10v4h3l4 3V7l-4 3z" />
      <path d="M14.5 10h6M14.5 14h6" />
    </Icon>
  );
}
