import {
  commands,
  type DragOutEnded,
  type DragOutStartDto,
  type DragOutTargetDto,
  events,
} from './bindings';
import { unwrap } from './result';

export type { DragOutEnded, DragOutStartDto, DragOutTargetDto };

/**
 * ドラッグアウトを始める（アプリの中のドラッグが窓の外へ出たとき。M15 段 5）。選んだもの（一覧の ID）の名前と形式を
 * 持ち出し先のプロジェクトの既定で決めて一時置き場に作り始め、OS のドラッグに切り替える。マウスのボタンが離れていれば
 * 始めない（`started` が false）。作るものがない・多すぎるときは `Invalid`。
 */
export function startDragOut(entries: number[]): Promise<DragOutStartDto> {
  return unwrap(commands.dragoutStart(entries));
}

/** 持ち出し先のプロジェクト（選んだものと、名前と形式に使うもの）。 */
export function dragOutTarget(): Promise<DragOutTargetDto> {
  return unwrap(commands.dragoutTarget());
}

/** 持ち出し先のプロジェクトを選ぶ（null で選ばない）。 */
export function setDragOutTarget(project: number | null): Promise<null> {
  return unwrap(commands.dragoutTargetSet(project));
}

/** ドラッグアウトが終わった知らせを受け取る。 */
export function onDragOutEnded(handler: (event: DragOutEnded) => void): Promise<() => void> {
  return events.dragoutEnded.listen((e) => handler(e.payload));
}
