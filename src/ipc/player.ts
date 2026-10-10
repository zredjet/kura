import {
  commands,
  type ExternalPlayerDto,
  type PlayerPrefsDto,
  type QueueDto,
  type SupportEntryDto,
} from './bindings';
import { unwrap } from './result';

export type { ExternalPlayerDto, PlayerPrefsDto, QueueDto, SupportEntryDto };

/** 起動時に作った再生可否の表を渡す（表が変わっていればプロキシの判定をやり直す）。 */
export function setPlaybackSupport(entries: SupportEntryDto[]): Promise<null> {
  return unwrap(commands.playbackSupportSet(entries));
}

/** 選択（アイテムと、まとめて表示したユニット）を並び順のまま再生キューにする。 */
export function playerQueue(entries: number[]): Promise<QueueDto> {
  return unwrap(commands.playerQueue(entries));
}

/** 1 スロット分の再生の報告（docs/playlists.md §6.4）。再生済みとして記録したら true。 */
export interface PlayReport {
  itemId: number;
  unitId: number | null;
  unitStart: boolean;
  playedMs: number;
  completed: boolean;
  durationMs: number | null;
}

/** スロットを離れたときに報告する。 */
export function reportPlayback(report: PlayReport): Promise<boolean> {
  return unwrap(commands.playbackReport(report));
}

/** プロキシを先に作ってもらう。`force` なら、元のまま再生できなかったものを作り直す。 */
export function requestProxies(ids: number[], force = false): Promise<null> {
  return unwrap(commands.proxyRequest(ids, force));
}

/** 元のファイルを外部プレイヤーで開く。 */
export function openExternal(hash: string): Promise<ExternalPlayerDto> {
  return unwrap(commands.openExternal(hash));
}

/** 再生の設定。 */
export function getPlayerPrefs(): Promise<PlayerPrefsDto> {
  return commands.playerPrefsGet();
}

/** 再生の設定を保存する（丸めた値が返る）。 */
export function setPlayerPrefs(prefs: PlayerPrefsDto): Promise<PlayerPrefsDto> {
  return unwrap(commands.playerPrefsSet(prefs));
}
