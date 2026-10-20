import { useEffect, useState } from 'react';

import { ja } from '../../i18n/ja';
import { libraryCacheUsage, type LibraryCacheUsageDto } from '../../ipc/library';
import { formatBytes, formatCount } from '../../lib/format';

const t = ja.mediaView;

/** 行の並び（ラベルと、バイト数・ファイルの数の欄）。 */
const ROWS: readonly [
  label: string,
  bytes: keyof LibraryCacheUsageDto,
  files: keyof LibraryCacheUsageDto,
][] = [
  [t.cacheThumbs, 'thumbsBytes', 'thumbsFiles'],
  [t.cachePeaks, 'peaksBytes', 'peaksFiles'],
  [t.cacheAnim, 'animBytes', 'animFiles'],
];

/**
 * 設定の「容量」の、中身から作ったキャッシュの行（design.md §3.1：ライブラリのファイルとは別の行で「サムネイル」
 * 「波形」「アニメ」。ADR-122 を広げる）。ファイルを数えるので開いたときに 1 回だけ読み、数えている間は
 * 「数えています…」。読めなければ「—」。`<dl>` の中に置く（`<dt>`・`<dd>` の組を返す）。
 */
export function CacheUsage() {
  const [usage, setUsage] = useState<LibraryCacheUsageDto | 'loading' | 'failed'>('loading');
  useEffect(() => {
    let alive = true;
    libraryCacheUsage().then(
      (value) => {
        if (alive) {
          setUsage(value);
        }
      },
      (error: unknown) => {
        console.warn('キャッシュの大きさを数えられませんでした', error);
        if (alive) {
          setUsage('failed');
        }
      },
    );
    return () => {
      alive = false;
    };
  }, []);

  return (
    <>
      {ROWS.map(([label, bytes, files]) => (
        <CacheRow
          key={label}
          label={label}
          value={
            usage === 'loading'
              ? t.cacheCounting
              : usage === 'failed'
                ? '—'
                : t.cacheValue(formatBytes(usage[bytes]), formatCount(usage[files]))
          }
        />
      ))}
    </>
  );
}

function CacheRow({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}
