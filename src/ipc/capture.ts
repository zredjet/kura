import {
  type CaptureModeDto,
  type CaptureRuleDto,
  type CaptureRuleInputDto,
  commands,
  events,
  type LoginShotEvent,
  type SnapshotResultDto,
} from './bindings';
import { unwrap } from './result';

export type {
  CaptureModeDto,
  CaptureRuleDto,
  CaptureRuleInputDto,
  LoginShotEvent,
  SnapshotResultDto,
};

/** キャプチャのルールを並び順に。 */
export function listCaptureRules(): Promise<CaptureRuleDto[]> {
  return unwrap(commands.captureRulesList());
}

/** ルールを作る（id が null）か変える。 */
export function saveCaptureRule(
  id: number | null,
  rule: CaptureRuleInputDto,
): Promise<CaptureRuleDto> {
  return unwrap(commands.captureRuleSave(id, rule));
}

/** ルールを消す。 */
export function deleteCaptureRule(id: number): Promise<null> {
  return unwrap(commands.captureRuleDelete(id));
}

/** ルールを並べ替える。 */
export function reorderCaptureRules(ids: number[]): Promise<null> {
  return unwrap(commands.captureRulesReorder(ids));
}

/** URL をルールのとおりに撮る（ジョブの ID）。 */
export function runCapture(url: string, ruleId: number): Promise<number> {
  return unwrap(commands.captureRun(url, ruleId));
}

/** ログイン用に Chrome を開く。 */
export function openCaptureLogin(url: string | null): Promise<null> {
  return unwrap(commands.captureLogin(url));
}

/** ログイン用の Chrome が開いているか。 */
export function captureLoginStatus(): Promise<boolean> {
  return commands.captureLoginStatus();
}

/** ログイン用の Chrome の前面のタブを撮って入れる（`fullPage` ならページ全体）。 */
export function snapshotLoginPage(fullPage: boolean): Promise<SnapshotResultDto> {
  return unwrap(commands.captureLoginSnapshot(fullPage));
}

/** ⌃⌥⌘C で撮った結果（`capture://login-shot`）を受け取る。戻り値で受け取りをやめる。 */
export function onLoginShot(handler: (event: LoginShotEvent) => void): Promise<() => void> {
  return events.captureLoginShot.listen((e) => handler(e.payload));
}

/** ログイン用の Chrome を閉じる。 */
export function closeCaptureLogin(): Promise<null> {
  return unwrap(commands.captureLoginClose());
}

/** アプリ内ブラウザで開く。 */
export function openBrowser(url: string): Promise<null> {
  return unwrap(commands.browserOpen(url));
}

/** アプリ内ブラウザで開いているページ（開いていなければ null）。 */
export function browserStatus(): Promise<string | null> {
  return commands.browserStatus();
}

/** アプリ内ブラウザを閉じる。 */
export function closeBrowser(): Promise<null> {
  return unwrap(commands.browserClose());
}

/** アプリ内ブラウザの表示範囲を撮って入れる。 */
export function snapshotBrowser(): Promise<SnapshotResultDto> {
  return unwrap(commands.browserSnapshot());
}
