/**
 * 時刻の文字を読む（音声エディタの時刻の欄・シークの時刻の欄で共有する。純粋な関数）。
 *
 * 「1:02.345」「62.345」「1:02:03.5」「0:04」を秒にする（負・読めなければ null）。分・秒の欄は 60 未満。
 * 全角の数字・「：」「．」も読む（日本語入力のまま打ったとき。NFKC で半角にする）。
 */
export function parseTime(text: string): number | null {
  const s = text.normalize('NFKC').trim();
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
