import { useId } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ProgressBar } from '../../components/ProgressBar/ProgressBar';
import { Segmented } from '../../components/Segmented/Segmented';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import type { BatchPrecheckDto, PrecheckGroupDto } from '../../ipc/batch';
import { formatBytes, formatCount } from '../../lib/format';
import {
  type ApplyMode,
  closeApply,
  expandName,
  setApplyMode,
  setIncludeDrafts,
  setNameTemplate,
  startApply,
  useApplyStore,
  validNameTemplate,
} from './applyStore';
import { boundText, codeLabel, GroupRow, lossText, shapeText, useItemTitles } from './batchParts';
import { formatLabel } from './recipeModel';
import styles from './Recipes.module.css';

const t = ja.batch;

function SkipGroups({ groups }: { groups: PrecheckGroupDto[] }) {
  if (groups.length === 0) return null;
  return (
    <ul className={styles.groups}>
      {groups.map((g) => (
        <GroupRow
          key={`${g.state}:${g.code}:${g.message}`}
          title={codeLabel(g.code, g.message)}
          ids={g.items}
          note={g.message}
          onSelect={closeApply}
        />
      ))}
    </ul>
  );
}

/** 確かめの結果。 */
function PrecheckBody({ precheck, mode }: { precheck: BatchPrecheckDto; mode: ApplyMode }) {
  const includeDrafts = useApplyStore((s) => s.includeDrafts);
  const skipped = precheck.skips.reduce((n, g) => n + g.items.length, 0);
  const drafts = precheck.skips.find((g) => g.code === 'Draft');
  const examples = precheck.outputs.flatMap((o) => o.examples.map((e) => e.itemId));
  const boundIds = precheck.bound.map((b) => b.itemId);
  const titles = useItemTitles([...examples.slice(0, 6), ...boundIds.slice(0, 3)], 9);
  const counts = [t.applicable(formatCount(precheck.apply.length))];
  if (skipped > 0) counts.push(t.notApplicable(formatCount(skipped)));
  if (precheck.missing.length > 0) counts.push(t.missing(formatCount(precheck.missing.length)));
  return (
    <>
      <p className={styles.counts}>{counts.join(' · ')}</p>
      {precheck.composed.length > 0 && (
        <p className={styles.note}>{t.composed(formatCount(precheck.composed.length))}</p>
      )}
      {mode === 'overwrite' && (drafts || includeDrafts) && (
        <div className={styles.switchRow}>
          <span className={styles.switchText}>
            <span>{t.includeDrafts(formatCount(drafts?.items.length ?? 0))}</span>
            <span className={styles.note}>{t.includeDraftsHint}</span>
          </span>
          <Switch
            checked={includeDrafts}
            onChange={setIncludeDrafts}
            aria-label={t.includeDrafts(formatCount(drafts?.items.length ?? 0))}
          />
        </div>
      )}
      {precheck.skips.length > 0 && (
        <section className={styles.field} aria-label={t.notApplicable('')}>
          <SkipGroups groups={precheck.skips} />
        </section>
      )}
      {precheck.unchecked.length > 0 && (
        <section className={styles.field}>
          <h3 className={styles.sectionTitle}>
            {t.unchecked(formatCount(precheck.unchecked.reduce((n, g) => n + g.items.length, 0)))}
          </h3>
          <SkipGroups groups={precheck.unchecked} />
        </section>
      )}
      {precheck.outputs.length > 0 && (
        <section className={styles.field}>
          <h3 className={styles.sectionTitle}>{t.outputs}</h3>
          <ul className={styles.groups}>
            {precheck.outputs.map((o) => (
              <li key={`${o.format}:${o.fallback}`} className={styles.group}>
                <div className={styles.groupHead}>
                  <span className={styles.groupTitle}>
                    {o.fallback ? t.fallback(formatLabel(o.format)) : formatLabel(o.format)}
                  </span>
                  <span className={styles.groupCount}>{t.count(formatCount(o.items.length))}</span>
                </div>
                {o.examples.map((e) => (
                  <p key={e.itemId} className={styles.example}>
                    <span className={styles.exampleName}>{titles.get(e.itemId) ?? ''}</span>
                    {`${shapeText(e.from)} → ${shapeText(e.to)}`}
                  </p>
                ))}
                {o.resized > 0 && (
                  <p className={styles.note}>{t.resized(formatCount(o.resized))}</p>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
      {precheck.losses.length > 0 && (
        <section className={styles.field}>
          <h3 className={styles.sectionTitle}>{t.losses}</h3>
          <ul className={styles.groups}>
            {precheck.losses.map((l) => (
              <GroupRow
                key={`${l.loss.type}:${l.format}`}
                title={lossText(l.loss, l.format)}
                ids={l.items}
                onSelect={closeApply}
              />
            ))}
          </ul>
        </section>
      )}
      {precheck.bound.length > 0 && (
        <section className={styles.field}>
          <h3 className={styles.sectionTitle}>{t.bound}</h3>
          {precheck.bound.map((b) => (
            <p key={b.itemId} className={styles.example}>
              <span className={styles.exampleName}>{titles.get(b.itemId) ?? ''}</span>
              {boundText(b.boundJson).join(' · ')}
            </p>
          ))}
        </section>
      )}
      {precheck.notes.length > 0 && <p className={styles.note}>{t.notes}</p>}
      <p className={styles.note}>
        {t.disk(formatBytes(precheck.disk.newBytes))}
        {precheck.disk.replacedBytes > 0 &&
          `。${t.diskReplaced(formatBytes(precheck.disk.replacedBytes))}`}
      </p>
      {precheck.memoryPeak > 0 && (
        <p className={styles.note}>{t.memory(formatBytes(precheck.memoryPeak))}</p>
      )}
    </>
  );
}

/**
 * レシピを当てるシート（M14 段 7。幅 600px）：保存のしかた（上書き｜別名で保存・名前の型）、確かめの進み（中止できる）、
 * 当てられる・当てられない（理由ごと）・確かめきれなかったもの・書く形式と形の例・失うもの・読む値の例・ディスクの目安。
 */
export function ApplySheet() {
  const phase = useApplyStore((s) => s.phase);
  const recipeName = useApplyStore((s) => s.recipeName);
  const retry = useApplyStore((s) => s.retry);
  const mode = useApplyStore((s) => s.mode);
  const nameTemplate = useApplyStore((s) => s.nameTemplate);
  const hadUnits = useApplyStore((s) => s.hadUnits);
  const done = useApplyStore((s) => s.done);
  const total = useApplyStore((s) => s.total);
  const precheck = useApplyStore((s) => s.precheck);
  const error = useApplyStore((s) => s.error);
  const starting = useApplyStore((s) => s.starting);
  const firstId = precheck?.apply[0] ?? null;
  const titles = useItemTitles(firstId === null ? [] : [firstId], 1);
  const titleId = useId();
  const nameId = useId();
  if (phase === 'closed' || phase === 'preparing') return null;
  const validName = mode === 'overwrite' || validNameTemplate(nameTemplate);
  const n = precheck?.apply.length ?? 0;
  const firstTitle = firstId === null ? undefined : titles.get(firstId);
  return (
    <Dialog labelledBy={titleId} onClose={closeApply} size="md">
      <div className={styles.sheet}>
        <h2 id={titleId} className={styles.title}>
          {retry ? t.retryTitle(recipeName) : t.title(recipeName)}
        </h2>
        <div className={styles.body}>
          <Segmented<ApplyMode>
            label={t.mode}
            value={mode}
            options={[
              { value: 'overwrite', label: t.overwrite },
              { value: 'saveAs', label: t.saveAs },
            ]}
            onChange={setApplyMode}
          />
          {mode === 'saveAs' && (
            <div className={styles.field}>
              <label htmlFor={nameId} className={styles.label}>
                {t.nameTemplate}
              </label>
              <input
                id={nameId}
                className={styles.input}
                value={nameTemplate}
                spellCheck={false}
                autoComplete="off"
                onChange={(e) => setNameTemplate(e.target.value)}
              />
              {validName ? (
                <p className={styles.note}>
                  {t.nameTemplateHint}
                  {firstTitle !== undefined &&
                    ` · ${t.nameExample(expandName(nameTemplate, firstTitle))}`}
                </p>
              ) : (
                <p className={styles.error} role="alert">
                  {ja.editor.saveAsBadName}
                </p>
              )}
            </div>
          )}
          {hadUnits && <p className={styles.note}>{t.units}</p>}
          {phase === 'checking' && (
            <div className={styles.checking} role="status">
              <span>
                {t.checking} {t.checkingCount(formatCount(done), formatCount(total))}
              </span>
              <ProgressBar value={total > 0 ? done / total : null} label={t.checking} />
            </div>
          )}
          {phase === 'error' && (
            <p className={styles.error} role="alert">
              {t.checkFailed}：{error}
            </p>
          )}
          {phase === 'ready' && precheck && <PrecheckBody precheck={precheck} mode={mode} />}
          {phase === 'ready' && error && (
            <p className={styles.error} role="alert">
              {error}
            </p>
          )}
          {phase === 'ready' && n === 0 && <p className={styles.note}>{t.noneApplicable}</p>}
        </div>
        <footer className={styles.actions}>
          <Button onClick={closeApply}>{t.cancel}</Button>
          <Button
            variant="primary"
            disabled={phase !== 'ready' || n === 0 || starting || !validName}
            onClick={() => void startApply()}
          >
            {mode === 'overwrite'
              ? t.startOverwrite(formatCount(n))
              : t.startSaveAs(formatCount(n))}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
