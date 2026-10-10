import { type KeyboardEvent, useEffect, useId, useState } from 'react';

import { ja } from '../../i18n/ja';
import { suggestTags, tagLabel, type TagRefDto, type TagSuggestionDto } from '../../ipc/tags';
import styles from './Tags.module.css';

/** 候補を探すまでの待ち時間（ms）。 */
const DELAY = 60;

interface TagFieldProps {
  /** 確定したタグの名前（候補なら正式名、新しいタグなら入力したまま）と、候補から選んだならそのタグ。 */
  onAdd: (name: string, tag: TagRefDto | null) => void;
  placeholder: string;
  label: string;
  /** 候補から外すタグ（すでに付いているもの）。 */
  exclude?: ReadonlySet<number>;
  /** 既存のタグだけを選ばせる（親タグなど）。 */
  existingOnly?: boolean;
  /** T キーでここに移るための印。 */
  focusKey?: string;
}

/**
 * タグを入れる欄（候補付き）。前方一致 → 部分一致、よく使うものから出す。
 * ↑↓ で候補を選び、Return で付ける。読点・カンマでも確定する。
 */
export function TagField({
  onAdd,
  placeholder,
  label,
  exclude,
  existingOnly = false,
  focusKey,
}: TagFieldProps) {
  const listId = useId();
  const [text, setText] = useState('');
  const [open, setOpen] = useState(false);
  const [suggestions, setSuggestions] = useState<TagSuggestionDto[]>([]);
  const [active, setActive] = useState(-1);

  useEffect(() => {
    if (!open) {
      return;
    }
    let alive = true;
    const timer = setTimeout(() => {
      suggestTags(text, 8)
        .then((found) => {
          if (alive) {
            setSuggestions(found.filter((s) => !exclude?.has(s.tag.id)));
            setActive(text.trim() ? 0 : -1);
          }
        })
        .catch((error: unknown) => console.warn('タグの候補を探せませんでした', error));
    }, DELAY);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [text, open, exclude]);

  const commit = (name: string, tag: TagRefDto | null) => {
    setText('');
    setActive(-1);
    if (name.trim()) {
      onAdd(name.trim(), tag);
    }
  };

  const typed = text.trim();
  const exact = suggestions.some((s) => tagLabel(s.tag) === typed);
  const showNew = !existingOnly && typed !== '' && !exact;
  const options = suggestions.length + (showNew ? 1 : 0);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (e.nativeEvent.isComposing) {
      return;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      setOpen(true);
      if (options > 0) {
        const step = e.key === 'ArrowDown' ? 1 : -1;
        setActive((i) => (i + step + options) % options);
      }
    } else if (e.key === 'Enter' || e.key === ',' || e.key === '、') {
      e.preventDefault();
      const chosen = suggestions[active];
      if (chosen) {
        commit(tagLabel(chosen.tag), chosen.tag);
      } else if (!existingOnly) {
        commit(typed, null);
      }
    } else if (e.key === 'Escape') {
      if (text) {
        e.preventDefault();
        setText('');
      } else {
        e.currentTarget.blur();
      }
    }
  };

  const activeId = active >= 0 ? `${listId}-${active}` : undefined;
  return (
    <div className={styles.field}>
      <input
        className={styles.input}
        value={text}
        placeholder={placeholder}
        aria-label={label}
        role="combobox"
        aria-expanded={open && options > 0}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={open ? activeId : undefined}
        data-focus-key={focusKey}
        onChange={(e) => {
          setText(e.target.value);
          setOpen(true);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onKeyDown={onKeyDown}
      />
      {open && options > 0 && (
        <ul id={listId} role="listbox" aria-label={label} className={styles.suggestions}>
          {suggestions.map((s, i) => (
            <li
              key={s.tag.id}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              className={styles.suggestion}
              // 入力欄からフォーカスを外さずに選ぶ（キーボードでは ↑↓ と Return）
              onMouseDown={(e) => {
                e.preventDefault();
                commit(tagLabel(s.tag), s.tag);
              }}
            >
              <span className={styles.suggestionName}>
                {s.tag.namespace && <span className={styles.namespace}>{s.tag.namespace}</span>}
                {s.tag.name}
              </span>
              {s.alias && (
                <span className={styles.suggestionNote}>{ja.inspector.aliasOf(s.alias)}</span>
              )}
              <span className={styles.suggestionCount}>{s.count}</span>
            </li>
          ))}
          {showNew && (
            <li
              id={`${listId}-${suggestions.length}`}
              role="option"
              aria-selected={active === suggestions.length}
              className={styles.suggestion}
              onMouseDown={(e) => {
                e.preventDefault();
                commit(typed, null);
              }}
            >
              <span className={styles.suggestionName}>{ja.inspector.newTag(typed)}</span>
            </li>
          )}
        </ul>
      )}
    </div>
  );
}
