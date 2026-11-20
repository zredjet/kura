import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ja } from '../../i18n/ja';
import { type BatchGroupDto, batchRows } from '../../ipc/batch';
import { formatCount } from '../../lib/format';
import { codeLabel, GroupRow, selectInList, stateLabel } from './batchParts';
import { closeResult, retryBatch, retryItems, undoFromResult, useResultStore } from './resultStore';
import styles from './Recipes.module.css';

const t = ja.batch;

/** 群の行（飛ばした・失敗は最初の行の言葉を読んで添える）。 */
function ResultGroup({
  job,
  group,
  action,
}: {
  job: number;
  group: BatchGroupDto;
  action: 'apply' | 'undo' | 'merge';
}) {
  const [message, setMessage] = useState<string | null>(null);
  const needsMessage = group.code !== null;
  useEffect(() => {
    if (!needsMessage) return;
    let alive = true;
    batchRows(job, { state: group.state, code: group.code ?? undefined }, 0, 1)
      .then((rows) => {
        if (alive) setMessage(rows[0]?.message ?? null);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [job, group.state, group.code, needsMessage]);
  const title = group.code
    ? `${stateLabel(group.state, action)} · ${codeLabel(group.code, message)}`
    : stateLabel(group.state, action);
  return <GroupRow title={title} ids={group.items} note={message} onSelect={closeResult} />;
}

/**
 * 一括の結果のシート（M14 段 7）：対象の件数・状態と理由ごとの群（［一覧で選ぶ］）・警告・作ったアイテムを選ぶ・
 * ［飛ばしたもの・失敗したものをもう一度…］（その一括の写しで当てる）・［元に戻す…］（元に戻せる件数があるとき）。
 */
export function BatchResultSheet() {
  const job = useResultStore((s) => s.job);
  const result = useResultStore((s) => s.result);
  const error = useResultStore((s) => s.error);
  const titleId = useId();
  if (job === null) return null;
  const actionType = result?.action.type ?? 'overwrite';
  const action = actionType === 'undo' ? 'undo' : actionType === 'merge' ? 'merge' : 'apply';
  const title =
    action === 'undo'
      ? t.undoTitle
      : action === 'merge'
        ? t.mergeTitle
        : t.resultTitle(result?.recipe ?? t.unnamed);
  const retry = result ? retryItems(result) : [];
  const created = result?.created ?? [];
  return (
    <Dialog labelledBy={titleId} onClose={closeResult} size="md">
      <div className={styles.sheet}>
        <h2 id={titleId} className={styles.title}>
          {title}
        </h2>
        <div className={styles.body}>
          {!result && !error && <p className={styles.note}>{t.resultLoading}</p>}
          {error && (
            <p className={styles.error} role="alert">
              {t.resultFailed}：{error}
            </p>
          )}
          {result && (
            <>
              <p className={styles.counts}>{t.resultSummary(formatCount(result.total))}</p>
              {result.undoneBy.length > 0 && <p className={styles.note}>{t.undoneBy}</p>}
              <ul className={styles.groups}>
                {result.groups.map((g) => (
                  <ResultGroup
                    key={`${g.state}:${g.code ?? ''}`}
                    job={result.job}
                    group={g}
                    action={action}
                  />
                ))}
              </ul>
              {result.warnings.length > 0 && (
                <section className={styles.field}>
                  <h3 className={styles.sectionTitle}>{t.warnings}</h3>
                  {result.warnings.map((w) => (
                    <p key={w} className={styles.note}>
                      {w}
                    </p>
                  ))}
                </section>
              )}
            </>
          )}
        </div>
        <footer className={styles.actions}>
          {result && result.undoAvailable > 0 && (
            <Button className={styles.left} onClick={() => void undoFromResult(result)}>
              {t.undo}
            </Button>
          )}
          {result && retry.length > 0 && (
            <Button onClick={() => retryBatch(result)}>{t.retry}</Button>
          )}
          {created.length > 0 && (
            <Button
              onClick={() => {
                closeResult();
                selectInList(created);
              }}
            >
              {action === 'merge' ? t.selectSheet : t.selectCreated(formatCount(created.length))}
            </Button>
          )}
          <Button variant="primary" onClick={closeResult}>
            {t.close}
          </Button>
        </footer>
      </div>
    </Dialog>
  );
}
