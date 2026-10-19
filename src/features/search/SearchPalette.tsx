import {
  type KeyboardEvent,
  type ReactNode,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from 'react';

import { CloseIcon } from '../../components/icons/CloseIcon';
import { SearchIcon } from '../../components/icons/SearchIcon';
import { SparkleIcon } from '../../components/icons/SparkleIcon';
import { TagIcon } from '../../components/icons/TagIcon';
import { ja } from '../../i18n/ja';
import { type ListView, utcOffsetMinutes } from '../../ipc/items';
import { kindBreakdown } from '../../ipc/kinds';
import { type QueryCheckDto, querySuggest, queryParse, type SuggestionDto } from '../../ipc/search';
import { formatCount } from '../../lib/format';
import { useSearchStore } from '../../stores/searchStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { useViewStore } from '../../stores/viewStore';
import { viewTitle } from '../library/viewTitle';
import { startPlayback } from '../player/playerActions';
import { clauseParts, currentWord, isComplete, joinClauses } from './clauses';
import styles from './SearchPalette.module.css';
import { saveAsSmartFolder, updateSmartQuery } from './searchActions';
import { isImeKey } from '../../lib/ime';

const t = ja.search;

/** 入力が止まってから解析・候補を取るまでの時間（search-query.md §6：50ms）。 */
const DEBOUNCE_MS = 50;

/** ⌘K パレット（Search.dc.html）。 */
export function SearchPalette() {
  const open = useSearchStore((s) => s.open);
  return open ? <PaletteBody /> : null;
}

/** チップの表示（フィールド名・除外は薄く前置）。 */
function ChipLabel({ text }: { text: string }) {
  const parts = clauseParts(text);
  if (parts.field) {
    const value =
      parts.op === ':' || parts.op === '='
        ? parts.value.split(',').join(t.or)
        : `${parts.op}${parts.value}`;
    return (
      <>
        <span className={styles.chipPrefix}>
          {parts.negated ? `${t.excluded} ${parts.name}` : parts.name}
        </span>
        {value}
      </>
    );
  }
  if (parts.negated) {
    return (
      <>
        <span className={styles.chipPrefix}>{t.excluded}</span>
        {parts.value}
      </>
    );
  }
  return <>{text}</>;
}

function PaletteBody() {
  const { initial, editing, createSmart, close } = useSearchStore.getState();
  const view = useViewStore((s) => s.view);
  const filter = useViewStore((s) => s.filter);
  const titleId = useId();
  const listId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [chips, setChips] = useState<string[]>(() =>
    initial.length > 0 ? initial : editing || createSmart ? [] : filter,
  );
  const [draft, setDraft] = useState('');
  const [check, setCheck] = useState<QueryCheckDto | null>(null);
  const [suggestions, setSuggestions] = useState<SuggestionDto[]>([]);
  const [active, setActive] = useState(-1);

  // スマートフォルダの条件は、開いている一覧ではなくライブラリ全体で数える
  const scope: ListView = useMemo(
    () => (editing || createSmart ? { kind: 'all' } : view),
    [editing, createSmart, view],
  );
  const committed = joinClauses(chips);
  const full = joinClauses([...chips, draft]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    let alive = true;
    const timer = setTimeout(() => {
      queryParse(full, scope)
        .then((result) => {
          if (!alive) {
            return;
          }
          setCheck(result);
          // 確定したチップは、解析した条件の区切りに合わせる（「海」「| 港」→「海 | 港」）
          if (!draft.trim() && !result.error) {
            const texts = result.clauses.map((c) => c.text);
            if (texts.join('\u0000') !== chips.join('\u0000')) {
              setChips(texts);
            }
          }
        })
        .catch((error: unknown) => console.warn('条件を解析できませんでした', error));
    }, DEBOUNCE_MS);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [full, scope, draft, chips]);

  const word = currentWord(draft);
  useEffect(() => {
    let alive = true;
    const timer = setTimeout(() => {
      querySuggest(word.word, 8)
        .then((found) => {
          if (alive) {
            setSuggestions(found);
            setActive(-1);
          }
        })
        .catch((error: unknown) => console.warn('候補を探せませんでした', error));
    }, DEBOUNCE_MS);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [word.word]);

  const commitDraft = (text: string) => {
    const value = text.trim();
    if (value) {
      setChips((c) => [...c, value]);
    }
    setDraft('');
  };

  const accept = (s: SuggestionDto) => {
    if (s.kind === 'saved' && s.id !== null) {
      close();
      useViewStore.getState().setView({ kind: 'smart', id: s.id });
      return;
    }
    const next = draft.slice(0, word.start) + s.insert;
    if (s.kind === 'field' || !isComplete(next)) {
      // フィールド名を選んだら続けて値を入れる
      setDraft(next);
    } else {
      commitDraft(next);
    }
    inputRef.current?.focus();
  };

  const finalText = () => joinClauses([...chips, draft]);

  const apply = async () => {
    const text = finalText();
    const parsed = await queryParse(text, scope).catch(() => null);
    if (parsed?.error) {
      setCheck(parsed);
      return;
    }
    if (editing) {
      if (await updateSmartQuery(editing.id, editing.name, text)) {
        close();
      }
      return;
    }
    if (createSmart) {
      if (await saveAsSmartFolder(text)) {
        close();
      }
      return;
    }
    useViewStore.getState().setFilter(parsed ? parsed.clauses.map((c) => c.text) : [...chips]);
    close();
  };

  /** ⇧↵：開いている一覧の中で条件に合うものを、ランダムな順で再生する（docs/search-query.md §6）。 */
  const randomPlay = async () => {
    const text = finalText();
    const parsed = await queryParse(text, scope).catch(() => null);
    if (parsed?.error) {
      setCheck(parsed);
      return;
    }
    close();
    const { collapseUnits } = useViewStore.getState();
    const { folders, smartFolders } = useSidebarStore.getState();
    void startPlayback(
      {
        kind: 'view',
        view: scope,
        query: text,
        collapseUnits,
        utcOffsetMinutes: utcOffsetMinutes(),
      },
      viewTitle(scope, folders, smartFolders),
      { shuffle: true },
    );
  };

  const save = async () => {
    const text = finalText();
    const parsed = await queryParse(text, scope).catch(() => null);
    if (parsed?.error) {
      setCheck(parsed);
      return;
    }
    const ok = editing
      ? await updateSmartQuery(editing.id, editing.name, text)
      : await saveAsSmartFolder(text);
    if (ok) {
      close();
    }
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (isImeKey(e)) {
      return;
    }
    const caretAtEnd = e.currentTarget.selectionStart === draft.length;
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
    } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      if (suggestions.length > 0) {
        // -1（どれも選んでいない）から候補の数 - 1 までを回る
        const step = e.key === 'ArrowDown' ? 1 : -1;
        const size = suggestions.length + 1;
        setActive((i) => ((i + 1 + step + size) % size) - 1);
      }
    } else if (
      (e.key === 'Tab' || (e.key === 'ArrowRight' && caretAtEnd)) &&
      suggestions.length > 0 &&
      draft
    ) {
      e.preventDefault();
      const s = suggestions[active >= 0 ? active : 0];
      if (s) {
        accept(s);
      }
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (e.metaKey) {
        void save();
      } else if (e.shiftKey && !editing && !createSmart) {
        void randomPlay();
      } else if (active >= 0 && suggestions[active]) {
        accept(suggestions[active]);
      } else {
        void apply();
      }
    } else if (e.key === ' ' && caretAtEnd && isComplete(draft)) {
      e.preventDefault();
      commitDraft(draft);
    } else if (e.key === 'Backspace' && draft === '' && chips.length > 0) {
      e.preventDefault();
      setChips((c) => c.slice(0, -1));
    }
  };

  // 解析エラーの位置：入力中の部分なら下線、確定したチップならそのチップを赤い縁にする
  const error = check?.error ?? null;
  const draftOffset = committed.length + (committed && draft ? 1 : 0);
  const errorInDraft = error && error.start >= draftOffset ? error : null;
  const chipErrors = chips.map((chip, i) => {
    const before = joinClauses(chips.slice(0, i));
    const start = before.length + (before ? 1 : 0);
    return (
      error !== null && !errorInDraft && error.start < start + chip.length && error.end > start
    );
  });

  const counts = check?.counts ?? null;
  // 解析で決まるフィールドを使っていて、解析がまだで一致が決まっていないものがあるときだけ（design.md §3.1）
  const unanalyzed = check?.unanalyzed ?? 0;
  const breakdown = counts
    ? kindBreakdown(counts)
        .map(({ label, count }) => `${label} ${formatCount(count)}`)
        .join(' · ')
    : '';

  const groups = useMemo(() => {
    const by = (kinds: string[]) =>
      suggestions.map((s, index) => ({ s, index })).filter(({ s }) => kinds.includes(s.kind));
    return [
      { title: t.sections.tags, rows: by(['tag']) },
      { title: t.sections.fields, rows: by(['field']) },
      { title: t.sections.values, rows: by(['value']) },
      { title: t.sections.saved, rows: by(['saved']) },
    ].filter((g) => g.rows.length > 0);
  }, [suggestions]);

  const activeId = active >= 0 ? `${listId}-${active}` : undefined;
  const iconFor = (s: SuggestionDto): ReactNode =>
    s.kind === 'saved' ? (
      <SparkleIcon size={16} strokeWidth={1.8} />
    ) : (
      <TagIcon size={16} strokeWidth={1.8} />
    );

  return (
    // 暗幕を押したら閉じる（キーボードでは esc）
    // eslint-disable-next-line jsx-a11y/no-static-element-interactions
    <div
      className={styles.scrim}
      data-sheet=""
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) {
          close();
        }
      }}
    >
      <div role="dialog" aria-modal="true" aria-labelledby={titleId} className={styles.palette}>
        <h2 id={titleId} className={styles.visuallyHidden}>
          {editing ? t.keys.update(editing.name) : createSmart ? t.saveTitle : t.label}
        </h2>
        <div className={styles.inputRow}>
          <span className={styles.searchIcon}>
            <SearchIcon size={18} strokeWidth={1.9} />
          </span>
          {chips.map((chip, i) => (
            <span
              key={`${i}-${chip}`}
              className={styles.chip}
              data-excluded={clauseParts(chip).negated || undefined}
              data-error={chipErrors[i] || undefined}
            >
              <ChipLabel text={chip} />
              <button
                type="button"
                className={styles.chipRemove}
                aria-label={t.removeClause(chip)}
                onClick={() => {
                  setChips((c) => c.filter((_, k) => k !== i));
                  inputRef.current?.focus();
                }}
              >
                <CloseIcon size={10} strokeWidth={3} />
              </button>
            </span>
          ))}
          <span className={styles.inputWrap}>
            {/* 解析エラーの位置に下線を引くための写し（入力欄の後ろに重ねる） */}
            <span className={styles.mirror} aria-hidden="true">
              {errorInDraft ? (
                <>
                  {draft.slice(0, errorInDraft.start - draftOffset)}
                  <mark className={styles.errorMark}>
                    {draft.slice(
                      errorInDraft.start - draftOffset,
                      Math.max(
                        errorInDraft.end - draftOffset,
                        errorInDraft.start - draftOffset + 1,
                      ),
                    ) || ' '}
                  </mark>
                  {draft.slice(errorInDraft.end - draftOffset)}
                </>
              ) : (
                draft
              )}
            </span>
            <input
              ref={inputRef}
              className={styles.input}
              value={draft}
              aria-label={t.inputLabel}
              role="combobox"
              aria-expanded={suggestions.length > 0}
              aria-controls={listId}
              aria-activedescendant={activeId}
              aria-invalid={error !== null}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={onKeyDown}
            />
          </span>
        </div>

        <div className={styles.status}>
          <span>
            {counts && (
              <>
                {t.matched} <strong className={styles.count}>{formatCount(counts.total)}</strong>{' '}
                {t.matchedUnit}
                {breakdown && ` — ${breakdown}`}
              </>
            )}
          </span>
          {error ? (
            <span className={styles.error} role="alert">
              {error.message}
            </span>
          ) : (
            <span className={styles.statusEnd}>
              {unanalyzed > 0 && (
                <span className={styles.statusHint} title={t.unanalyzedHint}>
                  {t.unanalyzed(formatCount(unanalyzed))}
                </span>
              )}
              {suggestions.length > 0 && draft && (
                <span className={styles.statusHint}>{t.typing}</span>
              )}
            </span>
          )}
        </div>

        <div id={listId} role="listbox" aria-label={t.label} className={styles.body}>
          {groups.map((group) => (
            <div key={group.title} role="group" aria-label={group.title}>
              <div className={styles.sectionTitle}>{group.title}</div>
              {group.rows.map(({ s, index }) => (
                // 候補はマウスでも選べる。キーボードでは入力欄から ↑↓・Tab・Return で選ぶ
                // eslint-disable-next-line jsx-a11y/click-events-have-key-events
                <div
                  key={`${s.kind}-${s.insert}-${index}`}
                  id={`${listId}-${index}`}
                  role="option"
                  aria-selected={index === active}
                  tabIndex={-1}
                  className={styles.option}
                  data-saved={s.kind === 'saved' || undefined}
                  onMouseDown={(e) => e.preventDefault()}
                  onMouseEnter={() => setActive(index)}
                  onClick={() => accept(s)}
                >
                  <span className={styles.optionIcon} data-active={index === active || undefined}>
                    {iconFor(s)}
                  </span>
                  <span className={styles.optionText}>
                    <span className={styles.optionLabel}>
                      {s.prefix && <span className={styles.chipPrefix}>{s.prefix}</span>}
                      {s.label}
                    </span>
                    {/* 保存済みの検索は条件、比較だけのフィールド（ラウドネスなど）は書き方の例 */}
                    {(s.kind === 'saved' || s.kind === 'field') && s.detail && (
                      <span className={styles.optionQuery}>{s.detail}</span>
                    )}
                  </span>
                  {s.kind === 'saved' ? (
                    <span className={styles.optionMeta}>{t.smartFolder}</span>
                  ) : (
                    s.count !== null && (
                      <span className={styles.optionMeta}>{formatCount(s.count)}</span>
                    )
                  )}
                  {index === active && <kbd className={styles.kbd}>↵</kbd>}
                </div>
              ))}
            </div>
          ))}
          {!draft && (
            <div>
              <div className={styles.sectionTitle}>{t.help}</div>
              <div className={styles.help}>
                {t.helpItems.map(([code, label]) => (
                  <span key={code} className={styles.helpItem}>
                    <code className={styles.code}>{code}</code>
                    {label}
                  </span>
                ))}
              </div>
            </div>
          )}
        </div>

        <footer className={styles.footer}>
          <span className={styles.hint}>
            <kbd className={styles.kbd}>↵</kbd>
            {editing ? t.keys.update(editing.name) : createSmart ? t.keys.save : t.keys.apply}
          </span>
          {!editing && !createSmart && (
            <span className={styles.hint}>
              <kbd className={styles.kbd}>⇧↵</kbd>
              {t.keys.randomPlay}
            </span>
          )}
          {!editing && !createSmart && (
            <span className={styles.hint}>
              <kbd className={styles.kbd}>⌘↵</kbd>
              {t.keys.save}
            </span>
          )}
          <span className={`${styles.hint} ${styles.hintEnd}`}>
            <kbd className={styles.kbd}>esc</kbd>
            {t.keys.close}
          </span>
        </footer>
      </div>
    </div>
  );
}
