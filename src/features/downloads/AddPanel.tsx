import { type FormEvent, type MouseEvent, useEffect, useState } from 'react';

import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { askChoice } from '../../components/Overlay/overlayStore';
import { CameraIcon } from '../../components/icons/CameraIcon';
import { CheckIcon } from '../../components/icons/CheckIcon';
import { ChevronDownIcon } from '../../components/icons/ChevronDownIcon';
import { LinkIcon } from '../../components/icons/LinkIcon';
import { WarningIcon } from '../../components/icons/WarningIcon';
import { ja } from '../../i18n/ja';
import {
  type DetectionDto,
  type DownloadOptionsDto,
  detectDownload,
  downloadDefaults,
  enqueueDownload,
  type QualityDto,
  toolCommand,
} from '../../ipc/downloads';
import { runCapture } from '../../ipc/capture';
import { useDownloadsStore } from '../../stores/downloadsStore';
import { notify } from '../../stores/noticeStore';
import { useSidebarStore } from '../../stores/sidebarStore';
import { destLabel, fail, pickDestFolder } from './downloadActions';
import styles from './Downloads.module.css';
import { previousMessage } from './previous';

const t = ja.downloads;

/** 判定を待つ時間（入力が止まってから）。 */
const DETECT_DELAY_MS = 250;

const QUALITIES: QualityDto[] = ['best', '1080', '720', '480'];

type AutoTags = 'both' | 'uploader' | 'site' | 'none';

function autoTagsOf(o: DownloadOptionsDto): AutoTags {
  if (o.tagUploader && o.tagSiteTags) {
    return 'both';
  }
  if (o.tagUploader) {
    return 'uploader';
  }
  return o.tagSiteTags ? 'site' : 'none';
}

const AUTO_TAG_LABELS: Record<AutoTags, string> = {
  both: t.autoTagsBoth,
  uploader: t.autoTagsUploader,
  site: t.autoTagsSite,
  none: t.autoTagsNone,
};

/** 押すと選択肢のメニューを出すチップ（「画質 最高 ▾」）。 */
function ChipMenu<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
}) {
  const current = options.find((o) => o.value === value)?.label ?? value;
  const open = (e: MouseEvent<HTMLButtonElement>) => {
    const entries: MenuEntry[] = options.map((o) => ({
      label: o.label,
      icon: o.value === value ? <CheckIcon size={12} strokeWidth={2.4} /> : null,
      onSelect: () => onChange(o.value),
    }));
    openMenuBelow(e.currentTarget, label, entries);
  };
  return (
    <button
      type="button"
      className={styles.chip}
      aria-label={`${label}：${current}`}
      aria-haspopup="menu"
      onClick={open}
    >
      <span className={styles.chipLabel}>{label}</span>
      {current}
      <ChevronDownIcon size={9} strokeWidth={2.8} />
    </button>
  );
}

