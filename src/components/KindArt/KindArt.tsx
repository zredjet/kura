import type { ItemKind } from '../../ipc/kinds';
import { KindIcon } from '../icons/KindIcon';
import styles from './KindArt.module.css';

/**
 * サムネイルのない素材（フォント・3D）の代わりに出す種類の絵（design.md §3.1）。
 * 置いた枠いっぱいに広がり、中央に種類のアイコンを置く。読み上げない（名前と種類は枠の側が読む）。
 */
export function KindArt({ kind, className }: { kind: ItemKind; className?: string }) {
  return (
    <span
      className={className ? `${styles.art} ${className}` : styles.art}
      data-kind={kind}
      aria-hidden="true"
    >
      <KindIcon kind={kind} strokeWidth={1.8} />
    </span>
  );
}
