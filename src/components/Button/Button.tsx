import type { ButtonHTMLAttributes } from 'react';

import styles from './Button.module.css';

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** 主ボタン（アクセント色）か副ボタン。 */
  variant?: 'primary' | 'secondary';
  /** 小さいボタン（高さ 30px、角丸 8px。シートの中の選択など）。 */
  size?: 'md' | 'sm';
}

/** カプセル形のボタン（design.md §4 の主ボタン・副ボタン）。 */
export function Button({
  variant = 'secondary',
  size = 'md',
  className,
  type = 'button',
  ...props
}: ButtonProps) {
  const classes = [styles.button, styles[variant], size === 'sm' ? styles.small : '', className]
    .filter(Boolean)
    .join(' ');
  return <button type={type} className={classes} {...props} />;
}
