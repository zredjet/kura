import { useEffect, useRef, useState } from 'react';

import { useRevisionStore } from '../../../stores/revisionStore';

/**
 * インスペクタの内容を読む。`key` が変わったとき（選ぶものが変わった）と、
 * ライブラリのデータが変わったときに読み直す。読み直しているあいだは前の内容を出したままにする。
 */
export function useLoaded<T>(key: string, load: () => Promise<T>): T | null {
  const revision = useRevisionStore((s) => s.revision);
  const [state, setState] = useState<{ key: string; value: T } | null>(null);
  const loadRef = useRef(load);
  useEffect(() => {
    loadRef.current = load;
  });

  useEffect(() => {
    let alive = true;
    loadRef
      .current()
      .then((value) => {
        if (alive) {
          setState({ key, value });
        }
      })
      .catch((error: unknown) => console.warn('インスペクタの内容を読めませんでした', error));
    return () => {
      alive = false;
    };
  }, [key, revision]);

  return state !== null && state.key === key ? state.value : null;
}
