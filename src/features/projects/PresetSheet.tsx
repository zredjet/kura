import { useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ja } from '../../i18n/ja';
import { exportFormats, type ExportFormatsDto, type ExportPresetDto } from '../../ipc/export';
import { projectItems, projectSettings, setProjectSettings } from '../../ipc/projects';
import { notify } from '../../stores/noticeStore';
import styles from '../export/Export.module.css';
import { ExportOptionsFields } from '../export/ExportOptionsFields';
import { closePresetSheet, usePresetSheetStore } from './presetSheetStore';
import { useProjectStore } from './projectStore';

const t = ja.presetSheet;

/**
 * プロジェクトの持ち出しの設定（M15 段 6。design.md §3.1）：ドラッグアウトとプロジェクトの中への書き出しで使う名前の規則・
 * 形式・JSON。名前の見本は、そのプロジェクトで今使っている素材で見せる（まだなければ見本なし）。
 */
export function PresetSheet() {
  const project = usePresetSheetStore((s) => s.project);
  if (project === null) return null;
  return <Sheet key={project} id={project} />;
}

function Sheet({ id }: { id: number }) {
  const titleId = useId();
  const name = useProjectStore((s) => s.list.find((p) => p.id === id)?.name ?? '');
  const [preset, setPreset] = useState<ExportPresetDto | null>(null);
  const [formats, setFormats] = useState<ExportFormatsDto | null>(null);
  const [items, setItems] = useState<number[]>([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    projectSettings(id)
      .then(setPreset)
      .catch((e: unknown) => {
        notify(e instanceof Error ? e.message : String(e));
        closePresetSheet();
      });
    exportFormats()
      .then(setFormats)
      .catch((e: unknown) => console.warn('書き出しの形式を読めませんでした', e));
    projectItems(id)
      .then((ids) => setItems(ids.slice(0, 50)))
      .catch(() => setItems([]));
  }, [id]);

  const save = async () => {
    if (!preset) return;
    setBusy(true);
    try {
      await setProjectSettings(id, preset);
      notify(t.saved);
      closePresetSheet();
    } catch (e) {
      notify(e instanceof Error ? e.message : String(e));
      setBusy(false);
    }
  };

  return (
    <Dialog labelledBy={titleId} onClose={closePresetSheet}>
      <div className={styles.head}>
        <h2 id={titleId} className={styles.title}>
          {t.title(name)}
        </h2>
        <p className={styles.sub}>{t.hint}</p>
      </div>
      <div className={styles.body}>
        {preset && (
          <ExportOptionsFields
            value={preset}
            onChange={setPreset}
            formats={formats}
            items={items}
          />
        )}
      </div>
      <div className={styles.actions}>
        <Button onClick={closePresetSheet}>{ja.common.cancel}</Button>
        <Button variant="primary" disabled={!preset || busy} onClick={() => void save()}>
          {t.save}
        </Button>
      </div>
    </Dialog>
  );
}
