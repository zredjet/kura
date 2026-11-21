/**
 * ドラッグアウト（M15 段 5。ADR-207）：アプリの中のドラッグ（タイル・リストの行）が窓の外へ出たら、OS のドラッグに切り替える。
 * 名前と形式を整えたファイルは Rust が一時置き場に作り、落とす先にファイルの URL で渡す（Godot・Unity・Finder など）。
 * 窓に戻ってきて Kura 自身に落としても取り込まない（Swift がドラッグのあいだ Kura の窓を落とす先から外し、念のため
 * 一時置き場のファイルのドロップも `isOwnDrop` で見分けて捨てる）。
 */

import { isTauri } from '@tauri-apps/api/core';
import { useEffect } from 'react';

import { ja } from '../../i18n/ja';
import { onDragOutEnded, startDragOut } from '../../ipc/dragout';
import { formatCount } from '../../lib/format';
import { notify } from '../../stores/noticeStore';

/** これまでの回の一時置き場のフォルダ（Kura 自身へのドロップを見分ける。開いている間だけ覚える。NFC で持つ）。 */
const ownDirs = new Set<string>();

/** パスを比べる形にする（macOS の URL は名前を NFD に分解して返すことがある）。 */
function comparable(path: string): string {
  return path.normalize('NFC');
}

/** ページの座標 (x, y) が窓（WebView の表示範囲）の外か。 */
export function outsideWindow(
  x: number,
  y: number,
  width = window.innerWidth,
  height = window.innerHeight,
): boolean {
  return x < 0 || y < 0 || x >= width || y >= height;
}

/** 落ちてきたパスがすべてドラッグアウトの一時置き場のものか（Kura から出たドラッグが戻ってきた）。 */
export function isOwnDrop(paths: readonly string[]): boolean {
  if (paths.length === 0 || ownDirs.size === 0) {
    return false;
  }
  return paths.every((raw) => {
    const path = comparable(raw);
    for (const dir of ownDirs) {
      if (path === dir || path.startsWith(`${dir}/`)) {
        return true;
      }
    }
    return false;
  });
}

function messageOf(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** OS のドラッグを始める（選んだ一覧の ID）。始められなかったとき（ボタンが離れていた）は何もしない。 */
export async function beginDragOut(ids: readonly number[]): Promise<boolean> {
  try {
    const result = await startDragOut([...ids]);
    ownDirs.add(comparable(result.dir));
    if (result.started && result.skipped > 0) {
      notify(ja.dragOut.skipped(formatCount(result.skipped)));
    }
    return result.started;
  } catch (error) {
    console.warn('ドラッグアウトを始められませんでした', error);
    notify(ja.dragOut.failed(messageOf(error)));
    return false;
  }
}

/** 終わったときに知らせる一言（渡せなかった項目・フォルダの中で作れなかったファイル。なければ null）。 */
export function endedMessage(missed: number, incomplete: number): string | null {
  if (missed > 0 && incomplete > 0) {
    return ja.dragOut.missedAndIncomplete(formatCount(missed), formatCount(incomplete));
  }
  if (missed > 0) {
    return ja.dragOut.missed(formatCount(missed));
  }
  if (incomplete > 0) {
    return ja.dragOut.incomplete(formatCount(incomplete));
  }
  return null;
}

/** ドラッグアウトが終わった知らせを受け、渡せなかったものがあれば知らせる（ライブラリの画面）。 */
export function useDragOutEvents(): void {
  useEffect(() => {
    if (!isTauri()) {
      return;
    }
    let unlisten: (() => void) | null = null;
    let disposed = false;
    onDragOutEnded((event) => {
      // 落とす先が求めたのに渡せなかった項目と、渡したフォルダの中で作れなかったファイルを知らせる（求められずに終わった
      // 回はやめたドラッグなので知らせない）
      const message = endedMessage(event.missed, event.incomplete);
      if (message) {
        notify(message);
      }
    })
      .then((fn) => {
        if (disposed) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((error: unknown) => console.warn('ドラッグアウトの知らせを受けられません', error));
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
}

/** 試験用：覚えた一時置き場を忘れる。 */
export function resetDragOutForTest(): void {
  ownDirs.clear();
}
