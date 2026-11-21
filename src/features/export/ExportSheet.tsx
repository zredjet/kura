import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ja } from '../../i18n/ja';
import { pickExportFolder } from '../../ipc/dialogs';
import {
  exportFormats,
  type ExportFormatsDto,
  type ExportPresetDto,
  startExport,
} from '../../ipc/export';
import { type ProjectDto, projectForDest, projectSettings } from '../../ipc/projects';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';
import { expandEntries } from '../library/actions';
import styles from './Export.module.css';
import { ExportOptionsFields } from './ExportOptionsFields';
import {
  closeExportSheet,
  lastPreset,
  rememberPreset,
  useExportSheetStore,
} from './exportSheetStore';

const t = ja.exportSheet;

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * 書き出しのシート（⌘⇧E・項目のメニュー。M15 段 6。design.md §3.1）：書き出し先（選ぶ…）・名前の規則・形式・JSON・名前の
 * 見本。書き出し先が登録したプロジェクトの中ならその持ち出しの既定を入れ（変えられる）、終わったら照らし直す。プロジェクトの
 * 外への書き出しは前回の名前と形式を覚える。
 */
export function ExportSheet() {
  const entries = useExportSheetStore((s) => s.entries);
  if (!entries) return null;
  return <Sheet key={entries.join(',')} entries={entries} />;
}

function Sheet({ entries }: { entries: number[] }) {
  const titleId = useId();
  const [dest, setDest] = useState<string | null>(null);
  const [project, setProject] = useState<ProjectDto | null>(null);
  const [preset, setPreset] = useState<ExportPresetDto>(lastPreset);
  // 既定を入れ替えたら欄を作り直す（テンプレートの入力を合わせる）
  const [presetKey, setPresetKey] = useState(0);
  const [formats, setFormats] = useState<ExportFormatsDto | null>(null);
  const [items, setItems] = useState<number[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    exportFormats()
      .then(setFormats)
      .catch((e: unknown) => console.warn('書き出しの形式を読めませんでした', e));
    expandEntries(entries)
      .then(setItems)
      .catch(() => setItems([]));
  }, [entries]);

  const choose = async () => {
    const picked = await pickExportFolder(ja.exporting.pickTitle);
    if (!picked) return;
    setDest(picked);
    try {
      const found = await projectForDest(picked);
      if (found) {
        setPreset(await projectSettings(found.id));
        setPresetKey((k) => k + 1);
      } else if (project) {
        // プロジェクトの外へ選び直したら、前回の名前と形式に戻す
        setPreset(lastPreset());
        setPresetKey((k) => k + 1);
      }
      setProject(found);
    } catch (e) {
      notify(messageOf(e));
    }
  };

  const start = async () => {
    if (!dest) return;
    setBusy(true);
    try {
      await startExport(dest, entries, { ...preset, project: project?.id ?? null });
      if (!project) rememberPreset(preset);
      closeExportSheet();
    } catch (e) {
      notify(messageOf(e));
      setBusy(false);
    }
  };

  return (
    <Dialog labelledBy={titleId} onClose={closeExportSheet}>
      <div className={styles.head}>
        <h2 id={titleId} className={styles.title}>
          {t.title}
        </h2>
        <p className={styles.sub}>{t.count(formatCount(items.length || entries.length))}</p>
      </div>
      <div className={styles.body}>
        <div className={styles.group}>
          <div className={styles.dest}>
            <span className={styles.rowLabel}>{t.dest}</span>
            <p className={styles.path}>{dest ?? t.noDest}</p>
            <Button size="sm" onClick={() => void choose()}>
              {t.choose}
            </Button>
          </div>
          {project && <p className={styles.hint}>{t.projectHint(project.name)}</p>}
        </div>
        <ExportOptionsFields
          key={presetKey}
          value={preset}
          onChange={setPreset}
          formats={formats}
          items={items}
        />
      </div>
      <div className={styles.actions}>
        <Button onClick={closeExportSheet}>{ja.common.cancel}</Button>
        <Button variant="primary" disabled={!dest || busy} onClick={() => void start()}>
          {t.start}
        </Button>
      </div>
    </Dialog>
  );
}
