import { useEffect, useRef, useState } from 'react';

import { CheckIcon } from '../../components/icons/CheckIcon';
import { RatingInput } from '../../components/RatingInput/RatingInput';
import { Switch } from '../../components/Switch/Switch';
import { TagInput } from '../../components/TagInput/TagInput';
import { ja } from '../../i18n/ja';
import { type PlaylistDto, type PlaylistPatchDto, updatePlaylist } from '../../ipc/playlists';
import type { SortKey } from '../../ipc/items';
import { regenerate, updatePlaylistSettings } from './playlistActions';
import { Choice, PlaylistOptions } from './PlaylistSettings';
import {
  type KindValue,
  fromClause,
  newRow,
  type RuleField,
  type RuleRow,
  toClause,
} from './ruleRows';
import styles from './PlaylistView.module.css';

const t = ja.playlist;

/** ルールを保存するまで待つ時間（入力の途中で何度も作り直さないため）。 */
const SAVE_DELAY_MS = 600;

const FIELDS: RuleField[] = [
  'rating',
  'kind',
  'tags',
  'duration',
  'played',
  'added',
  'folder',
  'name',
  'query',
];

const OPS: { [F in RuleField]: readonly { value: string; label: string }[] } = {
  rating: [
    { value: 'ge', label: t.ops.ge },
    { value: 'le', label: t.ops.le },
    { value: 'eq', label: t.ops.eq },
  ],
  kind: [
    { value: 'has', label: t.ops.has },
    { value: 'only', label: t.ops.only },
  ],
  tags: [
    { value: 'all', label: t.ops.all },
    { value: 'any', label: t.ops.any },
    { value: 'none', label: t.ops.none },
  ],
  duration: [
    { value: 'gt', label: t.ops.gt },
    { value: 'lt', label: t.ops.lt },
  ],
  played: [
    { value: 'before', label: t.ops.before },
    { value: 'within', label: t.ops.within },
    { value: 'never', label: t.ops.never },
  ],
  added: [
    { value: 'within', label: t.ops.within },
    { value: 'before', label: t.ops.before },
  ],
  folder: [
    { value: 'in', label: t.ops.in },
    { value: 'out', label: t.ops.out },
  ],
  name: [
    { value: 'has', label: t.ops.contains },
    { value: 'not', label: t.ops.notContains },
  ],
  query: [],
};

const KINDS: KindValue[] = ['image', 'audio', 'video'];

function NumberField({
  value,
  onChange,
  label,
}: {
  value: number;
  onChange: (n: number) => void;
  label: string;
}) {
  return (
    <input
      type="number"
      min={1}
      className={styles.numberInput}
      value={value}
      aria-label={label}
      onChange={(e) => {
        const n = Number(e.target.value);
        if (Number.isFinite(n) && n >= 1) {
          onChange(Math.round(n));
        }
      }}
    />
  );
}

/** 行の値の入力（項目ごとに形が違う）。 */
function RowValue({ row, onChange }: { row: RuleRow; onChange: (row: RuleRow) => void }) {
  switch (row.field) {
    case 'rating':
      return (
        <RatingInput
          value={row.value}
          label={t.fields.rating}
          starLabel={(n) => ja.inspector.rating(n)}
          onChange={(value) => onChange({ ...row, value })}
        />
      );
    case 'kind':
      return (
        <>
          {KINDS.map((kind) => {
            const on = row.value.includes(kind);
            return (
              <button
                key={kind}
                type="button"
                className={styles.toggleChip}
                aria-pressed={on}
                onClick={() =>
                  onChange({
                    ...row,
                    value: on ? row.value.filter((k) => k !== kind) : [...row.value, kind],
                  })
                }
              >
                {on && <CheckIcon size={11} strokeWidth={3} />}
                {ja.kinds[kind]}
              </button>
            );
          })}
        </>
      );
    case 'tags':
      return (
        <TagInput
          tags={row.value}
          placeholder={t.tagPlaceholder}
          inputLabel={t.fields.tags}
          removeLabel={ja.tagDialog.remove}
          onChange={(value) => onChange({ ...row, value })}
        />
      );
    case 'duration':
      return (
        <>
          <NumberField
            value={row.minutes}
            label={t.fields.duration}
            onChange={(minutes) => onChange({ ...row, minutes })}
          />
          {t.minutes}
        </>
      );
    case 'played':
      if (row.op === 'never') {
        return null;
      }
      return (
        <>
          <NumberField
            value={row.days}
            label={t.fields.played}
            onChange={(days) => onChange({ ...row, days })}
          />
          {row.op === 'before' ? t.daysBefore : t.days}
        </>
      );
    case 'added':
      return (
        <>
          <NumberField
            value={row.days}
            label={t.fields.added}
            onChange={(days) => onChange({ ...row, days })}
          />
          {row.op === 'before' ? t.daysAgo : t.days}
        </>
      );
    case 'folder':
      return (
        <input
          className={styles.textInput}
          value={row.path}
          placeholder={t.folderPlaceholder}
          aria-label={t.fields.folder}
          onChange={(e) => onChange({ ...row, path: e.target.value })}
        />
      );
    case 'name':
      return (
        <input
          className={styles.textInput}
          value={row.text}
          placeholder={t.textPlaceholder}
          aria-label={t.fields.name}
          onChange={(e) => onChange({ ...row, text: e.target.value })}
        />
      );
    case 'query':
      return (
        <input
          className={styles.textInput}
          value={row.text}
          placeholder={t.queryPlaceholder}
          aria-label={t.fields.query}
          onChange={(e) => onChange({ ...row, text: e.target.value })}
        />
      );
  }
}

