import { type KeyboardEvent, useEffect, useId, useState } from 'react';

import { Select, type SelectOption } from '../../components/Select/Select';
import { WarningIcon } from '../../components/icons/WarningIcon';
import { ja } from '../../i18n/ja';
import {
  type CreditPatchDto,
  type CreditPreviewDto,
  type CreditSummaryDto,
  type LicenseDto,
  previewItemCredit,
  setCredits,
  type SharedDto,
  summarizeCredits,
} from '../../ipc/credits';
import { type ItemProjectDto, itemProjects } from '../../ipc/projects';
import { formatCount } from '../../lib/format';
import { isImeKey } from '../../lib/ime';
import { notify } from '../../stores/noticeStore';
import { bumpRevision } from '../../stores/revisionStore';
import { useSettingsStore } from '../../stores/settingsStore';
import { useViewStore } from '../../stores/viewStore';
import { useLoaded } from '../library/inspector/useLoaded';
import styles from './Credit.module.css';
import { useLicenseStore } from './licenseStore';

const t = ja.credit;

/** 選択の値：未設定・ライセンスの ID・管理を開く・複数の値。 */
const UNSET = 'unset';
const MANAGE = 'manage';
const MIXED = 'mixed';

type TextKey = 'author' | 'work' | 'url' | 'credit';

/** ライセンスの印（表記・商用・改変・継承）。 */
export function licenseFlags(license: LicenseDto): string[] {
  const out: string[] = [
    t.attribution[license.attribution],
    t.commercial[license.commercial],
    t.modify[license.modify],
  ];
  if (license.shareAlike) out.push(t.shareAlike);
  return out;
}

/** ライセンスの選択肢（未設定・表の並び・管理を開く。値がばらばらなら「複数の値」）。 */
export function licenseOptions(licenses: readonly LicenseDto[], mixed: boolean): SelectOption[] {
  return [
    ...(mixed ? [{ value: MIXED, label: t.mixed, disabled: true }] : []),
    { value: UNSET, label: t.unset },
    ...licenses.map((l) => ({ value: String(l.id), label: l.name })),
    { value: MANAGE, label: t.manage },
  ];
}

/** 表記の見本の注意（自身が未設定・来歴の元に未設定・消えた元）。行がなく何も数えていなければ未設定とみなす。 */
export function previewWarnings(preview: CreditPreviewDto): string[] {
  const out: string[] = [];
  if (
    preview.unset ||
    (preview.lines.length === 0 && preview.unsetSources === 0 && preview.lost === 0)
  ) {
    out.push(t.previewUnset);
  }
  if (preview.unsetSources > 0) out.push(t.unsetSources(formatCount(preview.unsetSources)));
  if (preview.lost > 0) out.push(t.lost(formatCount(preview.lost)));
  return out;
}

/** 欄を作り直す鍵（値が外から変わったら入力を合わせる）。 */
function fieldKey(shared: SharedDto<string>): string {
  return shared.mixed ? '\u0000mixed' : (shared.value ?? '');
}

/** 1 行の文の欄（Enter・フォーカスを外したら決める。Esc で戻す。値がばらばらなら「複数の値」を薄く）。 */
function TextField({
  label,
  shared,
  placeholder,
  disabled,
  onCommit,
}: {
  label: string;
  shared: SharedDto<string>;
  placeholder: string;
  disabled: boolean;
  onCommit: (value: string) => void;
}) {
  const initial = shared.mixed ? '' : (shared.value ?? '');
  const [text, setText] = useState(initial);
  // 整えた値で比べる（空白だけは空と同じ。値がばらばらの欄は空のまま離れても消さない）
  const commit = () => {
    const v = text.trim();
    if (shared.mixed ? v === '' : v === initial.trim()) return;
    onCommit(v);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // 変換の確定の Enter・取り消しの Esc は IME に任せる（WebKit は確定の Enter を isComposing なしで送る）
    if (isImeKey(e)) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      e.currentTarget.blur();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      setText(initial);
    }
  };
  return (
    <label className={styles.field}>
      <span className={styles.fieldLabel}>{label}</span>
      <input
        className={styles.input}
        value={text}
        placeholder={shared.mixed ? t.mixed : placeholder}
        disabled={disabled}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={onKeyDown}
      />
    </label>
  );
}

/**
 * インスペクタのクレジットの欄（M15 段 6。design.md §3.1）：素材の欄の下にいつも出す。ライセンス（選ぶと表記・商用・改変・
 * 継承の印）・作者・作品・出典・表記の文。1 件なら表記の見本（来歴の元をたどる。未設定・消えた元も知らせる）と使っている
 * プロジェクトも出す。複数・ユニットはまとめて付ける（ユニットはメンバー全員。ADR-202 (1)）。対象が 0 件（メンバーのいない
 * ユニット）なら欄を押せなくして案内する。
 */
