import { hasThumb } from '../../ipc/media';

/** 元のファイルを出すかを決めるのに要る、アイテムの概要の値。 */
export interface StillSource {
  kind: string;
  /** アニメとして回せるか（解析の実効値。コマが 2 つ以上か、格子がある）。 */
  animated: boolean;
  thumbState: string;
}

/**
 * MediaImage の `original` に渡す元のファイル（渡さないなら undefined）。アニメの画像（GIF・APNG・アニメの WebP）は
 * `<img>` に置くと WebKit が自分で回してしまう（`prefers-reduced-motion` も見ない）ので、サムネイル（先頭のコマ）が
 * あれば元のファイルを出さない（design.md §3.1「アニメのプレビュー」：クイックルックと、M12 からはシアターでも、
 * 帯を待つ間・作れないときは今のサムネイル）。サムネイルがまだなければ元のファイル。
 * `animated` は素材の欄（`AssetDto.effective.animated`）で分かっていれば渡す（一覧の概要が古いとき）。
 */
export function stillOriginal(
  item: StillSource,
  url: string,
  animated = false,
): string | undefined {
  const moving = item.kind === 'image' && (item.animated || animated);
  return moving && hasThumb(item.thumbState) ? undefined : url;
}
