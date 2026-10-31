import { Icon, type IconProps } from './Icon';

/** フェード（音声エディタの道具）：上がって平らになって下がる線。 */
export function FadeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <path d="M3 19L9 6h6l6 13" />
    </Icon>
  );
}
