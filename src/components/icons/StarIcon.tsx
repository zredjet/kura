import { Icon, type IconProps } from './Icon';

interface StarIconProps extends IconProps {
  filled?: boolean;
}

/** 評価の星（Main.dc.html のインスペクタ）。 */
export function StarIcon({ filled = false, ...props }: StarIconProps) {
  return (
    <Icon {...props}>
      <path
        d="M12 4l2.4 5 5.4.6-4 3.7 1.1 5.3L12 16l-4.9 2.6 1.1-5.3-4-3.7 5.4-.6z"
        fill={filled ? 'currentColor' : 'none'}
      />
    </Icon>
  );
}
