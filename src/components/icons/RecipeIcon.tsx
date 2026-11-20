import { Icon, type IconProps } from './Icon';

/** レシピ（操作の並びのカード：線 3 本と、先頭の操作の点。M14 段 7）。 */
export function RecipeIcon(props: IconProps) {
  return (
    <Icon {...props}>
      <rect x="4" y="3.5" width="16" height="17" rx="2.5" />
      <path d="M11 8.5h5M11 12h5M11 15.5h5" />
      <circle cx="8" cy="8.5" r="0.9" />
      <circle cx="8" cy="12" r="0.9" />
      <circle cx="8" cy="15.5" r="0.9" />
    </Icon>
  );
}
