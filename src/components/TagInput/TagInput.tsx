import { type KeyboardEvent, useState } from 'react';

import { CloseIcon } from '../icons/CloseIcon';
import styles from './TagInput.module.css';

interface TagInputProps {
  tags: readonly string[];
  onChange: (tags: string[]) => void;
  placeholder: string;
  /** 入力欄の読み上げ名。 */
  inputLabel: string;
  /** 「×」ボタンの読み上げ名。 */
  removeLabel: (tag: string) => string;
}

/** 区切り文字（読点・カンマ）。 */
const SEPARATORS = new Set([',', '、', '，']);

/** タグのチップと入力欄。Enter か読点で確定、空のときの Backspace で最後のタグを外す。 */
export function TagInput({ tags, onChange, placeholder, inputLabel, removeLabel }: TagInputProps) {
  const [draft, setDraft] = useState('');

  const commit = (text: string) => {
    const value = text.trim();
    setDraft('');
    if (value && !tags.includes(value)) {
      onChange([...tags, value]);
    }
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // 日本語入力の変換中の Enter は確定に使わない
    if (e.nativeEvent.isComposing) {
      return;
    }
    if (e.key === 'Enter' || SEPARATORS.has(e.key)) {
      e.preventDefault();
      commit(draft);
    } else if (e.key === 'Backspace' && draft === '' && tags.length > 0) {
      e.preventDefault();
      onChange(tags.slice(0, -1));
    }
  };

  return (
    <div className={styles.field}>
      {tags.map((tag) => (
        <span key={tag} className={styles.chip}>
          {tag}
          <button
            type="button"
            className={styles.remove}
            aria-label={removeLabel(tag)}
            onClick={() => onChange(tags.filter((t) => t !== tag))}
          >
            <CloseIcon size={9} strokeWidth={3} />
          </button>
        </span>
      ))}
      <input
        type="text"
        className={styles.input}
        value={draft}
        placeholder={placeholder}
        aria-label={inputLabel}
        onChange={(e) => {
          const value = e.target.value;
          const last = value.slice(-1);
          if (SEPARATORS.has(last)) {
            commit(value.slice(0, -1));
          } else {
            setDraft(value);
          }
        }}
        onKeyDown={onKeyDown}
        onBlur={() => commit(draft)}
      />
    </div>
  );
}
