import { commands, events, type JobDto, type JobFilterDto, type JobProgress } from './bindings';
import { unwrap } from './result';

export type { JobDto, JobFilterDto, JobProgress };

/** ジョブを新しい順に一覧する。 */
export function listJobs(filter: JobFilterDto | null = null): Promise<JobDto[]> {
  return unwrap(commands.jobsList(filter));
}

/** 中止する。 */
export function cancelJob(id: number): Promise<null> {
  return unwrap(commands.jobCancel(id));
}

/** 一時停止する。 */
export function pauseJob(id: number): Promise<null> {
  return unwrap(commands.jobPause(id));
}

/** 一時停止したジョブを再開する。 */
export function resumeJob(id: number): Promise<null> {
  return unwrap(commands.jobResume(id));
}

/** 失敗・中止したジョブをもう一度動かす。 */
export function retryJob(id: number): Promise<null> {
  return unwrap(commands.jobRetry(id));
}

/** 待っているジョブを先に動かす。 */
export function prioritizeJob(id: number): Promise<null> {
  return unwrap(commands.jobPrioritize(id));
}

/** ジョブのログ（末尾）。 */
export function jobLog(id: number): Promise<string> {
  return unwrap(commands.jobLog(id));
}

/** 進捗イベント（`job://progress`）を受け取る。戻り値で受け取りをやめる。 */
export function onJobProgress(handler: (event: JobProgress) => void): Promise<() => void> {
  return events.jobProgress.listen((e) => handler(e.payload));
}

/** 一覧の 1 件を進捗イベントの形にする。 */
export function progressFromJob(job: JobDto): JobProgress {
  return {
    id: job.id,
    kind: job.kind,
    state: job.state,
    progress: job.progress,
    message: job.message,
    resultJson: job.resultJson,
  };
}
