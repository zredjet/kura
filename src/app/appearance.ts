import { getAppearance, type Appearance } from '../ipc/appearance';

/** 表示設定を `<html>` の属性に反映する。tokens.css が属性を見てガラスを不透明な面に切り替える。 */
export function applyAppearance(root: HTMLElement, appearance: Appearance): void {
  if (appearance.reduceTransparency) {
    root.dataset.reduceTransparency = 'true';
  } else {
    delete root.dataset.reduceTransparency;
  }
}

/** OS の表示設定を取得して反映する。Tauri の外（ブラウザでの確認など）では失敗を記録するだけ。 */
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

/**
 * OS の表示設定の変更に追いつく。設定はシステム設定（別のアプリ）で変えるので、
 * ウィンドウが前面に戻ったとき・見えるようになったときに読み直す。戻り値でやめる。
 */
export function watchAppearance(
  root: HTMLElement = document.documentElement,
  fetchAppearance: () => Promise<Appearance> = getAppearance,
): () => void {
  const sync = () => void syncAppearance(root, fetchAppearance);
  const onVisible = () => {
    if (document.visibilityState === 'visible') {
      sync();
    }
  };
  window.addEventListener('focus', sync);
  document.addEventListener('visibilitychange', onVisible);
  return () => {
    window.removeEventListener('focus', sync);
    document.removeEventListener('visibilitychange', onVisible);
  };
}
