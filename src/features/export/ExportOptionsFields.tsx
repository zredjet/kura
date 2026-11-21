import { useEffect, useId, useState } from 'react';

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

/**
 * 名前の規則（テンプレート・書き方・ASCII）と形式（画像・音声）と JSON の欄（書き出しのシートと持ち出しの設定で共有。M15
 * 段 6）。`items` があれば下に名前の見本（先頭の数件。形式を変えるものは印）を出し、テンプレートが読めなければ理由を出す。
 */
export function ExportOptionsFields({
  value,
  onChange,
  formats,
  items,
  manifest = true,
}: {
  value: ExportPresetDto;
  onChange: (value: ExportPresetDto) => void;
  formats: ExportFormatsDto | null;
  /** 名前の見本に使うアイテム（空なら見本を出さない）。 */
  items: readonly number[];
  /** JSON を添えるかの欄を出すか。 */
  manifest?: boolean;
}) {
  const templateId = useId();
  const [template, setTemplate] = useState(value.name.template ?? '');
  const [preview, setPreview] = useState<PreviewNameDto[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // テンプレートは打ち終わるのを少し待って渡す（打っている途中で見本を作り直さない）
  useEffect(() => {
    const next = template.trim() === '' ? null : template;
    if (next === value.name.template) return;
    const timer = setTimeout(
      () => onChange({ ...value, name: { ...value.name, template: next } }),
      300,
    );
    return () => clearTimeout(timer);
  }, [template, value, onChange]);

  const key = JSON.stringify([value, items.slice(0, 50)]);
  useEffect(() => {
    // 見本のアイテムがなければ見本を出さない（下で欄ごと出さない）
    if (items.length === 0) return;
    let stale = false;
    previewExportNames(items.slice(0, 50), { ...value, project: null })
      .then((names) => {
        if (stale) return;
        setPreview(names);
        setError(null);
      })
      .catch((e: unknown) => {
        if (stale) return;
        setPreview(null);
        setError(e instanceof Error ? e.message : String(e));
      });
    return () => {
      stale = true;
    };
    // key に値と見本のアイテムをまとめる
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return (
    <>
      <div className={styles.group}>
        <label className={styles.field} htmlFor={templateId}>
          <span className={styles.rowLabel}>{t.template}</span>
          <input
            id={templateId}
            className={styles.input}
            value={template}
            placeholder={t.templatePlaceholder}
            spellCheck={false}
            autoComplete="off"
            onChange={(e) => setTemplate(e.target.value)}
          />
          <span className={styles.hint}>{t.tokens}</span>
        </label>
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
      {items.length > 0 && (
        <div className={styles.group}>
          <span className={styles.rowLabel}>{t.preview}</span>
          {error ? (
            <p className={styles.error}>{error}</p>
          ) : preview && preview.length > 0 ? (
            <ul className={styles.preview}>
              {preview.slice(0, PREVIEW_ROWS).map((p) => (
                <li key={p.itemId}>
                  <span className={styles.previewName}>{p.name}</span>
                  {p.convert && (
                    <span className={styles.previewConvert}>{t.convertTo(p.convert)}</span>
                  )}
                </li>
              ))}
              {items.length > PREVIEW_ROWS && (
                <li className={styles.previewConvert}>
                  {t.previewMore(formatCount(items.length - PREVIEW_ROWS))}
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
