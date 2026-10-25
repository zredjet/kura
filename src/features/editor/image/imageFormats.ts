/**
 * 画像の形式の表（M12 の段 5b。M13 の段 6a で `formats.ts` から分けた）。
 *
 * - 形式の ID と拡張子は、エンジンの形式の表（`crates/kura-engine/src/codec/formats.rs` の画像の行。先頭の拡張子が
 *   代表）と同じ。bindings に形式の表の定数がないので、エディタが使う画像の行だけをここに写す。
 * - 保存で書ける形式は docs/editing.md §9.2 の「書く」（PNG・JPEG・GIF・WebP（可逆の 1 枚だけ））。
 */

import { formatTable } from '../formats';

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

/** 画像の形式の表。 */
export const IMAGE_FORMATS = formatTable({
  exts: IMAGE_FORMAT_EXTS,
  writable: WRITABLE_IMAGE_FORMATS,
  fallback: FALLBACK_FORMAT,
});

/** 画像の表の関数（画像だけを扱う所とテストで使う。種類によらない所は `formatsFor` で表を引く）。 */
export const { formatOfExt, extOfFormat, canWrite, exportName, effectiveFormat, savedExt } =
  IMAGE_FORMATS;
