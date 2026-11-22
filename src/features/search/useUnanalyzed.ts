import { useEffect, useState } from 'react';

import type { ListView } from '../../ipc/items';
import { queryParse } from '../../ipc/search';
import { joinClauses } from './clauses';

/**
 * 検索の条件が解析で決まるフィールド（透過・ループ・ラウドネスなど）を使うとき、一覧の中で解析がまだのために
 * 一致が決まっていない件数（なければ 0。design.md §3.1「未解析 N 件」）。数えるのは Rust（`query_parse`）。
 * `clauses` が空なら IPC を呼ばない。
 */
export function useUnanalyzedCount(view: ListView, clauses: readonly string[]): number {
  const text = joinClauses(clauses);
  const [counted, setCounted] = useState<{ key: string; n: number } | null>(null);
  const key = `${JSON.stringify(view)}\u0000${text}`;

  useEffect(() => {
    if (!text) {
      return;
    }
    let alive = true;
    queryParse(text, view)
      .then((result) => {
        if (alive) {
          setCounted({ key, n: result.unanalyzed ?? 0 });
        }
      })
      .catch((error: unknown) => console.warn('未解析の件数を数えられませんでした', error));
    return () => {
      alive = false;
    };
    // view は key に含めて比べる（同じ中身の新しいオブジェクトで数え直さない）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  return text && counted?.key === key ? counted.n : 0;
}