/** URL を入れて取得を積む欄（Downloads.dc.html の上の面）。判定の結果と、画質・保存先などの選択。 */
export function AddPanel() {
  const [url, setUrl] = useState('');
  // 判定の結果（どの URL についてのものか）。入力が変わったら古い結果は使わない
  const [result, setResult] = useState<{
    text: string;
    detection: DetectionDto | null;
    error: string | null;
  } | null>(null);
  const [sending, setSending] = useState(false);
  const [options, setOptions] = useState<DownloadOptionsDto | null>(null);
  // 保存先の表示をフォルダの名前の変更に合わせる
  useSidebarStore((s) => s.folders);

  useEffect(() => {
    downloadDefaults().then(setOptions, fail);
  }, []);

  const text = url.trim();
  const current = result?.text === text ? result : null;
  const checking = text !== '' && current === null;
  const detection = current?.detection ?? null;
  const error = current?.error ?? null;

  // 入力が止まったら判定する
  useEffect(() => {
    if (!text) {
      return;
    }
    let canceled = false;
    const timer = setTimeout(() => {
      detectDownload(text).then(
        (d) => {
          if (!canceled) {
            setResult({ text, detection: d, error: null });
          }
        },
        (e: unknown) => {
          if (!canceled) {
            setResult({ text, detection: null, error: e instanceof Error ? e.message : String(e) });
          }
        },
      );
    }, DETECT_DELAY_MS);
    return () => {
      canceled = true;
      clearTimeout(timer);
    };
  }, [text]);

  const update = (patch: Partial<DownloadOptionsDto>) =>
    setOptions((o) => (o ? { ...o, ...patch } : o));

  // 判定を待たずに積める（終わっていなければ、押したときに判定する）。ツールがないとわかっているときと、
  // URL として読めないときだけ止める
  const toolMissing =
    detection !== null &&
    (detection.capture ? !detection.capture.chromeFound : !detection.toolFound);
  const ready = options !== null && text !== '' && !toolMissing && error === null && !sending;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (error) {
      notify(error);
      return;
    }
    if (!ready) {
      return;
    }
    setSending(true);
    try {
      const d = detection ?? (await detectDownload(text));
      if (d.capture) {
        // キャプチャのルールに一致したら撮る（ダウンロードより先。ADR）
        if (!d.capture.chromeFound) {
          notify(t.capture.chromeMissing);
          return;
        }
        await runCapture(d.url, d.capture.ruleId);
        notify(t.capture.queued);
      } else {
        if (!d.toolFound) {
          notify(t.toolMissing(toolCommand(d.tool)));
          return;
        }
        // 前にダウンロードしたものは、取得済みの記録にあるので取り直さない。取り直すかを選んでもらう（ADR-132）
        let redownload = false;
        if (d.previous) {
          const choice = await askChoice({
            title: t.previous.title,
            message: previousMessage(d.previous),
            choices: [
              { value: 'skip', label: t.previous.skip },
              { value: 'redownload', label: t.previous.redownload, primary: true },
            ],
          });
          if (choice === null) {
            return;
          }
          redownload = choice === 'redownload';
        }
        await enqueueDownload(d.url, options, redownload);
        notify(redownload ? t.redownloadAdded : t.added);
      }
      setUrl('');
      await useDownloadsStore.getState().load();
    } catch (err) {
      fail(err);
    } finally {
      setSending(false);
    }
  };

  const chooseDest = (e: MouseEvent<HTMLButtonElement>) => {
    openMenuBelow(e.currentTarget, t.dest, [
      {
        label: t.destChoose,
        onSelect: () => {
          pickDestFolder().then((id) => {
            if (id !== undefined) {
              update({ destFolderId: id });
            }
          }, fail);
        },
      },
      {
        label: t.destClear,
        disabled: options?.destFolderId === null,
        onSelect: () => update({ destFolderId: null }),
      },
    ]);
  };

  const video = detection === null || detection.tool === 'ytDlp';

  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} noValidate>
      <div className={styles.urlRow}>
        <label className={styles.urlField}>
          <LinkIcon size={16} strokeWidth={1.9} />
          <input
            className={styles.urlInput}
            type="url"
            value={url}
            aria-label={t.urlLabel}
            placeholder={t.urlPlaceholder}
            spellCheck={false}
            autoComplete="off"
            // eslint-disable-next-line jsx-a11y/no-autofocus -- 画面を開いたらすぐ貼り付けられるように
            autoFocus
            onChange={(e) => setUrl(e.target.value)}
          />
        </label>
        <button type="submit" className={styles.addButton} disabled={!ready}>
          {t.add}
        </button>
      </div>
      <div className={styles.optionRow}>
        {checking && <span className={styles.hint}>{t.detecting}</span>}
        {!checking && error && <span className={styles.warning}>{error}</span>}
        {!checking && detection?.capture && detection.capture.chromeFound && (
          <span className={styles.detected}>
            <CameraIcon size={13} strokeWidth={2} />
            {t.capture.detected(detection.capture.ruleName)} ·{' '}
            <code className={styles.mono}>Chrome</code>
          </span>
        )}
        {!checking && detection?.capture && !detection.capture.chromeFound && (
          <span className={styles.warning}>
            <WarningIcon size={14} strokeWidth={2} />
            {t.capture.chromeMissing}
          </span>
        )}
        {!checking && detection && !detection.capture && detection.toolFound && (
          <span className={styles.detected}>
            <CheckIcon size={12} strokeWidth={3} />
            {t.detected(detection.siteLabel)} ·{' '}
            <code className={styles.mono}>{toolCommand(detection.tool)}</code>
            {detection.rule && <span className={styles.hint}>（{t.byRule(detection.rule)}）</span>}
          </span>
        )}
        {!checking && detection && !detection.capture && !detection.toolFound && (
          <span className={styles.warning}>
            <WarningIcon size={14} strokeWidth={2} />
            {t.toolMissing(toolCommand(detection.tool))}
          </span>
        )}
        {options && !detection?.capture && (
          <>
            {video && (
              <>
                <ChipMenu
                  label={t.quality}
                  value={options.quality}
                  options={QUALITIES.map((q) => ({ value: q, label: t.qualities[q] }))}
                  onChange={(quality) => update({ quality })}
                />
                <button
                  type="button"
                  className={styles.chip}
                  aria-pressed={options.audioOnly}
                  onClick={() => update({ audioOnly: !options.audioOnly })}
                >
                  {t.audioOnly}
                </button>
              </>
            )}
            <button
              type="button"
              className={styles.chip}
              aria-label={`${t.dest}：${destLabel(options.destFolderId)}`}
              aria-haspopup="menu"
              onClick={chooseDest}
            >
              <span className={styles.chipLabel}>{t.dest}</span>
              {destLabel(options.destFolderId)}
              <ChevronDownIcon size={9} strokeWidth={2.8} />
            </button>
            <ChipMenu<AutoTags>
              label={t.autoTags}
              value={autoTagsOf(options)}
              options={(['both', 'uploader', 'site', 'none'] as const).map((v) => ({
                value: v,
                label: AUTO_TAG_LABELS[v],
              }))}
              onChange={(v) =>
                update({
                  tagUploader: v === 'both' || v === 'uploader',
                  tagSiteTags: v === 'both' || v === 'site',
                })
              }
            />
            {detection?.playlist && (
              <button
                type="button"
                className={styles.chip}
                aria-pressed={options.playlistUnit}
                onClick={() => update({ playlistUnit: !options.playlistUnit })}
              >
                {t.playlistUnit}
              </button>
            )}
          </>
        )}
      </div>
    </form>
  );
}
