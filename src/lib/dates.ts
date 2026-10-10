const DAY_MS = 86_400_000;
const WEEKDAYS = ['日', '月', '火', '水', '木', '金', '土'] as const;

/** 端末の時間帯での「1970-01-01 からの日数」。 */
export function localDay(ms: number, offsetMinutes = -new Date(ms).getTimezoneOffset()): number {
  return Math.floor((ms + offsetMinutes * 60_000) / DAY_MS);
}

/**
 * 日付の見出し（「今日」「昨日」「10月8日（木）」、今年より前は「2025年10月8日（水）」）。
 * `day` と `today` は 1970-01-01 からの日数。
 */
export function dayLabel(day: number, today: number): string {
  if (day === today) {
    return '今日';
  }
  if (day === today - 1) {
    return '昨日';
  }
  const date = new Date(day * DAY_MS);
  const thisYear = new Date(today * DAY_MS).getUTCFullYear();
  const y = date.getUTCFullYear();
  const md = `${date.getUTCMonth() + 1}月${date.getUTCDate()}日（${WEEKDAYS[date.getUTCDay()]}）`;
  return y === thisYear ? md : `${y}年${md}`;
}

/** 日時を「2026年10月8日 21:14」の形にする（端末の時間帯）。 */
export function dateTimeLabel(ms: number): string {
  const d = new Date(ms);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  return `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日 ${hh}:${mm}`;
}
