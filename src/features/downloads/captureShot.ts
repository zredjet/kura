import { ja } from '../../i18n/ja';
import { onLoginShot, type SnapshotResultDto } from '../../ipc/capture';
import { reloadList } from '../../stores/jobsStore';
import { notify } from '../../stores/noticeStore';

const t = ja.downloads.capture;

/** 撮った結果の知らせの文。 */
export function snapshotMessage(result: SnapshotResultDto): string {
  const message =
    result.added === 0
      ? t.takenDuplicate(result.title)
      : result.ruleName
        ? t.takenWithRule(result.title, result.ruleName)
        : t.taken(result.title);
  return result.truncated ? `${message}${t.takenTruncated}` : message;
}

/** 撮ったことを知らせて、一覧と件数を読み直す（ジョブを通らないので）。 */
export function reportSnapshot(result: SnapshotResultDto): void {
  notify(snapshotMessage(result));
  reloadList();
}

/** ⌃⌥⌘C（ログイン用の Chrome を見ながら撮る）の結果の受け取りを始める。戻り値で止める。 */
export function startLoginShotEvents(): () => void {
  let stop: (() => void) | null = null;
  let stopped = false;
  onLoginShot((event) => {
    if (event.ok && event.result) {
      reportSnapshot(event.result);
    } else {
      notify(event.message);
    }
  })
    .then((unlisten) => {
      if (stopped) {
        unlisten();
      } else {
        stop = unlisten;
      }
    })
    .catch((error: unknown) => console.warn('撮影の結果を受け取れませんでした', error));
  return () => {
    stopped = true;
    stop?.();
  };
}