export function CreditSection({
  itemIds,
  unitIds,
  source = null,
  navigable = true,
}: {
  itemIds: readonly number[];
  unitIds: readonly number[];
  /** 1 件のときの取得元の URL（出典の既定の案内）。 */
  source?: string | null;
  /** 使っているプロジェクトを押して一覧を開けるか（エディタ・シアターの中のインスペクタでは名前だけ）。 */
  navigable?: boolean;
}) {
  const licenseId = useId();
  const licenses = useLicenseStore((s) => s.list);
  const key = `credit-${itemIds.join(',')}-${unitIds.join(',')}`;
  const single = itemIds.length === 1 && unitIds.length === 0 ? (itemIds[0] ?? null) : null;
  const summary = useLoaded<CreditSummaryDto>(key, () =>
    summarizeCredits([...itemIds], [...unitIds]),
  );
  const preview = useLoaded<CreditPreviewDto | null>(`${key}-preview`, () =>
    single === null ? Promise.resolve(null) : previewItemCredit(single),
  );
  const projects = useLoaded<ItemProjectDto[] | null>(`${key}-projects`, () =>
    single === null ? Promise.resolve(null) : itemProjects(single),
  );

  useEffect(() => {
    if (useLicenseStore.getState().list.length === 0) void useLicenseStore.getState().load();
  }, []);

  if (!summary) return null;
  const empty = summary.count === 0;
  const apply = async (patch: CreditPatchDto) => {
    try {
      const r = await setCredits([...itemIds], [...unitIds], patch);
      if (r.changed > 0 && single === null) notify(t.changed(formatCount(r.changed)));
      bumpRevision();
    } catch (e) {
      notify(e instanceof Error ? e.message : String(e));
    }
  };
  const text = (k: TextKey) => (value: string) => void apply({ [k]: value });

  const licenseValue = summary.licenseId.mixed
    ? MIXED
    : summary.licenseId.value === null
      ? UNSET
      : String(summary.licenseId.value);
  const license =
    summary.licenseId.value === null
      ? null
      : (licenses.find((l) => l.id === summary.licenseId.value) ?? null);
  const onLicense = (value: string) => {
    if (value === MANAGE) {
      useSettingsStore.getState().show('licenses');
      return;
    }
    if (value === MIXED) return;
    void apply({
      license: value === UNSET ? { type: 'unset' } : { type: 'set', id: Number(value) },
    });
  };

  return (
    <section className={styles.section} aria-label={t.section}>
      <h3 className={styles.label}>{t.section}</h3>
      {empty && <p className={styles.line}>{t.empty}</p>}
      <div className={styles.row}>
        <label className={styles.fieldLabel} htmlFor={licenseId}>
          {t.license}
        </label>
        <Select
          id={licenseId}
          disabled={empty}
          value={empty ? UNSET : licenseValue}
          options={licenseOptions(licenses, summary.licenseId.mixed)}
          onChange={onLicense}
        />
      </div>
      {license && !summary.licenseId.mixed && !empty && (
        <ul className={styles.flags} aria-label={license.name}>
          {licenseFlags(license).map((flag) => (
            <li key={flag} className={styles.flag}>
              {flag}
            </li>
          ))}
        </ul>
      )}
      <TextField
        key={`author-${fieldKey(summary.author)}`}
        disabled={empty}
        label={t.author}
        shared={summary.author}
        placeholder=""
        onCommit={text('author')}
      />
      <TextField
        key={`work-${fieldKey(summary.work)}`}
        disabled={empty}
        label={t.work}
        shared={summary.work}
        placeholder=""
        onCommit={text('work')}
      />
      <TextField
        key={`url-${fieldKey(summary.url)}`}
        disabled={empty}
        label={t.url}
        shared={summary.url}
        placeholder={source ? t.urlFallback(source) : t.urlPlaceholder}
        onCommit={text('url')}
      />
      <TextField
        key={`credit-${fieldKey(summary.credit)}`}
        disabled={empty}
        label={t.credit}
        shared={summary.credit}
        placeholder={t.creditPlaceholder}
        onCommit={text('credit')}
      />
      {single !== null && preview && (
        <div className={styles.preview}>
          <span className={styles.fieldLabel}>{t.preview}</span>
          {preview.lines.map((line, i) => (
            <p key={i} className={styles.line}>
              {line}
            </p>
          ))}
          {previewWarnings(preview).map((w) => (
            <p key={w} className={styles.unset}>
              <WarningIcon size={12} strokeWidth={2} />
              {w}
            </p>
          ))}
        </div>
      )}
      {projects && projects.length > 0 && (
        <div className={styles.preview}>
          <span className={styles.fieldLabel}>{t.projects}</span>
          <ul className={styles.projects}>
            {projects.map((p) => (
              <li key={p.projectId}>
                {navigable ? (
                  <button
                    type="button"
                    className={styles.projectLink}
                    title={p.paths.join('\n')}
                    onClick={() =>
                      useViewStore.getState().setView({ kind: 'project', id: p.projectId })
                    }
                  >
                    {p.missing ? t.projectMissing(p.name) : p.name}
                  </button>
                ) : (
                  <span className={styles.line} title={p.paths.join('\n')}>
                    {p.missing ? t.projectMissing(p.name) : p.name}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
