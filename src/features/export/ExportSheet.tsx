import { useEffect, useId, useRef, useState } from 'react';

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
  const [total, setTotal] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  // 選び直しの番号（古い照会の答えで新しい書き出し先を上書きしない）
  const picking = useRef(0);

  useEffect(() => {
    exportFormats()
      .then(setFormats)
      .catch((e: unknown) => console.warn('書き出しの形式を読めませんでした', e));
  }, []);

  /**
   * 書き出し先を選ぶ。照会し終えてから書き出し先・プロジェクト・名前と形式を一度にそろえる。既定を入れ替えるのは、
   * プロジェクトが変わったとき（入る・出る・別のプロジェクトへ）だけ（同じプロジェクトの中・外同士なら、シートで変えた値を
   * 残す。段 6 の検証の直し・利用者の決定）。照会に失敗したらプロジェクトの外として扱う。
   */
  const choose = async () => {
    const picked = await pickExportFolder(ja.exporting.pickTitle);
    if (!picked) return;
    const turn = ++picking.current;
    let found: ProjectDto | null;
    let nextPreset: ExportPresetDto | null = null;
    try {
      found = await projectForDest(picked);
      if (found && found.id !== project?.id) nextPreset = await projectSettings(found.id);
    } catch (e) {
      notify(messageOf(e));
      found = null;
      nextPreset = null;
    }
    if (turn !== picking.current) return;
    if (!found && project) nextPreset = lastPreset();
    setDest(picked);
    setProject(found);
    if (nextPreset) {
      setPreset(nextPreset);
      setPresetKey((k) => k + 1);
    }
  };

  const start = async () => {
    if (!dest) return;
    if (busy) return;
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
        <p className={styles.sub}>{t.count(formatCount(total ?? entries.length))}</p>
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
          entries={entries}
          onTotal={setTotal}
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
