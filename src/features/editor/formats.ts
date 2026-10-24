/**
 * エディタの出力の形式と書き出しの名前（純粋な関数。M12 の段 5b）。
 *
 * - 形式の ID と拡張子は、エンジンの形式の表（`crates/kura-engine/src/codec/formats.rs` の画像の行。先頭の拡張子が
 *   代表）と同じ。bindings に形式の表の定数がないので、エディタが使う画像の行だけをここに写す。
 * - 保存で書ける形式は docs/editing.md §9.2 の「書く」（PNG・JPEG・GIF・WebP（可逆の 1 枚だけ））。
 * - 書き出しの名前は Rust の `kura_core::export::names::export_name` と同じ規則（形式が同じなら元の名前のまま、違えば
 *   元の名前の語幹に今の拡張子）。出力の行の「書き出すときの名前：hero.webp」に使う。
 */

import type { FormatChoice, OutputSpec } from '../../ipc/edit';

/** 画像の形式の ID → 拡張子（先頭が代表）。 */
export const IMAGE_FORMAT_EXTS: Readonly<Record<string, readonly string[]>> = {
  png: ['png', 'apng'],
  jpeg: ['jpg', 'jpeg'],
  gif: ['gif'],
  webp: ['webp'],
  aseprite: ['aseprite', 'ase'],
  psd: ['psd', 'psb'],
  svg: ['svg'],
  heic: ['heic'],
  heif: ['heif'],
  avif: ['avif'],
  tiff: ['tif', 'tiff'],
  bmp: ['bmp'],
};

/** 保存で書ける画像の形式（並べる順）。 */
export const WRITABLE_IMAGE_FORMATS: readonly string[] = ['png', 'jpeg', 'gif', 'webp'];

/** 形式を書き換えられないときに使う形式（PSD・HEIC などを保存するとき）。 */
export const FALLBACK_FORMAT = 'png';

/** 拡張子（大文字・小文字は問わない）の形式の ID。知らなければ null。 */
export function formatOfExt(ext: string | null | undefined): string | null {
  if (!ext) return null;
  const e = ext.toLowerCase();
  for (const [id, exts] of Object.entries(IMAGE_FORMAT_EXTS)) {
    if (exts.includes(e)) return id;
  }
  return null;
}

/** 形式の代表の拡張子。知らなければ null。 */
export function extOfFormat(format: string): string | null {
  return IMAGE_FORMAT_EXTS[format]?.[0] ?? null;
}

/** 書ける形式か。 */
export function canWrite(format: string | null | undefined): boolean {
  return format != null && WRITABLE_IMAGE_FORMATS.includes(format);
}

/** 名前と拡張子に分ける（先頭の `.` と、最後の `.` のあとが空のものは拡張子とみなさない。Rust の `split_ext`）。 */
export function splitName(name: string): { stem: string; ext: string | null } {
  const i = name.lastIndexOf('.');
  return i > 0 && i + 1 < name.length
    ? { stem: name.slice(0, i), ext: name.slice(i + 1) }
    : { stem: name, ext: null };
}

/** 書き出すときの名前（Rust の `export_name` と同じ規則）。 */
export function exportName(originalName: string, ext: string): string {
  const { stem, ext: old } = splitName(originalName);
  const e = ext.trim();
  if (!e || !stem.trim()) return originalName;
  let same = false;
  if (old !== null) {
    const a = formatOfExt(old);
    const b = formatOfExt(e);
    same = a !== null && b !== null ? a === b : old.toLowerCase() === e.toLowerCase();
  }
  return same ? originalName : `${stem}.${e}`;
}

/** 文書の出力の形式の指定（出力がなければ「元と同じ」）。 */
export function outputChoice(outputs: readonly OutputSpec[]): FormatChoice {
  return outputs[0]?.format ?? { type: 'keep' };
}

/**
 * 保存したときの形式：「元と同じ」なら元の形式。元の形式に書けなければ `FALLBACK_FORMAT` にする（保存のときに
 * その形式を渡す。`forced` が真）。
 */
export function effectiveFormat(
  choice: FormatChoice,
  sourceFormat: string | null,
): { format: string; forced: boolean } {
  if (choice.type === 'to') return { format: choice.format, forced: false };
  if (sourceFormat && canWrite(sourceFormat)) return { format: sourceFormat, forced: false };
  return { format: FALLBACK_FORMAT, forced: true };
}

/**
 * 保存したあとのアイテムの拡張子（Rust の保存の段 6 と同じ：形式が今と同じなら今の拡張子、ほかは形式の代表。台帳に同じ
 * 中身の行があるときの拡張子は保存するまで分からないので見ない）。
 */
export function savedExt(format: string, currentExt: string): string {
  return formatOfExt(currentExt) === format ? currentExt : (extOfFormat(format) ?? currentExt);
}
