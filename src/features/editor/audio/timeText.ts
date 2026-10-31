/**
 * 音声の時刻の欄の文字（M13 の段 6。利用者の決定：時間で入れて、標本の位置を小さく添える）。純粋な関数。
 *
 * - 書く：フレームを「1:02.345」（1 時間を超えれば「1:02:03.456」）。ミリ秒に丸める。
 * - 読む：「1:02.345」「62.345」「1:02:03.5」「0:04」を秒にする（負・読めなければ null）。分・秒の欄は 60 未満。
 * - フレームにする：秒 × レートを丸める。標本の位置は欄の下に「185,220 標本」と出す（ミリ秒より細かい位置を見せる）。
 */

import { msText } from './waveMath';

/** フレームを時刻の文字にする（レートが分からなければ空）。 */
export function timeText(frames: number, rate: number): string {
  if (!(rate > 0) || !Number.isFinite(frames)) return '';
  return msText((frames * 1000) / rate, true);
}

/** 時刻の文字を秒にする（読めなければ null）。 */
export function parseTime(text: string): number | null {
  const s = text.trim().replace(/[：]/g, ':').replace(/[．]/g, '.');
  if (!/^\d+(?::\d+){0,2}(?:\.\d*)?$/.test(s)) return null;
  const parts = s.split(':');
  const last = Number(parts.pop());
  if (!Number.isFinite(last)) return null;
  let total = last;
  let scale = 60;
  // 秒の上の欄（分・時）は 60 未満の秒を前提にする
  if (parts.length > 0 && last >= 60) return null;
  for (let i = parts.length - 1; i >= 0; i--) {
    const v = Number(parts[i]);
    if (!Number.isInteger(v) || v < 0) return null;
    if (i > 0 && v >= 60) return null;
    total += v * scale;
    scale *= 60;
  }
  return total;
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
