import { useEffect, useId, useRef, useState } from 'react';

import { Segmented } from '../../components/Segmented/Segmented';
import { Select, type SelectOption } from '../../components/Select/Select';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import {
  type ExportFormatsDto,
  type ExportPresetDto,
  type FormatTargetDto,
  type NameStyleDto,
  previewExportNames,
  type PreviewNameDto,
} from '../../ipc/export';
import { formatCount } from '../../lib/format';
import styles from './Export.module.css';

const t = ja.exportOptions;

/** 見本に出す名前の数（残りは「ほか N 件」）。 */
const PREVIEW_ROWS = 6;

const STYLES: readonly NameStyleDto[] = ['keep', 'snake', 'kebab', 'pascal', 'camel'];

/** 形式の選択の値（`original`・`playable`・`to:<形式>`）。 */
export function targetValue(target: FormatTargetDto): string {
  return target.type === 'to' ? `to:${target.format}` : target.type;
}

/** 選択の値から形式の選び方に戻す。 */
export function targetOf(value: string): FormatTargetDto {
  if (value.startsWith('to:')) {
    return { type: 'to', format: value.slice(3) };
  }
  return value === 'playable' ? { type: 'playable' } : { type: 'original' };
}

/** 形式の選択肢（そのまま・変えない・書ける形式。今の値が表にない形式なら足す）。 */
export function targetOptions(
  formats: readonly string[],
  current: FormatTargetDto,
): SelectOption[] {
  const list = [...formats];
  if (current.type === 'to' && !list.includes(current.format)) {
    list.push(current.format);
  }
  return [
    { value: 'original', label: t.original },
    { value: 'playable', label: t.playable },
    ...list.map((f) => ({ value: `to:${f}`, label: t.to(f) })),
  ];
}

/** 見本を作り直すまで待つ長さ（打っている途中で見本を作り直さない）。 */
const PREVIEW_DELAY_MS = 300;

/**
 * 名前の規則（テンプレート・書き方・ASCII）と形式（画像・音声）と JSON の欄（書き出しのシートと持ち出しの設定で共有。M15
 * 段 6）。値は打つたびに渡し（すぐ書き出し・保存しても打った値が入る。変換中は確定してから）、見本は少し待ってから作り直す。
 * `entries` があれば下に名前の見本（書き出しのジョブと同じ並び・番号・ユニットのフォルダ。先頭の数件と件数。形式を変える
 * ものは印）を出し、テンプレートが読めなければ理由を出す。
 */
