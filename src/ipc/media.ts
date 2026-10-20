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

// ===== 班 D2 が使う口（班 D1 の media.ts と同じ名前・引数・中身の写し。取り込みでは D1 の方を採る）=====

/** アニメの帯の並べ方の版（Rust の `asset::anim::ANIM_V`。並べ方を変えたら上げる）。 */
const ANIM_V = 1;

/** `?r=`（`rev` が 0 より大きいときだけ。作り直したものを WebKit のメモリのキャッシュから出さないため）。 */
function revQuery(rev: number): string {
  return Number.isSafeInteger(rev) && rev > 0 ? `?r=${rev}` : '';
}

/** 波形の段（`kura-peaks`。immutable）。`fetch` して `src/lib/asset/peaks.ts` の `parsePeaks` で読む。 */
export function peaksUrl(hash: string): string {
  return `${BASE}/peaks/${hash}`;
}

/**
 * 試聴の PCM（フレームの半開区間 [start, end) の WAV。キャッシュしない）。`fetch` して `src/lib/asset/wav.ts` の
 * `readPcmWav` で読む。区間は 0 以上の整数で `start < end`（そうでなければ RangeError。Rust は 404 を返す）。
 */
export function pcmUrl(hash: string, start: number, end: number): string {
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || start >= end) {
    throw new RangeError(`PCM の区間が正しくありません：${start}-${end}`);
  }
  return `${BASE}/pcm/${hash}/${start}-${end}`;
}

/**
 * アニメの帯の PNG（immutable）。`rev` は `item_anim_get` の `AnimDto.rev`（格子の帯は格子ごとに変わる。ファイルのコマなら 0）。
 */
export function animUrl(hash: string, rev: number): string {
  return `${BASE}/anim/${hash}/${ANIM_V}${revQuery(rev)}`;
}
