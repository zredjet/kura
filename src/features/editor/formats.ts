/**
 * エディタの出力の形式と書き出しの名前（純粋な関数。M12 の段 5b、M13 の段 6a で種類ごとの表に分けた）。
 *
 * - 形式の表（形式の ID → 拡張子・保存で書ける形式・書けないときの形式）は種類ごとに持つ（画像は
 *   `image/imageFormats.ts`）。ここは表から関数を作る `formatTable` と、表によらない関数だけを置く。アイテムの種類から表を
 *   引くのは `kindTraits.ts` の `formatsFor`。
 * - 書き出しの名前は Rust の `kura_core::export::names::export_name` と同じ規則（形式が同じなら元の名前のまま、違えば
 *   元の名前の語幹に今の拡張子）。出力の行の「書き出すときの名前：hero.webp」に使う。
 */

import type { FormatChoice, OutputSpec } from '../../ipc/edit';

/** 種類ごとの形式の表（エンジンの形式の表の行を写す）。 */
export interface FormatTableDef {
  /** 形式の ID → 拡張子（先頭が代表）。 */
  exts: Readonly<Record<string, readonly string[]>>;
  /** 保存で書ける形式（並べる順）。 */
  writable: readonly string[];
  /** 元の形式に書けないときに使う形式。 */
  fallback: string;
  /** 書くのに ffmpeg が要る形式（なければ書けない。形式を選ぶシートで「ffmpeg が要ります」と添える）。 */
  ffmpeg?: readonly string[];
}

/** 形式の表と、表から作った関数（関数は `this` を使わないので、分けて取り出してよい）。 */
export interface FormatTable extends FormatTableDef {
  /** 書くのに ffmpeg が要る形式か。 */
  needsFfmpeg(format: string | null | undefined): boolean;
  /** 拡張子（大文字・小文字は問わない）の形式の ID。知らなければ null。 */
  formatOfExt(ext: string | null | undefined): string | null;
  /** 形式の代表の拡張子。知らなければ null。 */
  extOfFormat(format: string): string | null;
  /** 書ける形式か。 */
  canWrite(format: string | null | undefined): boolean;
  /** 書き出すときの名前（Rust の `export_name` と同じ規則）。 */
  exportName(originalName: string, ext: string): string;
  /**
   * 保存したときの形式：「元と同じ」なら元の形式。元の形式に書けなければ `fallback` にする（保存のときにその形式を渡す。
   * `forced` が真）。
   */
  effectiveFormat(
    choice: FormatChoice,
    sourceFormat: string | null,
  ): { format: string; forced: boolean };
  /**
   * 保存したあとのアイテムの拡張子（Rust の保存の段 6 と同じ：形式が今と同じなら今の拡張子、ほかは形式の代表。台帳に
   * 同じ中身の行があるときの拡張子は保存するまで分からないので見ない）。
   */
  savedExt(format: string, currentExt: string): string;
}

/** 名前と拡張子に分ける（先頭の `.` と、最後の `.` のあとが空のものは拡張子とみなさない。Rust の `split_ext`）。 */
export function splitName(name: string): { stem: string; ext: string | null } {
  const i = name.lastIndexOf('.');
  return i > 0 && i + 1 < name.length
    ? { stem: name.slice(0, i), ext: name.slice(i + 1) }
    : { stem: name, ext: null };
}

/** 文書の出力の形式の指定（出力がなければ「元と同じ」）。 */
export function outputChoice(outputs: readonly OutputSpec[]): FormatChoice {
  return outputs[0]?.format ?? { type: 'keep' };
}

/** 表から関数を作る。 */
export function formatTable(def: FormatTableDef): FormatTable {
  const formatOfExt = (ext: string | null | undefined): string | null => {
    if (!ext) return null;
    const e = ext.toLowerCase();
    for (const [id, exts] of Object.entries(def.exts)) {
      if (exts.includes(e)) return id;
    }
    return null;
  };
  const extOfFormat = (format: string): string | null => def.exts[format]?.[0] ?? null;
  const canWrite = (format: string | null | undefined): boolean =>
    format != null && def.writable.includes(format);
  const needsFfmpeg = (format: string | null | undefined): boolean =>
    format != null && (def.ffmpeg ?? []).includes(format);
  const exportName = (originalName: string, ext: string): string => {
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
  };
  const effectiveFormat = (
    choice: FormatChoice,
    sourceFormat: string | null,
  ): { format: string; forced: boolean } => {
    if (choice.type === 'to') return { format: choice.format, forced: false };
    if (sourceFormat && canWrite(sourceFormat)) return { format: sourceFormat, forced: false };
    return { format: def.fallback, forced: true };
  };
  const savedExt = (format: string, currentExt: string): string =>
    formatOfExt(currentExt) === format ? currentExt : (extOfFormat(format) ?? currentExt);
  return {
    ...def,
    formatOfExt,
    extOfFormat,
    canWrite,
    needsFfmpeg,
    exportName,
    effectiveFormat,
    savedExt,
  };
}
