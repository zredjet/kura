import { type KeyboardEvent, useEffect, useId, useRef, useState } from 'react';

import { ja } from '../../i18n/ja';
import { Button } from '../Button/Button';
import { Dialog } from '../Dialog/Dialog';
import { formatCount } from '../../lib/format';
import { Segmented } from '../Segmented/Segmented';
import {
  type AddModeValue,
  cancelOverlay,
  type NewPlaylistValue,
  type Overlay,
  type PickerOption,
  useOverlayStore,
} from './overlayStore';
import styles from './Overlays.module.css';
import { isImeKey } from '../../lib/ime';

/** 名前の入力・確認・選択肢・候補から選ぶダイアログ（`askText`・`askConfirm`・`askChoice`・`askPick` で開く）。 */
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
    case 'choice':
      return <ChoiceDialog overlay={overlay} />;
    case 'picker':
      return <PickerDialog overlay={overlay} />;
    case 'newPlaylist':
      return <NewPlaylistDialog overlay={overlay} />;
    case 'addMode':
      return <AddModeDialog overlay={overlay} />;
  }
}

function NewPlaylistDialog({ overlay }: { overlay: Extract<Overlay, { kind: 'newPlaylist' }> }) {
  const t = ja.newPlaylist;
  const titleId = useId();
  const inputId = useId();
  const [value, setValue] = useState<NewPlaylistValue>(overlay.initial);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);
  const submit = () => {
    if (value.name.trim()) {
      overlay.resolve({ ...value, name: value.name.trim() });
    }
  };
  return (
    <Dialog labelledBy={titleId} onClose={cancelOverlay} size="sm">
      <h2 id={titleId} className={styles.title}>
        {t.title}
      </h2>
      <div className={styles.field}>
        <label htmlFor={inputId} className={styles.label}>
          {t.name}
        </label>
        <input
          ref={inputRef}
          id={inputId}
          className={styles.input}
          value={value.name}
          onChange={(e) => setValue({ ...value, name: e.target.value })}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !isImeKey(e)) {
              e.preventDefault();
              submit();
            }
          }}
        />
      </div>
      <div className={styles.field}>
        <span className={styles.label}>{t.kind}</span>
        <Segmented
          label={t.kind}
          value={value.kind}
          options={[
            { value: 'manual', label: t.manual },
            { value: 'smart', label: t.smart },
          ]}
          onChange={(kind) => setValue({ ...value, kind })}
        />
      </div>
      <div className={styles.field}>
        <span className={styles.label}>{t.granularity}</span>
        <Segmented
          label={t.granularity}
          value={value.granularity}
          options={[
            { value: 'file', label: t.file },
            { value: 'unit', label: t.unit },
          ]}
          onChange={(granularity) => setValue({ ...value, granularity })}
        />
        <p className={styles.note}>{value.granularity === 'file' ? t.hintFile : t.hintUnit}</p>
      </div>
      <footer className={styles.actions}>
        <Button onClick={cancelOverlay}>{ja.common.cancel}</Button>
        <Button variant="primary" disabled={!value.name.trim()} onClick={submit}>
          {t.confirm}
        </Button>
      </footer>
    </Dialog>
  );
}

function AddModeDialog({ overlay }: { overlay: Extract<Overlay, { kind: 'addMode' }> }) {
  const t = ja.playlist.addMode;
  const titleId = useId();
  const inputId = useId();
  const [value, setValue] = useState<AddModeValue>(overlay.initial);
  const submit = () => {
    if (value.mode === 'each' || value.name.trim()) {
      overlay.resolve({ ...value, name: value.name.trim() });
    }
  };
  return (
    <Dialog labelledBy={titleId} onClose={cancelOverlay} size="sm">
      <h2 id={titleId} className={styles.title}>
        {t.title}
      </h2>
      <p className={styles.message}>{t.message(formatCount(overlay.count))}</p>
      <div className={styles.choices} role="radiogroup" aria-labelledby={titleId}>
        {(['group', 'each'] as const).map((mode) => (
          <label key={mode} className={styles.choice}>
            <input
              type="radio"
              name="add-mode"
              checked={value.mode === mode}
              onChange={() => setValue({ ...value, mode })}
            />
            {mode === 'group' ? t.group : t.each}
          </label>
        ))}
      </div>
      {value.mode === 'group' && (
        <div className={styles.field}>
          <label htmlFor={inputId} className={styles.label}>
            {t.name}
          </label>
          <input
            id={inputId}
            className={styles.input}
            value={value.name}
            onChange={(e) => setValue({ ...value, name: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !isImeKey(e)) {
                e.preventDefault();
                submit();
              }
            }}
          />
        </div>
      )}
      <label className={styles.choice}>
        <input
          type="checkbox"
          checked={value.remember}
          onChange={(e) => setValue({ ...value, remember: e.target.checked })}
        />
        {t.remember}
      </label>
      <footer className={styles.actions}>
        <Button onClick={cancelOverlay}>{ja.common.cancel}</Button>
        <Button
          variant="primary"
          disabled={value.mode === 'group' && !value.name.trim()}
          onClick={submit}
        >
          {t.confirm}
        </Button>
      </footer>
    </Dialog>
  );
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
            if (e.key === 'Enter' && !isImeKey(e)) {
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

function ChoiceDialog({ overlay }: { overlay: Extract<Overlay, { kind: 'choice' }> }) {
  const titleId = useId();
  return (
    <Dialog labelledBy={titleId} onClose={cancelOverlay} size="sm">
      <h2 id={titleId} className={styles.title}>
        {overlay.title}
      </h2>
      <p className={styles.message}>{overlay.message}</p>
      <footer className={styles.actions}>
        <Button onClick={cancelOverlay}>{ja.common.cancel}</Button>
        {overlay.choices.map((choice) => (
          <Button
            key={choice.value}
            variant={choice.primary ? 'primary' : 'secondary'}
            onClick={() => overlay.resolve(choice.value)}
          >
            {choice.label}
          </Button>
        ))}
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
    if (isImeKey(e)) {
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
