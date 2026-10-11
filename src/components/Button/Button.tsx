import type { ButtonHTMLAttributes } from 'react';

import styles from './Button.module.css';

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** 主ボタン（アクセント色）か副ボタン。 */
  variant?: 'primary' | 'secondary';
  /** 小さいボタン（高さ 30px、角丸 8px。シートの中の選択など）、大きいボタン（高さ 38px。画面の見出しの横）。 */
  size?: 'md' | 'sm' | 'lg';
}

/** カプセル形のボタン（design.md §4 の主ボタン・副ボタン）。 */
export function Button({
  variant = 'secondary',
  size = 'md',
  className,
  type = 'button',
  ...props
}: ButtonProps) {
  const sizeClass = size === 'sm' ? styles.small : size === 'lg' ? styles.large : '';
  const classes = [styles.button, styles[variant], sizeClass, className].filter(Boolean).join(' ');
  return <button type={type} className={classes} {...props} />;
}
