import { useCallback, useEffect, useState } from 'react';

import { askText } from '../../components/Overlay/overlayStore';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { ja } from '../../i18n/ja';
import {
  browserStatus,
  type CaptureRuleDto,
  closeBrowser,
  listCaptureRules,
  openBrowser,
  openCaptureLogin,
  snapshotBrowser,
} from '../../ipc/capture';
import { formatCount } from '../../lib/format';
import { reloadList } from '../../stores/jobsStore';
import { notify } from '../../stores/noticeStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { relativeDateTime } from '../../lib/dates';
import { CaptureRuleSheet } from './CaptureRuleSheet';
import { destLabel, fail } from './downloadActions';
import styles from './Downloads.module.css';

const t = ja.downloads.capture;

/** アプリ内ブラウザが開いているかを確かめる間隔。 */
const BROWSER_POLL_MS = 1500;

/** ルールの一覧を読み直す（ほかの部品からも呼ぶ）。 */
let reloadRules: () => void = () => {};

/** ルールを読み直す（ルールが動いて最終実行が変わったときなど）。 */
export function refreshCaptureRules(): void {
  reloadRules();
}

/** 1 つのルールのカード（Downloads.dc.html）。押すと編集する。 */
function RuleCard({ rule, onEdit }: { rule: CaptureRuleDto; onEdit: () => void }) {
  const last =
    rule.lastRunAt === null
      ? t.never
      : `${relativeDateTime(rule.lastRunAt)} · ${rule.lastStatus === 'ok' ? t.ok : t.failed}`;
  return (
    <button
      type="button"
      className={styles.ruleCard}
      data-disabled={!rule.enabled}
      aria-label={t.editRule(rule.name)}
      onClick={onEdit}
    >
      <span className={styles.ruleHead}>
        {rule.name}
        <span className={styles.badge}>{t.modes[rule.mode]}</span>
        {!rule.enabled && <span className={styles.badge}>{t.disabled}</span>}
      </span>
      <code className={styles.pattern}>{rule.urlPattern}</code>
      <span className={styles.ruleFacts}>
        {rule.mode === 'element' && rule.targetSelector && (
          <>
            <span>{t.target}</span>
            <code>{rule.targetSelector}</code>
          </>
        )}
        {rule.mode !== 'element' && (
          <>
            <span>{t.width}</span>
            <span>
              {t.widthValue(formatCount(rule.viewportWidth), formatCount(rule.scrollTimes))}
            </span>
          </>
        )}
        {rule.hideSelectors.length > 0 && (
          <>
            <span>{t.hide}</span>
            <code>{rule.hideSelectors.join(', ')}</code>
          </>
        )}
        {rule.destFolderId !== null && (
          <>
            <span>{ja.downloads.dest}</span>
            <span>{destLabel(rule.destFolderId)}</span>
          </>
        )}
        <span>{t.lastRun}</span>
        <span>{last}</span>
      </span>
    </button>
  );
}

/** 右の欄のキャプチャ：ルール・見たままキャプチャ・ログイン用に開く。 */
export function CapturePanel() {
  const [rules, setRules] = useState<CaptureRuleDto[]>([]);
  const [editing, setEditing] = useState<CaptureRuleDto | 'new' | null>(null);
  const [browserUrl, setBrowserUrl] = useState<string | null>(null);
  const [taking, setTaking] = useState(false);
  // 保存先の表示をフォルダの名前の変更に合わせる
  useSidebarStore((s) => s.folders);

  const load = useCallback(() => {
    listCaptureRules().then(setRules, fail);
  }, []);
  useEffect(() => {
    load();
    reloadRules = load;
    return () => {
      reloadRules = () => {};
    };
  }, [load]);

  // アプリ内ブラウザで開いているページ（閉じられたら消す）
  useEffect(() => {
    let alive = true;
    const check = () => {
      browserStatus().then(
        (url) => {
          if (alive) {
            setBrowserUrl(url);
          }
        },
        () => undefined,
      );
    };
    check();
    const timer = setInterval(check, BROWSER_POLL_MS);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, []);

  const startSnapshot = async () => {
    const url = await askText({
      title: t.snapshotTitle,
      label: t.snapshotLabel,
      initial: browserUrl ?? 'https://',
      confirm: t.snapshotOpen,
    });
    if (url) {
      openBrowser(url).then(() => setBrowserUrl(url), fail);
    }
  };

  const login = async () => {
    const url = await askText({
      title: t.loginTitle,
      label: t.loginLabel,
      initial: 'https://',
      confirm: t.loginOpen,
    });
    if (url === null) {
      return;
    }
    const target = url.trim() === '' || url.trim() === 'https://' ? null : url;
    openCaptureLogin(target).then(() => notify(t.loginOpened), fail);
  };

  const take = () => {
    setTaking(true);
    snapshotBrowser()
      .then((result) => {
        notify(t.taken(result.title));
        // 見たままキャプチャはジョブを通らないので、ここで一覧と件数を読み直す
        reloadList();
      }, fail)
      .finally(() => setTaking(false));
  };

  return (
    <section className={styles.sidePanel} aria-label={t.title}>
      <div className={styles.sideHead}>
        <h2 className={styles.sideTitle}>{t.title}</h2>
        <button type="button" className={styles.smallButton} onClick={() => void startSnapshot()}>
          {t.snapshot}
        </button>
      </div>
      <p className={styles.sideText}>{t.intro}</p>
      {browserUrl && (
        <div className={styles.browserRow}>
          <span className={styles.browserUrl}>{t.browserOpen(browserUrl)}</span>
          <span className={styles.browserActions}>
            <button
              type="button"
              className={`${styles.pillButton} ${styles.pillPrimary}`}
              disabled={taking}
              onClick={take}
            >
              {taking ? t.taking : t.take}
            </button>
            <button
              type="button"
              className={styles.pillButton}
              onClick={() => {
                closeBrowser().then(() => setBrowserUrl(null), fail);
              }}
            >
              {t.closeBrowser}
            </button>
          </span>
        </div>
      )}
      {rules.length === 0 && <p className={styles.sideText}>{t.noRules}</p>}
      {rules.map((rule) => (
        <RuleCard key={rule.id} rule={rule} onEdit={() => setEditing(rule)} />
      ))}
      <button type="button" className={styles.addRule} onClick={() => setEditing('new')}>
        <PlusIcon size={12} strokeWidth={2.4} />
        {t.addRule}
      </button>
      <div className={styles.sideRow}>
        <span />
        <button type="button" className={styles.smallButton} onClick={() => void login()}>
          {t.login}
        </button>
      </div>
      {editing && (
        <CaptureRuleSheet
          rule={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={load}
        />
      )}
    </section>
  );
}
