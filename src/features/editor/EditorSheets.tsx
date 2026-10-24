import { useEffect, useId, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ja } from '../../i18n/ja';
import type { EditVersionDto } from '../../ipc/edit';
import { relativeDateTime } from '../../lib/dates';
import { formatBytes, formatCount } from '../../lib/format';
import { isImeKey } from '../../lib/ime';
import {
  answerDraft,
  answerUnsaved,
  askRestore,
  confirmRevert,
  confirmWarnings,
  dismissSheet,
  loadVersions,
  saveFailureText,
  submitSaveAs,
  validSaveAsName,
} from './editorSave';
import { type EditorSheet, hasUnsavedChanges, useEditorStore } from './editorStore';
import styles from './Editor.module.css';
import { effectiveFormat, outputChoice, savedExt } from './formats';
import { opName } from './opsCatalog';
import { issueText } from './ParamFields';

const t = ja.editor;

/** 形式の名前。 */
function formatName(format: string | null): string {
  return format ? (t.formats[format] ?? format.toUpperCase()) : '—';
}

/** 版の一覧を読む（読めなければ空）。 */
function useVersions(): EditVersionDto[] | null {
  const [versions, setVersions] = useState<EditVersionDto[] | null>(null);
  useEffect(() => {
    let alive = true;
    loadVersions().then(
      (list) => {
        if (alive) setVersions(list);
      },
      () => {
        if (alive) setVersions([]);
      },
    );
    return () => {
      alive = false;
    };
  }, []);
  return versions;
}

/** 下書きのシート：「前回の編集の下書きがあります（日時）」［捨てる］［下書きから開く］。土台が変わっていれば捨てるだけ。 */
function DraftSheet() {
  const draft = useEditorStore((s) => s.draft);
  const titleId = useId();
  if (!draft) return null;
  const when = draft.updatedAt !== null ? relativeDateTime(draft.updatedAt) : '';
  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="sm">
      <h2 id={titleId} className={styles.sheetTitle}>
        {t.draftTitle}
      </h2>
      <p className={styles.sheetMessage}>{t.draftMessage(when)}</p>
      <p className={styles.sheetNote}>{draft.fresh ? t.draftHistory : t.draftStale}</p>
      <footer className={styles.sheetActions}>
        <Button
          variant={draft.fresh ? 'secondary' : 'primary'}
          onClick={() => answerDraft('discard')}
        >
          {t.draftDiscard}
        </Button>
        {draft.fresh && (
          <Button variant="primary" onClick={() => answerDraft('open')}>
            {t.draftOpen}
          </Button>
        )}
      </footer>
    </Dialog>
  );
}

/** 未保存で閉じるとき：［保存しない］［キャンセル］［保存］。 */
function UnsavedSheet() {
  const title = useEditorStore((s) => s.item?.title ?? '');
  const titleId = useId();
  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="sm">
      <h2 id={titleId} className={styles.sheetTitle}>
        {t.unsavedTitle}
      </h2>
      <p className={styles.sheetMessage}>{t.unsavedMessage(title)}</p>
      <footer className={styles.sheetActions}>
        <Button className={styles.sheetLeft} onClick={() => answerUnsaved('discard')}>
          {t.dontSave}
        </Button>
        <Button onClick={() => answerUnsaved('cancel')}>{ja.common.cancel}</Button>
        <Button variant="primary" onClick={() => answerUnsaved('save')}>
          {t.save}
        </Button>
      </footer>
    </Dialog>
  );
}

/** 警告があるときの保存の確認：警告を並べる。 */
function WarningsSheet() {
  const warnings = useEditorStore((s) => s.warnings);
  const nodes = useEditorStore((s) => s.doc.nodes);
  const titleId = useId();
  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="sm">
      <h2 id={titleId} className={styles.sheetTitle}>
        {t.warningsTitle}
      </h2>
      <ul className={styles.sheetList}>
        {warnings.map((w, i) => {
          const node = w.node === null ? undefined : nodes[w.node];
          return <li key={i}>{t.warningLine(node ? opName(node.op) : null, issueText(w))}</li>;
        })}
      </ul>
      <p className={styles.sheetMessage}>{t.warningsMessage}</p>
      <footer className={styles.sheetActions}>
        <Button onClick={dismissSheet}>{ja.common.cancel}</Button>
        <Button variant="primary" onClick={confirmWarnings}>
          {t.saveAnyway}
        </Button>
      </footer>
    </Dialog>
  );
}

