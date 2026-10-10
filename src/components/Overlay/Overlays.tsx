import { type KeyboardEvent, useEffect, useId, useRef, useState } from 'react';

import { ja } from '../../i18n/ja';
import { Button } from '../Button/Button';
import { Dialog } from '../Dialog/Dialog';
import { cancelOverlay, type Overlay, type PickerOption, useOverlayStore } from './overlayStore';
import styles from './Overlays.module.css';

/** 名前の入力・確認・候補から選ぶダイアログ（`askText`・`askConfirm`・`askPick` で開く）。 */
export function Overlays() {
  const overlay = useOverlayStore((s) => s.overlay);
  if (!overlay) {
    return null;
  }
  switch (overlay.kind) {
    case 'prompt':
      return <PromptDialog overlay={overlay} />;
    case 'confirm':
      return <ConfirmDialog overlay={overlay} />;
    case 'picker':
      return <PickerDialog overlay={overlay} />;
  }
}

function PromptDialog({ overlay }: { overlay: Extract<Overlay, { kind: 'prompt' }> }) {
  const titleId = useId();
  const inputId = useId();
  const [value, setValue] = useState(overlay.initial);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);
  const submit = () => {
    if (value.trim()) {
      overlay.resolve(value.trim());
    }
  };
  return (
    <Dialog labelledBy={titleId} onClose={cancelOverlay} size="sm">
      <h2 id={titleId} className={styles.title}>
        {overlay.title}
      </h2>
      <div className={styles.field}>
        <label htmlFor={inputId} className={styles.label}>
          {overlay.label}
        </label>
        <input
          ref={inputRef}
          id={inputId}
          className={styles.input}
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.nativeEvent.isComposing) {
              e.preventDefault();
              submit();
            }
          }}
        />
      </div>
      <footer className={styles.actions}>
        <Button onClick={cancelOverlay}>{ja.common.cancel}</Button>
        <Button variant="primary" disabled={!value.trim()} onClick={submit}>
          {overlay.confirm}
        </Button>
      </footer>
    </Dialog>
  );
}

function ConfirmDialog({ overlay }: { overlay: Extract<Overlay, { kind: 'confirm' }> }) {
  const titleId = useId();
  return (
    <Dialog labelledBy={titleId} onClose={cancelOverlay} size="sm">
      <h2 id={titleId} className={styles.title}>
        {overlay.title}
      </h2>
      <p className={styles.message}>{overlay.message}</p>
      <footer className={styles.actions}>
        <Button onClick={cancelOverlay}>{ja.common.cancel}</Button>
        <Button variant="primary" onClick={() => overlay.resolve(true)}>
          {overlay.confirm}
        </Button>
      </footer>
    </Dialog>
  );
}

/** 入力が止まってから候補を探すまでの時間（ms）。 */
const SEARCH_DELAY = 60;

function PickerDialog({ overlay }: { overlay: Extract<Overlay, { kind: 'picker' }> }) {
  const titleId = useId();
  const listId = useId();
  const [query, setQuery] = useState('');
  const [options, setOptions] = useState<PickerOption[]>([]);
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    let alive = true;
    const timer = setTimeout(() => {
      overlay
        .search(query)
        .then((found) => {
          if (alive) {
            setOptions(found);
            setActive(0);
          }
        })
        .catch((error: unknown) => console.warn('候補を探せませんでした', error));
    }, SEARCH_DELAY);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [query, overlay]);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing) {
      return;
    }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const step = e.key === 'ArrowDown' ? 1 : -1;
      setActive((i) => (options.length === 0 ? 0 : (i + step + options.length) % options.length));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const option = options[active];
      if (option) {
        overlay.resolve(option);
      }
    }
  };

  const activeOption = options[active];
  return (
    <Dialog labelledBy={titleId} onClose={cancelOverlay} size="sm">
      <h2 id={titleId} className={styles.title}>
        {overlay.title}
      </h2>
      <input
        ref={inputRef}
        className={styles.input}
        value={query}
        placeholder={overlay.placeholder}
        aria-label={overlay.placeholder}
        role="combobox"
        aria-expanded="true"
        aria-controls={listId}
        aria-activedescendant={activeOption ? `${listId}-${activeOption.key}` : undefined}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={onKeyDown}
      />
      <ul id={listId} role="listbox" aria-label={overlay.title} className={styles.options}>
        {options.length === 0 && <li className={styles.empty}>{overlay.empty}</li>}
        {options.map((option, i) => (
          // 候補の行はマウスでも選べる。キーボードでは入力欄（combobox）から ↑↓ と Return で選ぶ
          // eslint-disable-next-line jsx-a11y/click-events-have-key-events
          <li
            key={option.key}
            id={`${listId}-${option.key}`}
            role="option"
            aria-selected={i === active}
            className={styles.option}
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => overlay.resolve(option)}
            onMouseEnter={() => setActive(i)}
          >
            {option.icon && <span className={styles.optionIcon}>{option.icon}</span>}
            <span className={styles.optionText}>
              <span className={styles.optionLabel}>{option.label}</span>
              {option.detail && <span className={styles.optionDetail}>{option.detail}</span>}
            </span>
          </li>
        ))}
      </ul>
    </Dialog>
  );
}
