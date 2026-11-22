import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { Segmented } from '../../components/Segmented/Segmented';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import {
  type CreditsFormatDto,
  type CreditsLangDto,
  type CreditsRenderDto,
  type CreditsScopeDto,
  type CreditsTextDto,
  DEFAULT_CREDITS_RENDER,
  renderCredits,
  saveCredits,
} from '../../ipc/credits';
import { pickCreditsPath } from '../../ipc/dialogs';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import styles from '../export/Export.module.css';
import { useProjectStore } from '../projects/projectStore';
import { closeCreditsSheet, useCreditsSheetStore } from './creditsSheetStore';

const t = ja.creditsSheet;

/** 前回の一覧の作り方を覚える鍵（表示の設定と同じ軽いもの）。 */
const OPTIONS_KEY = 'kura.credits.options';

function lastOptions(): CreditsRenderDto {
  try {
    const raw = localStorage.getItem(OPTIONS_KEY);
    if (!raw) return DEFAULT_CREDITS_RENDER;
    return { ...DEFAULT_CREDITS_RENDER, ...(JSON.parse(raw) as Partial<CreditsRenderDto>) };
  } catch {
    return DEFAULT_CREDITS_RENDER;
  }
}

function remember(options: CreditsRenderDto): void {
  try {
    localStorage.setItem(OPTIONS_KEY, JSON.stringify(options));
  } catch {
    // 覚えられなくても作れる
  }
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** 範囲の言い方（プロジェクト・選んだもの）。 */
function scopeLabel(scope: CreditsScopeDto, name: string | null): string {
  if (scope.type === 'project') return t.scopeProject(name ?? '');
  return t.scopeItems(formatCount(scope.itemIds.length + scope.unitIds.length));
}

/**
 * クレジットの一覧のシート（M15 段 6。design.md §3.1。ADR-206）：範囲（プロジェクトで使っている素材か選んだもの）・言葉・
 * 書式・任意と不要を載せるか・素材の名前を添えるか。見本と行・要確認・載せなかった数を出し、コピーか保存（保存のダイアログ。
 * プロジェクトならそのフォルダの CREDITS.md）。
 */
export function CreditsSheet() {
  const scope = useCreditsSheetStore((s) => s.scope);
  const name = useCreditsSheetStore((s) => s.name);
  if (!scope) return null;
  return <Sheet key={JSON.stringify(scope)} scope={scope} name={name} />;
}

function Sheet({ scope, name }: { scope: CreditsScopeDto; name: string | null }) {
  const titleId = useId();
  const [options, setOptions] = useState<CreditsRenderDto>(lastOptions);
  // 作り方ごとの結果（今の作り方と違う結果は出さない＝作っています…）
  const [made, setMade] = useState<{ key: string; result: CreditsTextDto } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const optionsKey = JSON.stringify(options);
  const result = made?.key === optionsKey ? made.result : null;
  const root = useProjectStore((s) =>
    scope.type === 'project' ? (s.list.find((p) => p.id === scope.id)?.root ?? null) : null,
  );

  useEffect(() => {
    let stale = false;
    const key = JSON.stringify(options);
    renderCredits(scope, options)
      .then((r) => {
        if (stale) return;
        setMade({ key, result: r });
        setError(null);
      })
      .catch((e: unknown) => {
        if (!stale) setError(messageOf(e));
      });
    return () => {
      stale = true;
    };
  }, [scope, options]);

  const change = (patch: Partial<CreditsRenderDto>) => {
    const next = { ...options, ...patch };
    setOptions(next);
    remember(next);
  };

  const copy = async () => {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.text);
      notify(t.copied);
    } catch (e) {
      notify(messageOf(e));
    }
  };

  const save = async () => {
    const markdown = options.format === 'markdown';
    const file = markdown ? 'CREDITS.md' : 'CREDITS.txt';
    const path = await pickCreditsPath(
      t.saveTitle,
      root ? `${root}/${file}` : file,
      markdown ? { name: t.filterMarkdown, ext: 'md' } : { name: t.filterText, ext: 'txt' },
    );
    if (!path) return;
    try {
      await saveCredits(path, scope, options);
      notify(t.saved(path.split('/').pop() ?? path));
    } catch (e) {
      notify(messageOf(e));
    }
  };

  const omitted = (result?.omittedOptional ?? 0) + (result?.omittedNone ?? 0);
  return (
    <Dialog labelledBy={titleId} onClose={closeCreditsSheet}>
      <div className={styles.head}>
        <h2 id={titleId} className={styles.title}>
          {t.title}
        </h2>
        <p className={styles.sub}>{scopeLabel(scope, name)}</p>
      </div>
      <div className={styles.body}>
        <div className={styles.group}>
          <div className={styles.row}>
            <span className={styles.rowLabel}>{t.lang}</span>
            <Segmented<CreditsLangDto>
              label={t.lang}
              value={options.lang}
              options={[
                { value: 'ja', label: t.langs.ja },
                { value: 'en', label: t.langs.en },
              ]}
              onChange={(lang) => change({ lang })}
            />
          </div>
          <div className={styles.row}>
            <span className={styles.rowLabel}>{t.format}</span>
            <Segmented<CreditsFormatDto>
              label={t.format}
              value={options.format}
              options={[
                { value: 'markdown', label: t.formats.markdown },
                { value: 'text', label: t.formats.text },
              ]}
              onChange={(format) => change({ format })}
            />
          </div>
          <label className={styles.row}>
            <span className={styles.rowLabel}>{t.includeOptional}</span>
            <Switch
              checked={options.includeOptional}
              onChange={(includeOptional) => change({ includeOptional })}
            />
          </label>
          <label className={styles.row}>
            <span className={styles.rowLabel}>{t.includeNone}</span>
            <Switch
              checked={options.includeNone}
              onChange={(includeNone) => change({ includeNone })}
            />
          </label>
          <label className={styles.row}>
            <span className={styles.rowLabel}>{t.listItems}</span>
            <Switch checked={options.listItems} onChange={(listItems) => change({ listItems })} />
          </label>
        </div>
        {error ? (
          <p className={styles.error}>{error}</p>
        ) : result ? (
          <>
            <p className={styles.stats} aria-live="polite">
              {[
                <span key="lines">{t.summary(formatCount(result.lines))}</span>,
                result.warnings > 0 ? (
                  <span key="warn" className={styles.warn}>
                    {t.warnings(formatCount(result.warnings))}
                  </span>
                ) : null,
                omitted > 0 ? <span key="omit">{t.omitted(formatCount(omitted))}</span> : null,
              ]
                .filter(Boolean)
                .flatMap((part, k) => (k === 0 ? [part] : [' · ', part]))}
            </p>
            <pre className={styles.text}>{result.text}</pre>
          </>
        ) : (
          <p className={styles.hint}>{t.loading}</p>
        )}
      </div>
      <div className={styles.actions}>
        <Button onClick={closeCreditsSheet}>{ja.common.close}</Button>
        <Button disabled={!result} onClick={() => void copy()}>
          {t.copy}
        </Button>
        <Button variant="primary" disabled={!result} onClick={() => void save()}>
          {t.save}
        </Button>
      </div>
    </Dialog>
  );
}
