import { type MouseEvent, useEffect, useState } from 'react';

import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { CheckIcon } from '../../components/icons/CheckIcon';
import { ChevronDownIcon } from '../../components/icons/ChevronDownIcon';
import { MoreIcon } from '../../components/icons/MoreIcon';
import { WarningIcon } from '../../components/icons/WarningIcon';
import { ja } from '../../i18n/ja';
import { pickToolFile } from '../../ipc/dialogs';
import {
  type CookieSourceDto,
  type DownloadPrefsDto,
  getDownloadPrefs,
  setDownloadPrefs,
  setToolPath,
  type ToolInfoDto,
  toolsStatus,
  updateTools,
} from '../../ipc/downloads';
import { formatCount } from '../../lib/format';
import { fail } from './downloadActions';
import styles from './Downloads.module.css';
import { RulesSheet } from './RulesSheet';
import { TextSheet } from './TextSheet';

const t = ja.downloads;

/** 取得に要るツール（ないと入れ方を案内する）。ffprobe は ffmpeg と一緒に入る。 */
const REQUIRED = ['yt-dlp', 'gallery-dl', 'deno', 'ffmpeg'];

const COOKIES: CookieSourceDto[] = ['none', 'firefox', 'safari', 'chrome'];

/** 前に読んだ状態（画面を開き直したときに、読み直すあいだも出しておく）。 */
let lastTools: ToolInfoDto[] | null = null;

/** 右の欄：外部ツールの状態・更新・場所の指定、Cookie の読み込み元、取得の規則。 */
export function ToolsPanel() {
  const [tools, setTools] = useState<ToolInfoDto[] | null>(lastTools);
  const [prefs, setPrefs] = useState<DownloadPrefsDto | null>(null);
  const [checking, setChecking] = useState(false);
  const [sheet, setSheet] = useState<{ title: string; intro: string; text: string } | null>(null);
  const [editingRules, setEditingRules] = useState(false);

  const reload = () =>
    toolsStatus().then((list) => {
      lastTools = list;
      setTools(list);
    }, fail);
  useEffect(() => {
    void reload();
    getDownloadPrefs().then(setPrefs, fail);
  }, []);

  const missing = (tools ?? [])
    .filter((tool) => REQUIRED.includes(tool.name) && tool.path === null)
    .map((tool) => tool.name);

  const check = () => {
    setChecking(true);
    updateTools()
      .then((result) => {
        const lines = result.outputs.map((o) => `$ ${o.tool}\n${o.text}`);
        setSheet({
          title: t.tools.updateTitle,
          intro: result.brewCommand ? t.tools.brewHint : t.tools.noUpdates,
          text: [result.brewCommand, ...lines].filter(Boolean).join('\n\n'),
        });
        return reload();
      }, fail)
      .finally(() => setChecking(false));
  };

  const toolMenu = (tool: ToolInfoDto) => (e: MouseEvent<HTMLButtonElement>) => {
    const entries: MenuEntry[] = [
      {
        label: t.tools.choosePath,
        onSelect: () => {
          pickToolFile(t.tools.chooseTitle(tool.name))
            .then((path) => (path ? setToolPath(tool.name, path).then(reload) : undefined))
            .catch(fail);
        },
      },
      {
        label: t.tools.resetPath,
        disabled: !tool.overridden,
        onSelect: () => {
          setToolPath(tool.name, null).then(reload, fail);
        },
      },
    ];
    openMenuBelow(e.currentTarget, t.tools.menu(tool.name), entries);
  };

  const savePrefs = (next: DownloadPrefsDto) => setDownloadPrefs(next).then(setPrefs);

  const cookieMenu = (e: MouseEvent<HTMLButtonElement>) => {
    if (!prefs) {
      return;
    }
    openMenuBelow(
      e.currentTarget,
      t.cookies.label,
      COOKIES.map((c) => ({
        label: t.cookies[c],
        icon: c === prefs.cookies ? <CheckIcon size={12} strokeWidth={2.4} /> : null,
        onSelect: () => {
          savePrefs({ ...prefs, cookies: c }).catch(fail);
        },
      })),
    );
  };

  return (
    <section className={styles.sidePanel} aria-label={t.tools.title}>
      <div className={styles.sideHead}>
        <h2 className={styles.sideTitle}>{t.tools.title}</h2>
        <button type="button" className={styles.smallButton} disabled={checking} onClick={check}>
          {checking ? t.tools.checking : t.tools.check}
        </button>
      </div>
      <div className={styles.tools}>
        {(tools ?? []).map((tool) => {
          const found = tool.path !== null;
          const note = t.tools.notes[tool.name];
          return (
            <div key={tool.name} className={styles.toolRow}>
              <span className={styles.toolName} title={tool.path ?? undefined}>
                <code className={styles.mono}>{tool.name}</code>
                {note && <span className={styles.toolNote}>{note}</span>}
              </span>
              <span
                className={styles.toolState}
                data-missing={!found}
                title={tool.overridden ? t.tools.overridden : undefined}
              >
                {found ? (
                  <CheckIcon size={11} strokeWidth={3.2} />
                ) : (
                  <WarningIcon size={12} strokeWidth={2.2} />
                )}
                {found ? (tool.version ?? t.tools.available) : t.tools.missing}
              </span>
              <button
                type="button"
                className={styles.toolMenu}
                aria-label={t.tools.menu(tool.name)}
                aria-haspopup="menu"
                onClick={toolMenu(tool)}
              >
                <MoreIcon size={14} />
              </button>
            </div>
          );
        })}
      </div>
      {missing.length > 0 && (
        <div className={styles.sideRow}>
          <span className={styles.warning}>
            <WarningIcon size={13} strokeWidth={2} />
            {missing.join('・')}
          </span>
          <button
            type="button"
            className={styles.smallButton}
            onClick={() =>
              setSheet({
                title: t.tools.installTitle,
                intro: t.tools.installHint,
                text: `brew install ${missing.join(' ')}`,
              })
            }
          >
            {t.tools.install}
          </button>
        </div>
      )}
      <div className={styles.sideRow}>
        {t.cookies.label}
        <button
          type="button"
          className={styles.menuButton}
          aria-haspopup="menu"
          aria-label={`${t.cookies.label}：${prefs ? t.cookies[prefs.cookies] : ''}`}
          disabled={!prefs}
          onClick={cookieMenu}
        >
          {prefs ? t.cookies[prefs.cookies] : ''}
          <ChevronDownIcon size={9} strokeWidth={2.8} />
        </button>
      </div>
      <div className={styles.sideRow}>
        <span>
          {t.rules.title}
          <span className={styles.toolNote}>
            {' '}
            {prefs && prefs.rules.length > 0
              ? t.rules.count(formatCount(prefs.rules.length))
              : t.rules.none}
          </span>
        </span>
        <button
          type="button"
          className={styles.smallButton}
          disabled={!prefs}
          onClick={() => setEditingRules(true)}
        >
          {t.rules.edit}
        </button>
      </div>
      {sheet && (
        <TextSheet
          title={sheet.title}
          intro={sheet.intro}
          text={sheet.text}
          onClose={() => setSheet(null)}
        />
      )}
      {editingRules && prefs && (
        <RulesSheet
          rules={prefs.rules}
          onSave={(rules) => savePrefs({ ...prefs, rules })}
          onClose={() => setEditingRules(false)}
        />
      )}
    </section>
  );
}
