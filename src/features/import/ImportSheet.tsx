import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { DuplicateIcon } from '../../components/icons/DuplicateIcon';
import { ImportIcon } from '../../components/icons/ImportIcon';
import { InfoIcon } from '../../components/icons/InfoIcon';
import { Select } from '../../components/Select/Select';
import { Switch } from '../../components/Switch/Switch';
import { TagInput } from '../../components/TagInput/TagInput';
import { ja } from '../../i18n/ja';
import type { DuplicatePolicyDto } from '../../ipc/bindings';
import { foldersTree } from '../../ipc/folders';
import type { ImportPreviewDto } from '../../ipc/import';
import { formatBytes, formatCount } from '../../lib/format';
import { useImportStore } from '../../stores/importStore';
import {
  confirmLabel,
  describeSources,
  folderChoices,
  folderTagExample,
  reproduceExample,
  unitExample,
} from './describe';
import styles from './ImportSheet.module.css';

const t = ja.import;

/** 取り込みシート（Import.dc.html）。ドロップ・選択のあと、確認中から表示する。 */
export function ImportSheet() {
  const phase = useImportStore((s) => s.phase);
  const preview = useImportStore((s) => s.preview);
  const error = useImportStore((s) => s.error);
  const close = useImportStore((s) => s.close);
  const titleId = useId();

  if (phase === 'idle') {
    return null;
  }
  return (
    <Dialog labelledBy={titleId} onClose={close}>
      {phase === 'checking' || (!preview && !error) ? (
        <Message titleId={titleId} title={t.checking} onClose={close} />
      ) : !preview ? (
        <Message titleId={titleId} title={t.failed(error ?? '')} onClose={close} />
      ) : preview.total === 0 ? (
        <Message titleId={titleId} title={t.nothing} detail={t.nothingDetail} onClose={close} />
      ) : (
        <SheetBody titleId={titleId} preview={preview} />
      )}
    </Dialog>
  );
}

function Message({
  titleId,
  title,
  detail,
  onClose,
}: {
  titleId: string;
  title: string;
  detail?: string;
  onClose: () => void;
}) {
  return (
    <>
      <header className={styles.header}>
        <span className={styles.badge}>
          <ImportIcon size={22} />
        </span>
        <div className={styles.headingText}>
          <h2 id={titleId} className={styles.title}>
            {title}
          </h2>
          {detail && <span className={styles.subtitle}>{detail}</span>}
        </div>
      </header>
      <footer className={styles.actions}>
        <Button onClick={onClose}>{ja.common.close}</Button>
      </footer>
    </>
  );
}

