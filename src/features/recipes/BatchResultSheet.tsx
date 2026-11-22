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

/** 群の言葉を読む行の数（見えている群だけ。理由が何通りあるかもここから数える）。 */
const MESSAGE_ROWS = 100;

/** 群の行の言葉を、理由の見出しと添え書きにする（1 通りならその言葉、何通りもあれば最初の言葉とほかの数）。 */
export function groupNote(messages: readonly (string | null)[]): {
  first: string | null;
  note: string | null;
} {
  const distinct = [...new Set(messages.filter((m): m is string => !!m))];
  const first = distinct[0] ?? null;
  if (distinct.length <= 1) return { first, note: first };
  return { first, note: `${first}（${t.moreReasons(formatCount(distinct.length - 1))}）` };
}

/** 群の行（飛ばした・失敗は行の言葉を読んで添える。理由がアイテムごとに違えばそう言う）。 */
function ResultGroup({
  job,
  group,
  action,
}: {
  job: number;
  group: BatchGroupDto;
  action: 'apply' | 'undo' | 'merge';
}) {
  const [messages, setMessages] = useState<(string | null)[]>([]);
  const needsMessage = group.code !== null;
  useEffect(() => {
    if (!needsMessage) return;
    let alive = true;
    batchRows(job, { state: group.state, code: group.code ?? undefined }, 0, MESSAGE_ROWS)
      .then((rows) => {
        if (alive) setMessages(rows.map((r) => r.message));
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [job, group.state, group.code, needsMessage]);
  const { first, note } = groupNote(messages);
  const title = group.code
    ? `${stateLabel(group.state, action)} · ${codeLabel(group.code, first)}`
    : stateLabel(group.state, action);
  return <GroupRow title={title} ids={group.items} note={note} onSelect={closeResult} />;
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
