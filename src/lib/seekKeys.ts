import { isImeKey } from './ime';
import { isReplayed } from './keys';
import { clampSeek, FINE_STEP_MS, frameStepTarget } from './seek';

/**
 * シークのキー（ADR-230。シアターとクイックルックの動画・音声で共有する）。
 *
 * | キー | 動き |
 * |---|---|
 * | , / . | 止めて 1 コマ戻す / 進める（コマが分からない・動画でなければ 0.1 秒） |
 * | ⇧, / ⇧.（< >） | 1 秒戻す / 進める（打たれた文字「<」「>」で見る） |
 * | J / L | 5 秒戻す / 進める |
 * | K | 再生 / 一時停止 |
 *
 * ⌘・⌃・⌥ と一緒・メニューが押したことにしたキー（「ループを試聴（L）」など）・日本語入力の変換中は受けない。
 * シークは今のアイテムの中だけ（0 と終わりの手前で止まる）。
 */

export type SeekKeyAction =
  { kind: 'frame'; dir: 1 | -1 } | { kind: 'by'; ms: number } | { kind: 'toggle' };

/** J・L で動かす幅。 */
export const JUMP_MS = 5000;
/** ⇧, ⇧. で動かす幅。 */
export const NUDGE_MS = 1000;

/** キーからシークの動きを決める（シークのキーでなければ null）。 */
export function seekKeyAction(e: KeyboardEvent): SeekKeyAction | null {
  if (e.metaKey || e.ctrlKey || e.altKey || isImeKey(e) || isReplayed(e)) {
    return null;
  }
  // 打たれた文字で見る（Dvorak などでも「,」「.」「<」「>」の字のキー。⇧, ⇧. は US・JIS とも「<」「>」）
  if (e.key === '<' || e.key === '>') {
    return { kind: 'by', ms: e.key === '<' ? -NUDGE_MS : NUDGE_MS };
  }
  if ((e.key === ',' || e.key === '.') && !e.shiftKey) {
    return { kind: 'frame', dir: e.key === ',' ? -1 : 1 };
  }
  if (e.shiftKey) {
    return null;
  }
  switch (e.key.toLowerCase()) {
    case 'j':
      return { kind: 'by', ms: -JUMP_MS };
    case 'l':
      return { kind: 'by', ms: JUMP_MS };
    case 'k':
      return { kind: 'toggle' };
    default:
      return null;
  }
}

/** シークのキーを当てる先（シアターのプレーヤー・クイックルックの `<video>`／`<audio>`）。 */
export interface SeekTarget {
  /** 今の位置（ms）。 */
  positionMs(): number;
  /** 今のアイテムの終わり（ms。トリムがあればトリムの終わり。分からなければ null）。 */
  endMs(): number | null;
  /** 飛ぶ（先は `applySeekAction` が 0 と終わりの手前に収めて渡す。コマ送りは終わりの半コマ手前まで）。 */
  seek(ms: number): void;
  pause(): void;
  togglePlay(): void;
  /** 1 コマの長さ（ms。動画でない・分からなければ null で、コマ送りは 0.1 秒）。 */
  frameMs(): number | null;
}

/** シークの動きを当てる。 */
export function applySeekAction(action: SeekKeyAction, target: SeekTarget): void {
  switch (action.kind) {
    case 'toggle':
      target.togglePlay();
      return;
    case 'by':
      target.seek(clampSeek(target.positionMs() + action.ms, target.endMs()));
      return;
    case 'frame': {
      target.pause();
      const frame = target.frameMs();
      const pos = target.positionMs();
      const end = target.endMs();
      target.seek(
        frame !== null && frame > 0
          ? frameStepTarget(pos, frame, action.dir, end)
          : clampSeek(pos + action.dir * FINE_STEP_MS, end),
      );
      return;
    }
  }
}
