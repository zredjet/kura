import { homeDir } from '@tauri-apps/api/path';
import { useCallback, useEffect, useId, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { askConfirm } from '../../components/Overlay/overlayStore';
import { Segmented } from '../../components/Segmented/Segmented';
import { Select } from '../../components/Select/Select';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import {
  type ArchiveStatusDto,
  clearDownloadArchive,
  downloadArchives,
  toolCommand,
} from '../../ipc/downloads';
import {
  type BackupDto,
  backupNow,
  checkLibrary,
  type IntegrityDto,
  libraryInfo,
  libraryUsage,
  type LibraryUsageDto,
  listBackups,
  revealPath,
} from '../../ipc/library';
import { relativeDateTime } from '../../lib/dates';
import { formatBytes, formatCount, tildePath } from '../../lib/format';
import { useLibraryStore } from '../../stores/libraryStore';
import { notify } from '../../stores/noticeStore';
import { type SettingsTab, useSettingsStore } from '../../stores/settingsStore';
import { useViewStore } from '../../stores/viewStore';
import { ToolsPanel } from '../downloads/ToolsPanel';
import { updatePlayerPrefs } from '../player/playerActions';
import { usePlayerStore } from '../player/playerStore';
import { createNew, openOther } from '../sidebar/LibrarySwitcher';
import { integritySummary } from './integrity';
import styles from './Settings.module.css';

const t = ja.settings;

/** 画像の表示時間の選択肢（秒）。 */
const IMAGE_SECONDS = [2, 3, 5, 8, 10, 15, 20, 30, 60];

function fail(error: unknown): void {
  notify(error instanceof Error ? error.message : String(error));
}

/** 設定画面（design.md §3.1。⌘,・サイドバーの歯車）。一般・ライブラリ・ダウンロードのタブ。 */
export function SettingsSheet() {
  const open = useSettingsStore((s) => s.open);
  const tab = useSettingsStore((s) => s.tab);
  const setTab = useSettingsStore((s) => s.setTab);
  const close = useSettingsStore((s) => s.close);
  const id = useId();
  if (!open) {
    return null;
  }
  const tabs: { value: SettingsTab; label: string }[] = [
    { value: 'general', label: t.tabs.general },
    { value: 'library', label: t.tabs.library },
    { value: 'downloads', label: t.tabs.downloads },
  ];
  return (
    <Dialog labelledBy={id} onClose={close} size="lg">
      <div className={styles.head}>
        <h2 id={id} className={styles.title}>
          {t.title}
        </h2>
        <Segmented label={t.tabsLabel} value={tab} options={tabs} onChange={setTab} />
      </div>
      <div className={styles.body}>
        {tab === 'general' && <GeneralTab />}
        {tab === 'library' && <LibraryTab />}
        {tab === 'downloads' && <DownloadsTab />}
      </div>
      <div className={styles.actions}>
        <Button variant="primary" onClick={close}>
          {t.done}
        </Button>
      </div>
    </Dialog>
  );
}

/** 一般：画像の表示時間・ユニットをまとめて表示。 */
function GeneralTab() {
  const imageMs = usePlayerStore((s) => s.prefs.imageDurationMs);
  const collapseUnits = useViewStore((s) => s.collapseUnits);
  const toggleCollapseUnits = useViewStore((s) => s.toggleCollapseUnits);
  const seconds = Math.round(imageMs / 1000);
  const options = (IMAGE_SECONDS.includes(seconds) ? IMAGE_SECONDS : [...IMAGE_SECONDS, seconds])
    .sort((a, b) => a - b)
    .map((s) => ({ value: String(s), label: t.seconds(s) }));
  return (
    <>
      <div className={styles.row}>
        <span className={styles.label}>
          {t.imageDuration}
          <span className={styles.hint}>{t.imageDurationHint}</span>
        </span>
        <Select
          aria-label={t.imageDuration}
          value={String(seconds)}
          options={options}
          onChange={(value) => updatePlayerPrefs({ imageDurationMs: Number(value) * 1000 })}
        />
      </div>
      <div className={styles.row}>
        <span className={styles.label}>
          {ja.library.collapseUnits}
          <span className={styles.hint}>{t.collapseUnitsHint}</span>
        </span>
        <Switch
          checked={collapseUnits}
          aria-label={ja.library.collapseUnits}
          onChange={toggleCollapseUnits}
        />
      </div>
    </>
  );
}

/** ライブラリ：場所・容量・整合性チェック・バックアップ。 */
function LibraryTab() {
  const info = useLibraryStore((s) => s.info);
  const [home, setHome] = useState<string | null>(null);
  const [usage, setUsage] = useState<LibraryUsageDto | null>(null);
  const [integrity, setIntegrity] = useState<IntegrityDto | null>(info?.integrity ?? null);
  const [checking, setChecking] = useState(false);
  const [backups, setBackups] = useState<BackupDto[] | null>(null);
  const [backingUp, setBackingUp] = useState(false);

  const loadBackups = useCallback(() => {
    listBackups().then(setBackups, fail);
  }, []);
  useEffect(() => {
    homeDir().then(setHome, () => undefined);
    libraryUsage().then(setUsage, fail);
    // 開いたときのチェックの結果（画面を開いた時点では終わっていなかったことがある）
    libraryInfo().then((latest) => {
      if (latest.integrity) {
        setIntegrity((current) => current ?? latest.integrity);
      }
    }, fail);
    loadBackups();
  }, [loadBackups]);

  if (!info) {
    return null;
  }
  const check = () => {
    setChecking(true);
    checkLibrary()
      .then(setIntegrity, fail)
      .finally(() => setChecking(false));
  };
  const makeBackup = () => {
    setBackingUp(true);
    backupNow()
      .then((made) => {
        notify(t.backupMade(relativeDateTime(made.createdAt)));
        loadBackups();
      }, fail)
      .finally(() => setBackingUp(false));
  };
  const reveal = (path: string) => {
    revealPath(path).catch(fail);
  };

  return (
    <>
      <section className={styles.group} aria-label={t.location}>
        <h3 className={styles.groupTitle}>{t.location}</h3>
        <p className={styles.path}>{tildePath(info.path, home)}</p>
        <div className={styles.buttons}>
          <Button size="sm" onClick={() => reveal(info.path)}>
            {t.reveal}
          </Button>
          <Button size="sm" onClick={() => void openOther()}>
            {t.openOther}
          </Button>
          <Button size="sm" onClick={() => void createNew()}>
            {t.createNew}
          </Button>
        </div>
      </section>
      <section className={styles.group} aria-label={t.usage}>
        <h3 className={styles.groupTitle}>{t.usage}</h3>
        <dl className={styles.facts}>
          <dt>{t.libraryBytes}</dt>
          <dd>{usage ? formatBytes(usage.libraryBytes) : '—'}</dd>
          <dt>{t.diskFree}</dt>
          <dd>
            {usage?.volumeAvailableBytes != null && usage.volumeTotalBytes != null
              ? t.diskFreeValue(
                  formatBytes(usage.volumeAvailableBytes),
                  formatBytes(usage.volumeTotalBytes),
                )
              : '—'}
          </dd>
        </dl>
      </section>
      <section className={styles.group} aria-label={t.integrity}>
        <h3 className={styles.groupTitle}>{t.integrity}</h3>
        <p className={styles.text} role="status">
          {integrity === null ? t.integrityPending : integritySummary(integrity).text}
        </p>
        <div className={styles.buttons}>
          <Button size="sm" disabled={checking} onClick={check}>
            {checking ? t.checking : t.checkAgain}
          </Button>
        </div>
      </section>
      <section className={styles.group} aria-label={t.backups}>
        <h3 className={styles.groupTitle}>{t.backups}</h3>
        <p className={styles.hint}>{t.backupsHint}</p>
        {backups !== null && backups.length === 0 && <p className={styles.text}>{t.noBackups}</p>}
        {backups && backups.length > 0 && (
          <ul className={styles.list}>
            {backups.map((b) => (
              <li key={b.name} className={styles.listRow}>
                <span>{relativeDateTime(b.createdAt)}</span>
                <span className={styles.muted}>{formatBytes(b.sizeBytes)}</span>
              </li>
            ))}
          </ul>
        )}
        <div className={styles.buttons}>
          <Button size="sm" disabled={backingUp} onClick={makeBackup}>
            {backingUp ? t.backingUp : t.backupNow}
          </Button>
          <Button size="sm" onClick={() => reveal(backups?.[0]?.path ?? `${info.path}/backups`)}>
            {t.reveal}
          </Button>
        </div>
        <p className={styles.hint}>{t.restoreHint}</p>
      </section>
    </>
  );
}

/** ダウンロード：外部ツール・Cookie・取得の規則（ダウンロード画面と同じ欄）と、取得済みの記録。 */
function DownloadsTab() {
  const [archives, setArchives] = useState<ArchiveStatusDto[] | null>(null);
  const load = useCallback(() => {
    downloadArchives().then(setArchives, fail);
  }, []);
  useEffect(load, [load]);

  const clear = async (archive: ArchiveStatusDto) => {
    const name = toolCommand(archive.tool);
    const ok = await askConfirm({
      title: t.archiveClearTitle,
      message: t.archiveClearMessage(name),
      confirm: t.archiveClear,
    });
    if (ok) {
      clearDownloadArchive(archive.tool).then(() => {
        notify(t.archiveCleared(name));
        load();
      }, fail);
    }
  };

  return (
    <>
      <ToolsPanel />
      <section className={styles.group} aria-label={t.archives}>
        <h3 className={styles.groupTitle}>{t.archives}</h3>
        <p className={styles.hint}>{t.archivesHint}</p>
        <ul className={styles.list}>
          {(archives ?? []).map((a) => (
            <li key={a.tool} className={styles.listRow}>
              <span>{toolCommand(a.tool)}</span>
              <span className={styles.muted}>
                {a.entries === null
                  ? t.archiveUnreadable
                  : t.archiveEntries(formatCount(a.entries))}
              </span>
              <Button
                size="sm"
                disabled={a.entries === 0 && a.sizeBytes === 0}
                onClick={() => void clear(a)}
              >
                {t.archiveClear}
              </Button>
            </li>
          ))}
        </ul>
      </section>
    </>
  );
}
