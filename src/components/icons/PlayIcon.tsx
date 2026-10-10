import { Icon, type IconProps } from './Icon';

/** 再生・再開。 */
export function PlayIcon(props: IconProps) {
  return (
    <Icon {...props} filled>
      <path d="M6 4v16l14-8z" />
    </Icon>
  );
}
