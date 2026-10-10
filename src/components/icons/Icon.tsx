import type { ReactNode } from 'react';

export interface IconProps {
  /** 一辺のピクセル数。 */
  size?: number;
  strokeWidth?: number;
}

interface BaseProps extends IconProps {
  children: ReactNode;
  /** 塗りで描くアイコン（一時停止・再生など）。 */
  filled?: boolean;
}

/** アイコンの共通の枠（24 のビューボックス、round の端と角、currentColor）。読み上げない。 */
export function Icon({ size = 16, strokeWidth = 1.9, filled = false, children }: BaseProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill={filled ? 'currentColor' : 'none'}
      stroke={filled ? 'none' : 'currentColor'}
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {children}
    </svg>
  );
}
