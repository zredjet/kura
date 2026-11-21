import { create } from 'zustand';

import type { ExportPresetDto, FormatTargetDto } from '../../ipc/export';

/** 書き出しのシート（選んだ一覧の ID。M15 段 6）。 */
interface ExportSheetState {
  entries: number[] | null;
}

export const useExportSheetStore = create<ExportSheetState>(() => ({ entries: null }));

/** 書き出しのシートを開く（⌘⇧E・項目のメニュー）。 */
export function openExportSheet(entries: readonly number[]): void {
  useExportSheetStore.setState({ entries: [...entries] });
}

/** 閉じる。 */
export function closeExportSheet(): void {
  useExportSheetStore.setState({ entries: null });
}

/** M9 と同じ書き出し（元の名前・形式はそのまま・JSON を添える）。 */
export const M9_PRESET: ExportPresetDto = {
  name: { template: null, style: 'keep', ascii: false },
  image: { type: 'original' },
  audio: { type: 'original' },
  manifest: true,
};

/** 前回の書き出しの名前と形式を覚える鍵（表示の設定と同じ軽いもの。プロジェクトの既定は覚えない）。 */
const LAST_KEY = 'kura.export.lastPreset';

const STYLES = ['keep', 'snake', 'kebab', 'pascal', 'camel'];

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

/** 形式の選び方として読めるか。 */
function isTarget(v: unknown): v is FormatTargetDto {
  if (!isRecord(v)) return false;
  if (v.type === 'original' || v.type === 'playable') return true;
  return v.type === 'to' && typeof v.format === 'string' && v.format !== '';
}

/** 覚えた値として読めるか（型の違う値で画面が落ちないように、項目ごとに確かめる）。 */
export function isPreset(v: unknown): v is ExportPresetDto {
  if (!isRecord(v) || !isRecord(v.name)) return false;
  const { template, style, ascii } = v.name;
  return (
    (template === null || typeof template === 'string') &&
    typeof style === 'string' &&
    STYLES.includes(style) &&
    typeof ascii === 'boolean' &&
    isTarget(v.image) &&
    isTarget(v.audio) &&
    typeof v.manifest === 'boolean'
  );
}

/** 前回の書き出しの名前と形式（なければ・読めなければ M9）。 */
export function lastPreset(): ExportPresetDto {
  try {
    const raw = localStorage.getItem(LAST_KEY);
    if (!raw) return M9_PRESET;
    const parsed: unknown = JSON.parse(raw);
    return isPreset(parsed) ? parsed : M9_PRESET;
  } catch {
    return M9_PRESET;
  }
}

/** 書き出しの名前と形式を覚える。 */
export function rememberPreset(preset: ExportPresetDto): void {
  try {
    localStorage.setItem(LAST_KEY, JSON.stringify(preset));
  } catch {
    // 覚えられなくても書き出しはできる
  }
}