export function ExportOptionsFields({
  value,
  onChange,
  formats,
  entries,
  onTotal,
  manifest = true,
}: {
  value: ExportPresetDto;
  onChange: (value: ExportPresetDto) => void;
  formats: ExportFormatsDto | null;
  /** 名前の見本に使う一覧の ID（ユニットは印付き。空なら見本を出さない）。 */
  entries: readonly number[];
  /** 書き出す件数が分かったら知らせる（見本と同じ数え方）。 */
  onTotal?: (total: number) => void;
  /** JSON を添えるかの欄を出すか。 */
  manifest?: boolean;
}) {
  const templateId = useId();
  const [template, setTemplate] = useState(value.name.template ?? '');
  const [preview, setPreview] = useState<{ names: PreviewNameDto[]; total: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const composing = useRef(false);
  const onTotalRef = useRef(onTotal);
  useEffect(() => {
    onTotalRef.current = onTotal;
  });

  const commitTemplate = (text: string) => {
    const next = text.trim() === '' ? null : text;
    if (next !== value.name.template)
      onChange({ ...value, name: { ...value.name, template: next } });
  };

  const key = JSON.stringify([value, entries]);
  useEffect(() => {
    // 見本の対象がなければ見本を出さない（下で欄ごと出さない）
    if (entries.length === 0) return;
    let stale = false;
    const options = JSON.parse(key) as [ExportPresetDto, number[]];
    const timer = setTimeout(() => {
      previewExportNames(options[1], { ...options[0], project: null })
        .then((p) => {
          if (stale) return;
          setPreview(p);
          setError(null);
          onTotalRef.current?.(p.total);
        })
        .catch((e: unknown) => {
          if (stale) return;
          setPreview(null);
          setError(e instanceof Error ? e.message : String(e));
        });
    }, PREVIEW_DELAY_MS);
    return () => {
      stale = true;
      clearTimeout(timer);
    };
  }, [key, entries.length]);

  return (
    <>
      <div className={styles.group}>
        <div className={styles.field}>
          <label className={styles.rowLabel} htmlFor={templateId}>
            {t.template}
          </label>
          <input
            id={templateId}
            aria-describedby={`${templateId}-hint`}
            className={styles.input}
            value={template}
            placeholder={t.templatePlaceholder}
            spellCheck={false}
            autoComplete="off"
            onChange={(e) => {
              setTemplate(e.target.value);
              // 変換中は確定するまで渡さない（確定前の文字で見本を作らない）
              if (!composing.current) commitTemplate(e.target.value);
            }}
            onCompositionStart={() => {
              composing.current = true;
            }}
            onCompositionEnd={(e) => {
              composing.current = false;
              commitTemplate(e.currentTarget.value);
            }}
          />
          <span id={`${templateId}-hint`} className={styles.hint}>
            {t.tokens}
          </span>
        </div>
        <div className={styles.row}>
          <span className={styles.rowLabel}>{t.style}</span>
          <Segmented
            label={t.style}
            value={value.name.style}
            options={STYLES.map((s) => ({ value: s, label: t.styles[s] }))}
            onChange={(style) => onChange({ ...value, name: { ...value.name, style } })}
          />
        </div>
        <label className={styles.row}>
          <span className={styles.rowLabel}>{t.ascii}</span>
          <Switch
            checked={value.name.ascii}
            onChange={(ascii) => onChange({ ...value, name: { ...value.name, ascii } })}
          />
        </label>
      </div>
      <div className={styles.group}>
        <div className={styles.row}>
          <span className={styles.rowLabel}>{t.image}</span>
          <Select
            aria-label={t.image}
            value={targetValue(value.image)}
            options={targetOptions(formats?.image ?? [], value.image)}
            onChange={(v) => onChange({ ...value, image: targetOf(v) })}
          />
        </div>
        <div className={styles.row}>
          <span className={styles.rowLabel}>{t.audio}</span>
          <Select
            aria-label={t.audio}
            value={targetValue(value.audio)}
            options={targetOptions(formats?.audio ?? [], value.audio)}
            onChange={(v) => onChange({ ...value, audio: targetOf(v) })}
          />
        </div>
        {manifest && (
          <label className={styles.row}>
            <span className={styles.rowLabel}>{t.manifest}</span>
            <Switch
              checked={value.manifest}
              onChange={(m) => onChange({ ...value, manifest: m })}
            />
          </label>
        )}
      </div>
      {entries.length > 0 && (
        <div className={styles.group}>
          <span className={styles.rowLabel}>{t.preview}</span>
          {error ? (
            <p className={styles.error}>{error}</p>
          ) : preview && preview.names.length > 0 ? (
            <ul className={styles.preview}>
              {preview.names.slice(0, PREVIEW_ROWS).map((p, i) => (
                <li key={`${p.itemId}-${i}`}>
                  <span className={styles.previewName}>{p.name}</span>
                  {p.convert && (
                    <span className={styles.previewConvert}>{t.convertTo(p.convert)}</span>
                  )}
                </li>
              ))}
              {preview.total > PREVIEW_ROWS && (
                <li className={styles.previewConvert}>
                  {t.previewMore(formatCount(preview.total - PREVIEW_ROWS))}
                </li>
              )}
            </ul>
          ) : (
            preview && <p className={styles.hint}>{t.previewEmpty}</p>
          )}
        </div>
      )}
    </>
  );
}
