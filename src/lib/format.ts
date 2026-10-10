/** 件数を「12,480」の形にする。 */
export function formatCount(n: number): string {
  return new Intl.NumberFormat('ja-JP').format(n);
}

const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB'] as const;

/** バイト数を「18.4 GB」の形にする（Finder と同じ 1000 単位。100 未満は小数 1 桁）。 */
export function formatBytes(bytes: number): string {
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1000 && unit < BYTE_UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  if (unit === 0) {
    return `${value} B`;
  }
  const digits = value < 100 ? 1 : 0;
  // 999.95 → 「1000.0」にならないよう、丸めて 1000 に届いたら次の単位にする
  const rounded = Number(value.toFixed(digits));
  if (rounded >= 1000 && unit < BYTE_UNITS.length - 1) {
    return `1.0 ${BYTE_UNITS[unit + 1]}`;
  }
  return `${rounded.toFixed(digits)} ${BYTE_UNITS[unit]}`;
}

/** ホームフォルダを「~」で表す。 */
export function tildePath(path: string, home: string | null): string {
  if (!home) {
    return path;
  }
  const base = home.endsWith('/') ? home.slice(0, -1) : home;
  return path === base || path.startsWith(`${base}/`) ? `~${path.slice(base.length)}` : path;
}
