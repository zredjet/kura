import { ja } from '../../i18n/ja';
import { useViewStore } from '../../stores/viewStore';
import { effectiveTarget, useProjectStore } from '../projects/projectStore';
import { useDragStore } from './dragState';
import styles from './DragGhost.module.css';

/**
 * ドラッグ中にポインタの横に出す印（「3 項目」・フォルダ名）。タイル・リストの行を落とし先のない所で動かしているときは、
 * 窓の外へ出すと持ち出せることと持ち出し先を 2 行目に添える（M15 段 6。design.md §3.1）。
 */
export function DragGhost() {
  const payload = useDragStore((s) => s.payload);
  const x = useDragStore((s) => s.x);
  const y = useDragStore((s) => s.y);
  const label = useDragStore((s) => s.label);
  const target = useDragStore((s) => s.target);
  const project = useProjectStore((s) => effectiveTarget(s.list, s.target));
  // ゴミ箱の中は持ち出せない（ドラッグアウトが断る）ので添えない
  const trash = useViewStore((s) => s.view.kind === 'trash');
  if (!payload) {
    return null;
  }
  const hint =
    payload.kind === 'entries' && target === null && !trash
      ? project
        ? ja.dragOut.hint(project.name)
        : ja.dragOut.hintPlain
      : null;
  return (
    <div
      className={styles.ghost}
      style={{ transform: `translate(${x + 14}px, ${y + 10}px)` }}
      aria-hidden="true"
    >
      {label}
      {hint && <span className={styles.hint}>{hint}</span>}
    </div>
  );
}
