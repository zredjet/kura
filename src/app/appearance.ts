import { getAppearance, type Appearance } from '../ipc/appearance';

/** 表示設定を `<html>` の属性に反映する。tokens.css が属性を見てガラスを不透明な面に切り替える。 */
export function applyAppearance(root: HTMLElement, appearance: Appearance): void {
  if (appearance.reduceTransparency) {
    root.dataset.reduceTransparency = 'true';
  } else {
    delete root.dataset.reduceTransparency;
  }
}

/** OS の表示設定を取得して反映する。Tauri の外（ブラウザでの確認など）では何もしない。 */
export async function syncAppearance(
  root: HTMLElement = document.documentElement,
  fetchAppearance: () => Promise<Appearance> = getAppearance,
): Promise<void> {
  try {
    applyAppearance(root, await fetchAppearance());
  } catch (error) {
    console.warn('表示設定を取得できませんでした', error);
  }
}
