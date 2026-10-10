/** `kura://localhost/...` の URL（docs/architecture.md §4）。メディアは IPC で送らず、この URL で読む。 */

const BASE = 'kura://localhost';

/** サムネイル（s：長辺 512、l：長辺 1600）。 */
export function thumbUrl(hash: string, size: 's' | 'l'): string {
  return `${BASE}/thumb/${hash}/${size}`;
}

/** 音楽のジャケット。 */
export function artUrl(hash: string): string {
  return `${BASE}/art/${hash}`;
}

/** 元ファイル。 */
export function fileUrl(hash: string): string {
  return `${BASE}/file/${hash}`;
}

/** 再生用（プロキシがあればプロキシ）。Range に対応している。 */
export function playUrl(hash: string): string {
  return `${BASE}/play/${hash}`;
}
