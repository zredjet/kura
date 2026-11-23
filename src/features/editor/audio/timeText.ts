/**
 * 音声の時刻の欄の文字（M13 の段 6。利用者の決定：時間で入れて、標本の位置を小さく添える）。純粋な関数。
 *
 * - 書く：フレームを「1:02.345」（1 時間を超えれば「1:02:03.456」）。ミリ秒に丸める。
 * - 読む：「1:02.345」「62.345」「1:02:03.5」「0:04」を秒にする（`src/lib/time.ts` の `parseTime`。全角も読む）。
 * - フレームにする：秒 × レートを丸める。標本の位置は欄の下に「185,220 標本」と出す（ミリ秒より細かい位置を見せる）。
 */

import { parseTime } from '../../../lib/time';
import { msText } from './waveMath';

/** フレームを時刻の文字にする（レートが分からなければ空）。 */
export function timeText(frames: number, rate: number): string {
  if (!(rate > 0) || !Number.isFinite(frames)) return '';
  return msText((frames * 1000) / rate, true);
}

/** 時刻の文字をフレームにする（`[min, max]` に収める。読めなければ null）。 */
export function parseFrames(
  text: string,
  rate: number,
  min = 0,
  max = Number.POSITIVE_INFINITY,
): number | null {
  const sec = parseTime(text);
  if (sec === null || !(rate > 0)) return null;
  return Math.min(max, Math.max(min, Math.round(sec * rate)));
}