function SheetBody({ titleId, preview }: { titleId: string; preview: ImportPreviewDto }) {
  const close = useImportStore((s) => s.close);
  const confirm = useImportStore((s) => s.confirm);
  const error = useImportStore((s) => s.error);
  const [folders, setFolders] = useState<{ id: number; path: string[] }[]>([]);
  const [destId, setDestId] = useState<number | null>(null);
  const [duplicates, setDuplicates] = useState<DuplicatePolicyDto>('addMemberships');
  const [reproduce, setReproduce] = useState(true);
  const [folderTags, setFolderTags] = useState(false);
  const [units, setUnits] = useState(false);
  const [fileInfoTags, setFileInfoTags] = useState(true);
  const [tags, setTags] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const organizeId = useId();
  const destLabelId = useId();
  const tagsId = useId();

  useEffect(() => {
    foldersTree()
      .then((nodes) => setFolders(folderChoices(nodes)))
      .catch((err: unknown) => console.warn('仮想フォルダを読めませんでした', err));
  }, []);

  const hasFolders = preview.roots.some((r) => r.isDir);
  const destPath = folders.find((f) => f.id === destId)?.path ?? null;

  const onConfirm = async () => {
    setBusy(true);
    await confirm({
      destFolderId: destId,
      reproduceFolders: hasFolders && reproduce,
      folderNamesAsTags: hasFolders && folderTags,
      unitsPerFolder: hasFolders && units,
      fileInfoTags,
      tags,
      duplicates,
    });
    setBusy(false);
  };

  return (
    <>
      <header className={styles.header}>
        <span className={styles.badge}>
          <ImportIcon size={22} />
        </span>
        <div className={styles.headingText}>
          <h2 id={titleId} className={styles.title}>
            {t.title(formatCount(preview.total))}
          </h2>
          <span className={styles.subtitle}>{describeSources(preview)}</span>
        </div>
      </header>

      <dl className={styles.stats}>
        <Stat label={t.stats.images} value={formatCount(preview.images)} />
        <Stat label={t.stats.videos} value={formatCount(preview.videos)} />
        <Stat label={t.stats.audios} value={formatCount(preview.audios)} />
        <Stat label={t.stats.total} value={formatBytes(preview.totalBytes)} />
      </dl>

      {preview.duplicates > 0 && (
        <div className={styles.duplicates}>
          <span className={styles.duplicateIcon}>
            <DuplicateIcon size={16} strokeWidth={2} />
          </span>
          <span className={styles.duplicateText}>
            <span>{t.duplicates(formatCount(preview.duplicates))}</span>
            <span className={styles.hint}>
              {duplicates === 'ignore'
                ? t.duplicatePolicy.ignoreHint
                : t.duplicatePolicy.addMembershipsHint}
            </span>
          </span>
          <Select
            aria-label={t.duplicatePolicy.label}
            value={duplicates}
            onChange={(value) => setDuplicates(value === 'ignore' ? 'ignore' : 'addMemberships')}
            options={[
              { value: 'addMemberships', label: t.duplicatePolicy.addMemberships },
              { value: 'ignore', label: t.duplicatePolicy.ignore },
            ]}
          />
        </div>
      )}

      <section className={styles.organize} aria-labelledby={organizeId}>
        <h3 id={organizeId} className={styles.sectionTitle}>
          {t.organize}
        </h3>
        <div className={styles.destinationRow}>
          <span id={destLabelId}>{t.destination}</span>
          <Select
            aria-labelledby={destLabelId}
            className={styles.destination}
            value={destId === null ? '' : String(destId)}
            onChange={(value) => setDestId(value === '' ? null : Number(value))}
            options={[
              { value: '', label: t.noDestination },
              ...folders.map((f) => ({ value: String(f.id), label: t.folderPath(f.path) })),
            ]}
          />
        </div>
        {hasFolders && (
          <>
            <SwitchRow
              label={t.reproduce}
              hint={reproduceExample(preview.sampleDirs, destPath)}
              checked={reproduce}
              onChange={setReproduce}
            />
            <SwitchRow
              label={t.folderTags}
              hint={folderTagExample(preview.sampleDirs)}
              checked={folderTags}
              onChange={setFolderTags}
            />
            <SwitchRow
              label={t.unitsPerFolder}
              hint={unitExample(preview.sampleDirs)}
              checked={units}
              onChange={setUnits}
            />
          </>
        )}
        <SwitchRow
          label={t.fileInfoTags}
          hint={t.fileInfoTagsHint}
          checked={fileInfoTags}
          onChange={setFileInfoTags}
        />
      </section>

      <section className={styles.tags} aria-labelledby={tagsId}>
        <h3 id={tagsId} className={styles.sectionTitle}>
          {t.tagsForAll}
        </h3>
        <TagInput
          tags={tags}
          onChange={setTags}
          placeholder={t.addTag}
          inputLabel={t.addTagLabel}
          removeLabel={t.removeTag}
        />
      </section>

      <p className={styles.note}>
        <span className={styles.noteIcon}>
          <InfoIcon size={14} strokeWidth={2} />
        </span>
        {t.note}
      </p>

      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}

      <footer className={styles.actions}>
        <Button onClick={close}>{ja.common.cancel}</Button>
        <Button variant="primary" onClick={() => void onConfirm()} disabled={busy}>
          {confirmLabel(preview)}
        </Button>
      </footer>
    </>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className={styles.stat}>
      <dt className={styles.statLabel}>{label}</dt>
      <dd className={styles.statValue}>{value}</dd>
    </div>
  );
}

function SwitchRow({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className={styles.switchRow}>
      <span className={styles.switchText}>
        <span>{label}</span>
        {hint && <span className={styles.hint}>{hint}</span>}
      </span>
      <Switch checked={checked} onChange={onChange} />
    </label>
  );
}