/** 行の比べ方を変える（値はできるだけ引き継ぐ）。 */
function withOp(row: RuleRow, op: string): RuleRow {
  return { ...row, op } as RuleRow;
}

/**
 * スマートプレイリストのルール（Playlist.dc.html）：「次の すべて／いずれか に一致」と条件の行、
 * その下に並び順・上限・作り直し・シードなどの設定。変えると少し待って保存し、並びを作り直す。
 */
export function SmartPanel({ playlist }: { playlist: PlaylistDto }) {
  const rule = playlist.rule ?? { match: 'all' as const, clauses: [] };
  const [rows, setRows] = useState<RuleRow[]>(() =>
    rule.clauses.length > 0 ? rule.clauses.map(fromClause) : [],
  );
  const [match, setMatch] = useState<'all' | 'any'>(rule.match);
  const [error, setError] = useState<string | null>(null);
  const dirty = useRef(false);

  useEffect(() => {
    if (!dirty.current) {
      return;
    }
    const timer = setTimeout(() => {
      dirty.current = false;
      const clauses = rows.map(toClause).filter(Boolean);
      updatePlaylist(playlist.id, { rule: { match, clauses } })
        .then(() => {
          setError(null);
          return regenerate(playlist.id);
        })
        .catch((e: unknown) => setError(t.ruleError(e instanceof Error ? e.message : String(e))));
    }, SAVE_DELAY_MS);
    return () => clearTimeout(timer);
  }, [rows, match, playlist.id]);

  const change = (next: RuleRow[]) => {
    dirty.current = true;
    setRows(next);
  };

  const update = (patch: PlaylistPatchDto) => {
    void updatePlaylistSettings(playlist.id, patch).then((ok) => {
      if (ok) {
        void regenerate(playlist.id);
      }
    });
  };

  const byUnit = playlist.granularity === 'unit';
  const limitKey =
    playlist.limit.kind === null ? 'none' : `${playlist.limit.kind}-${playlist.limit.value}`;
  const limitOptions = [
    { value: 'none', label: t.limitNone },
    ...[25, 50, 100, 200].map((n) => ({
      value: `count-${n}`,
      label: byUnit ? t.limitUnits(String(n)) : t.limitItems(String(n)),
    })),
    { value: `duration-${30 * 60_000}`, label: t.limitMinutes('30') },
    { value: `duration-${60 * 60_000}`, label: t.limitHours('1') },
    { value: `duration-${120 * 60_000}`, label: t.limitHours('2') },
  ];
  if (!limitOptions.some((o) => o.value === limitKey)) {
    limitOptions.push({
      value: limitKey,
      label:
        playlist.limit.kind === 'count'
          ? t.limitItems(String(playlist.limit.value))
          : t.limitMinutes(String(Math.round(playlist.limit.value / 60_000))),
    });
  }
  const sortKeys: SortKey[] = ['imported', 'captured', 'name', 'rating', 'duration', 'size'];
  const orderValue = playlist.shuffle
    ? playlist.weightByRating
      ? 'weighted'
      : 'random'
    : `${playlist.sort.key}-${playlist.sort.desc ? 'desc' : 'asc'}`;
  const orderOptions = [
    { value: 'weighted', label: t.orderWeighted },
    { value: 'random', label: t.orderRandom },
    ...sortKeys.flatMap((key) =>
      [true, false].map((desc) => ({
        value: `${key}-${desc ? 'desc' : 'asc'}`,
        label: t.orderSort(ja.sort.keys[key], ja.sort.orders[key][desc ? 0 : 1]),
      })),
    ),
  ];

  return (
    <section className={styles.panel} aria-label={t.rules}>
      <div className={styles.panelHead}>
        <h2 className={styles.panelTitle}>{t.rules}</h2>
        {t.matchPrefix}
        <Choice
          small
          label={t.rules}
          value={match}
          options={[
            { value: 'all', label: t.matchAll },
            { value: 'any', label: t.matchAny },
          ]}
          onChange={(m) => {
            dirty.current = true;
            setMatch(m);
          }}
        />
        {t.matchSuffix}
      </div>
      <div className={styles.rules}>
        {rows.map((row, i) => (
          // 行は並び替えないので位置で区別する
          <div key={i} className={styles.rule}>
            <Choice
              label={t.ruleField}
              value={row.field}
              options={FIELDS.map((f) => ({ value: f, label: t.fields[f] }))}
              onChange={(field) => change(rows.map((r, j) => (j === i ? newRow(field) : r)))}
            />
            {OPS[row.field].length > 0 && 'op' in row && (
              <Choice
                label={t.ruleOp}
                value={row.op}
                options={OPS[row.field]}
                onChange={(op) => change(rows.map((r, j) => (j === i ? withOp(r, op) : r)))}
              />
            )}
            <div className={styles.ruleValue}>
              <RowValue
                row={row}
                onChange={(next) => change(rows.map((r, j) => (j === i ? next : r)))}
              />
            </div>
            <span className={styles.ruleButtons}>
              <button
                type="button"
                className={styles.roundButton}
                aria-label={t.removeRule}
                onClick={() => change(rows.filter((_, j) => j !== i))}
              >
                −
              </button>
              <button
                type="button"
                className={styles.roundButton}
                aria-label={t.addRule}
                onClick={() =>
                  change([...rows.slice(0, i + 1), newRow('tags'), ...rows.slice(i + 1)])
                }
              >
                ＋
              </button>
            </span>
          </div>
        ))}
        {rows.length === 0 && (
          <div className={styles.rule}>
            <button
              type="button"
              className={styles.select}
              onClick={() => change([newRow('rating')])}
            >
              ＋ {t.addRule}
            </button>
          </div>
        )}
        {error && <p className={styles.ruleError}>{error}</p>}
      </div>
      <div className={styles.divider} />
      <PlaylistOptions playlist={playlist} onChange={() => void regenerate(playlist.id)}>
        <div className={styles.option}>
          <span>{t.order}</span>
          <Choice
            label={t.order}
            value={orderValue}
            options={orderOptions}
            onChange={(value) => {
              if (value === 'weighted' || value === 'random') {
                update({ shuffle: true, weightByRating: value === 'weighted' });
              } else {
                const [key, dir] = value.split('-') as [SortKey, string];
                update({ shuffle: false, sort: { key, desc: dir === 'desc' } });
              }
            }}
          />
        </div>
        <div className={styles.option}>
          <span>{t.limit}</span>
          <Choice
            label={t.limit}
            value={limitKey}
            options={limitOptions}
            onChange={(value) => {
              if (value === 'none') {
                update({ limit: { kind: null, value: 0 } });
              } else {
                const [kind, n] = value.split('-');
                update({
                  limit: { kind: kind === 'duration' ? 'duration' : 'count', value: Number(n) },
                });
              }
            }}
          />
        </div>
        {byUnit && (
          <div className={styles.option}>
            <span>{t.unitMatch}</span>
            <Choice
              label={t.unitMatch}
              value={playlist.unitMatch}
              options={[
                { value: 'any', label: t.unitMatchAny },
                { value: 'all', label: t.unitMatchAll },
                { value: 'unitOnly', label: t.unitMatchUnitOnly },
              ]}
              onChange={(unitMatch) => update({ unitMatch })}
            />
          </div>
        )}
        {byUnit && (
          <label className={styles.option}>
            {t.looseItems}
            <Switch
              checked={playlist.includeLooseItems}
              onChange={(on) => update({ includeLooseItems: on })}
            />
          </label>
        )}
      </PlaylistOptions>
    </section>
  );
}
