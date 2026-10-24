/**
 * `kura://localhost/...` の URL（docs/architecture.md §4）。メディアとその派生は IPC で送らず、この URL で読む。
 * `kura://` の文字列はこのファイルの外に書かない（media.test.ts が見張る。.claude/rules/frontend.md）。
 */

const BASE = 'kura://localhost';

/** アニメの帯の並べ方の版（Rust の `asset::anim::ANIM_V`。並べ方を変えたら上げる）。 */
const ANIM_V = 1;

/** `?r=`（`rev` が 0 より大きいときだけ。作り直したものを WebKit のメモリのキャッシュから出さないため）。 */
function revQuery(rev: number): string {
  return Number.isSafeInteger(rev) && rev > 0 ? `?r=${rev}` : '';
}

/**
 * サムネイル（s：長辺 512、l：長辺 1600。PNG か JPEG）。`rev` はアイテムの概要・表紙の `thumbRev`（省けない。
 * 渡し忘れると作り直した絵が出ない）。表示は `components/MediaImage` を通す（市松模様・最近傍・フォントの mask）。
 */
export function thumbUrl(hash: string, size: 's' | 'l', rev: number): string {
  return `${BASE}/thumb/${hash}/${size}${revQuery(rev)}`;
}

/** サムネイルがあるか（`ready`、または作り直すまで今の絵を出す `stale`）。 */
export function hasThumb(thumbState: string | undefined | null): boolean {
  return thumbState === 'ready' || thumbState === 'stale';
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

/** エディタのタイルの中身：行を詰めた非圧縮の RGBA8（拡張子なし）か PNG（`.png`）。 */
export type EditTileFormat = 'rgba' | 'png';

/** セッションの ID（小文字の 16 進。Rust の `editor::is_session_id`）。 */
const SESSION_ID = /^[0-9a-f]{1,64}$/;

/**
 * エディタの表示のタイル（docs/editing.md §12.13。immutable）：`/edit/{session}/{rev}/{level}/{frame}/{tx}/{ty}`。
 * タイルは 1 辺 256・フレームの画像の座標（右端・下端は小さい）。`rev` はそのタイルが最後に変わった番号（変わらない
 * タイルは同じ URL のまま）。Rust が応えるのは、エディタが表示した今か直前の表示の rev・段で、そのタイルが有効なときだけ。
 * 数は 0 以上の整数、段は 0〜3、セッションは小文字の 16 進（そうでなければ RangeError）。
 */
export function editTileUrl(
  session: string,
  rev: number,
  level: number,
  frame: number,
  tx: number,
  ty: number,
  format: EditTileFormat,
): string {
  const counts = [rev, frame, tx, ty];
  if (
    !SESSION_ID.test(session) ||
    !counts.every((n) => Number.isSafeInteger(n) && n >= 0) ||
    !Number.isInteger(level) ||
    level < 0 ||
    level > 3
  ) {
    throw new RangeError(
      `タイルの指定が正しくありません：${session}/${rev}/${level}/${frame}/${tx}/${ty}`,
    );
  }
  const ext = format === 'png' ? '.png' : '';
  return `${BASE}/edit/${session}/${rev}/${level}/${frame}/${tx}/${ty}${ext}`;
}
