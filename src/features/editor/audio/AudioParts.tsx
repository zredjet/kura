import { OpsPanel } from '../OpsPanel';

/*
 * 音声エディタの殻に置く部品（design.md §3.1 の「音声エディタ」の案文）：上部の道具のカプセルと名前の下の一行、
 * 左下・右下の表示、右パネルの「操作」、下のカプセル。G7 で中身を組む。
 */

/** 上部の道具のカプセル。 */
export function AudioTools() {
  return null;
}

/** 名前の下の一行。 */
export function AudioMeta() {
  return null;
}

/** 左下・右下の表示。 */
export function AudioStatus() {
  return null;
}

/** 下のカプセル。 */
export function AudioCapsule() {
  return null;
}

/** 右パネルの「操作」。 */
export function AudioOps() {
  return <OpsPanel />;
}
