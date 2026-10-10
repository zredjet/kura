import { commands, type Appearance } from './bindings';
import { unwrap } from './result';

export type { Appearance };

/** OS の表示設定（「透明度を下げる」など）を取得する。 */
export function getAppearance(): Promise<Appearance> {
  return unwrap(commands.appearanceGet());
}
