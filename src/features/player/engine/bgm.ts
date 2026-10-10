import type { SlotKind } from './types';

/** BGM（直前の音楽スロットの曲）の状態。 */
export type BgmStatus = 'none' | 'playing' | 'paused' | 'ended';

/** スロットに入ったときの BGM の扱い（docs/playlists.md §6.2）。 */
export type BgmAction =
  /** 音楽のスロット：その曲に切り替える（流れていれば 300ms のクロスフェード）。 */
  | 'switch'
  /** そのまま。 */
  | 'keep'
  /** 動画のスロット：300ms でフェードアウトして一時停止（動画は自分の音声を使う）。 */
  | 'fadeOutPause'
  /** 動画のあとの画像：一時停止していた曲を 300ms で戻す。 */
  | 'fadeInResume'
  /** 画像の間に終わっていた曲を頭から（「BGM をループ」）。 */
  | 'restart'
  /** 「画像の間も直前の音楽を流す」がオフ：フェードアウトして止める。 */
  | 'fadeOutStop'
  /** 一時停止・終わった曲を片付ける。 */
  | 'stop';

/** スロットに入ったときの BGM の扱いを決める（純粋関数）。 */
export function bgmOnEnter(
  kind: SlotKind,
  status: BgmStatus,
  options: { throughImages: boolean; loop: boolean },
): BgmAction {
  if (kind === 'audio') {
    return 'switch';
  }
  if (kind === 'video') {
    return status === 'playing' ? 'fadeOutPause' : 'keep';
  }
  if (!options.throughImages) {
    if (status === 'playing') {
      return 'fadeOutStop';
    }
    return status === 'none' ? 'keep' : 'stop';
  }
  switch (status) {
    case 'paused':
      return 'fadeInResume';
    case 'ended':
      return options.loop ? 'restart' : 'keep';
    default:
      return 'keep';
  }
}

/** 画像の間に流していた曲が終わったときの扱い：ループするなら頭から、しないなら無音。 */
export function bgmOnTrackEnd(options: { loop: boolean }): 'restart' | 'silence' {
  return options.loop ? 'restart' : 'silence';
}