/** 別名で保存：名前のシート（ファイル名は名前と出力の形式の拡張子）。 */
function SaveAsSheet() {
  const item = useEditorStore((s) => s.item);
  const outputs = useEditorStore((s) => s.doc.outputs);
  const source = useEditorStore((s) => s.source);
  const titleId = useId();
  const inputId = useId();
  const [name, setName] = useState(() => t.saveAsDefault(item?.title ?? ''));
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);
  const eff = effectiveFormat(outputChoice(outputs), source?.format ?? null);
  const ext = savedExt(eff.format, item?.ext ?? '');
  const trimmed = name.trim();
  const valid = validSaveAsName(trimmed);
  const submit = () => {
    if (valid) submitSaveAs(trimmed);
  };
  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="sm">
      <h2 id={titleId} className={styles.sheetTitle}>
        {t.saveAsTitle}
      </h2>
      <div className={styles.sheetField}>
        <label htmlFor={inputId} className={styles.sheetLabel}>
          {t.saveAsName}
        </label>
        <input
          ref={inputRef}
          id={inputId}
          className={styles.sheetInput}
          value={name}
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !isImeKey(e)) {
              e.preventDefault();
              submit();
            }
          }}
        />
        {trimmed.includes('/') ? (
          <p className={styles.paramIssue} role="alert">
            {t.saveAsBadName}
          </p>
        ) : (
          trimmed && <p className={styles.sheetNote}>{t.saveAsFile(`${trimmed}.${ext}`)}</p>
        )}
      </div>
      <p className={styles.sheetNote}>{t.saveAsNote}</p>
      <footer className={styles.sheetActions}>
        <Button onClick={dismissSheet}>{ja.common.cancel}</Button>
        <Button variant="primary" disabled={!valid} onClick={submit}>
          {t.save}
        </Button>
      </footer>
    </Dialog>
  );
}

/** 戻せる版があるか（版の一覧の頭が版 1 以降）。 */
export function canRevert(versions: readonly EditVersionDto[]): boolean {
  return versions.some((v) => v.version > 0);
}

/** 元に戻す・途中の版に戻すの確認。別名で作ったアイテムは Conflict になりうることを添える。 */
function RevertSheet({ to }: { to: number }) {
  const unsaved = useEditorStore((s) => hasUnsavedChanges(s));
  const versions = useVersions();
  const titleId = useId();
  const ready = versions !== null;
  const possible = ready && canRevert(versions);
  const label = to === 0 ? t.revertTitle : t.versionRestoreTitle(formatCount(to));
  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="sm">
      <h2 id={titleId} className={styles.sheetTitle}>
        {label}
      </h2>
      {ready && !possible ? (
        <p className={styles.sheetMessage}>{t.versionsEmpty}</p>
      ) : (
        <>
          <p className={styles.sheetMessage}>
            {to === 0 ? t.revertMessage : t.versionRestoreMessage(formatCount(to))}
          </p>
          {to === 0 && unsaved && <p className={styles.sheetMessage}>{t.revertUnsaved}</p>}
          {to === 0 && <p className={styles.sheetNote}>{t.revertConflictHint}</p>}
        </>
      )}
      <footer className={styles.sheetActions}>
        <Button onClick={dismissSheet}>{ja.common.cancel}</Button>
        <Button variant="primary" disabled={!possible} onClick={() => confirmRevert(to)}>
          {to === 0 ? t.revertConfirm : t.versionRestore}
        </Button>
      </footer>
    </Dialog>
  );
}

/** 版の一覧の 1 行の文（日時・種類・形式・大きさ）。 */
export function versionLine(v: EditVersionDto, now: number = Date.now()): string {
  const kind =
    v.kind === 'revert' && v.revertTo !== null
      ? t.versionRevertTo(formatCount(v.revertTo))
      : (t.versionKinds[v.kind] ?? v.kind);
  const size =
    v.width !== null && v.height !== null
      ? t.size(formatCount(v.width), formatCount(v.height))
      : null;
  return [
    relativeDateTime(v.createdAt, now),
    kind,
    formatName(v.format),
    size,
    formatBytes(v.sizeBytes),
  ]
    .filter(Boolean)
    .join(' · ');
}

