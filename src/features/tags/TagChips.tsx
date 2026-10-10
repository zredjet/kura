import { CloseIcon } from '../../components/icons/CloseIcon';
import { ja } from '../../i18n/ja';
import { tagLabel, type TagRefDto } from '../../ipc/tags';
import { openTagDialog } from './tagDialogStore';
import styles from './Tags.module.css';

interface TagChipsProps {
  tags: readonly TagRefDto[];
  onRemove: (tag: TagRefDto) => void;
  /** 一部にだけ付いているタグ（複数選択）。押すとすべてに付ける。 */
  partial?: readonly { tag: TagRefDto; count: number }[];
  total?: number;
  onApplyAll?: (tag: TagRefDto) => void;
}

/** タグのチップ（design.md §4：高さ 24px、角丸 6px、名前空間は tertiary で前置）。名前を押すと編集。 */
export function TagChips({ tags, onRemove, partial = [], total = 0, onApplyAll }: TagChipsProps) {
  if (tags.length === 0 && partial.length === 0) {
    return null;
  }
  return (
    <ul className={styles.chips}>
      {tags.map((tag) => (
        <li key={tag.id} className={styles.chip} data-color={tag.color ?? undefined}>
          <button
            type="button"
            className={styles.chipName}
            aria-label={ja.inspector.editTag(tagLabel(tag))}
            onClick={() => openTagDialog(tag.id)}
          >
            {tag.color && <span className={styles.dot} aria-hidden="true" />}
            {tag.namespace && <span className={styles.namespace}>{tag.namespace}</span>}
            {tag.name}
          </button>
          <button
            type="button"
            className={styles.chipRemove}
            aria-label={ja.inspector.removeTag(tagLabel(tag))}
            onClick={() => onRemove(tag)}
          >
            <CloseIcon size={9} strokeWidth={3} />
          </button>
        </li>
      ))}
      {partial.map(({ tag, count }) => (
        <li key={tag.id} className={`${styles.chip} ${styles.partial}`}>
          <button
            type="button"
            className={styles.chipName}
            aria-label={ja.inspector.applyToAll(tagLabel(tag))}
            title={ja.inspector.partial(String(count), String(total))}
            onClick={() => onApplyAll?.(tag)}
          >
            {tag.namespace && <span className={styles.namespace}>{tag.namespace}</span>}
            {tag.name}
            <span className={styles.partialCount}>
              {count}/{total}
            </span>
          </button>
          <button
            type="button"
            className={styles.chipRemove}
            aria-label={ja.inspector.removeTag(tagLabel(tag))}
            onClick={() => onRemove(tag)}
          >
            <CloseIcon size={9} strokeWidth={3} />
          </button>
        </li>
      ))}
    </ul>
  );
}