/** 版の一覧（文字だけ）：日時・種類・形式・大きさ、照合が合わずに残した版の印。途中の版に戻せる。 */
function VersionsSheet() {
  const versions = useVersions();
  const titleId = useId();
  const head = versions && versions.length > 0 ? Math.max(...versions.map((v) => v.version)) : -1;
  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="sm">
      <h2 id={titleId} className={styles.sheetTitle}>
        {t.versionsTitle}
      </h2>
      {versions === null ? (
        <p className={styles.sheetMessage}>{t.versionsLoading}</p>
      ) : versions.length === 0 ? (
        <p className={styles.sheetMessage}>{t.versionsEmpty}</p>
      ) : (
        <>
          <ol className={styles.versionList}>
            {[...versions].reverse().map((v) => (
              <li key={v.version} className={styles.versionRow}>
                <span className={styles.versionText}>
                  <span className={styles.versionName}>
                    {t.versionName(formatCount(v.version))}
                    {v.version === head && <span className={styles.badge}>{t.versionCurrent}</span>}
                    {v.fileState === 'kept' && (
                      <span className={styles.badge} title={t.versionKept}>
                        {t.versionKept}
                      </span>
                    )}
                  </span>
                  <span className={styles.versionMeta}>{versionLine(v)}</span>
                </span>
                {v.version !== head && (
                  <Button size="sm" onClick={() => askRestore(v.version)}>
                    {t.versionRestore}
                  </Button>
                )}
              </li>
            ))}
          </ol>
          <p className={styles.sheetNote}>{t.versionsHint}</p>
        </>
      )}
      <footer className={styles.sheetActions}>
        <Button onClick={dismissSheet}>{ja.common.close}</Button>
      </footer>
    </Dialog>
  );
}

/** 保存できなかったとき：code ごとの分かる言葉と、Rust の言葉。 */
function FailedSheet({ sheet }: { sheet: Extract<EditorSheet, { kind: 'saveFailed' }> }) {
  const titleId = useId();
  const text = saveFailureText(sheet.mode, sheet.code, sheet.message);
  return (
    <Dialog labelledBy={titleId} onClose={dismissSheet} size="sm">
      <h2 id={titleId} className={styles.sheetTitle}>
        {t.saveFailed}
      </h2>
      <div role="alert">
        {text.lead && <p className={styles.sheetMessage}>{text.lead}</p>}
        {text.detail && <p className={styles.sheetNote}>{text.detail}</p>}
        {text.hint && <p className={styles.sheetMessage}>{text.hint}</p>}
      </div>
      <footer className={styles.sheetActions}>
        <Button variant="primary" onClick={dismissSheet}>
          {t.ok}
        </Button>
      </footer>
    </Dialog>
  );
}

function SheetOf({ sheet }: { sheet: EditorSheet }) {
  switch (sheet.kind) {
    case 'draft':
      return <DraftSheet />;
    case 'unsaved':
      return <UnsavedSheet />;
    case 'saveWarnings':
      return <WarningsSheet />;
    case 'saveAs':
      return <SaveAsSheet />;
    case 'revert':
      return <RevertSheet to={0} />;
    case 'restore':
      return <RevertSheet to={sheet.version} />;
    case 'versions':
      return <VersionsSheet />;
    case 'saveFailed':
      return <FailedSheet sheet={sheet} />;
  }
}

/**
 * エディタの上に出すシート（下書き・未保存で閉じる・警告・別名・元に戻す・版の一覧・失敗）。エディタの層（z-index 16）の
 * 中に描くと、ルートのシート（設定など）の下に隠れるので、body に出す（設定のシートからライブラリを切り替えるとき・
 * 設定を開いたまま窓を閉じるときも、未保存のシートがいちばん上に出る）。
 */
export function EditorSheets() {
  const sheet = useEditorStore((s) => s.sheet);
  if (!sheet) return null;
  return createPortal(<SheetOf sheet={sheet} />, document.body);
}
