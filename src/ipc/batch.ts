/**
 * 一括加工（M14 段 5。docs/editing.md §14.2）：事前チェック（進みと中止）・一括を積む・結果・1 件ずつの行・［元に戻す…］。
 * 一時停止・再開・中止はジョブの口（`src/ipc/jobs.ts`）。
 */

import {
  type Anchor,
  type BatchActionDto,
  type BatchGroupDto,
  type BatchModeDto,
  type BatchPrecheckDto,
  type BatchPrecheckProgress,
  type BatchRequestDto,
  type BatchResultDto,
  type BatchRowDto,
  type BoundExampleDto,
  commands,
  type DiskEstimateDto,
  events,
  type LossDto,
  type LossGroupDto,
  type MergeRequestDto,
  type OutputGroupDto,
  type PrecheckGroupDto,
  type RecipeSourceDto,
  type ShapeChangeDto,
  type ShapeInfoDto,
  type SheetInfoDto,
  type SheetParams,
} from './bindings';
import { unwrap } from './result';

export type {
  Anchor,
  BatchActionDto,
  BatchGroupDto,
  BatchModeDto,
  BatchPrecheckDto,
  BatchPrecheckProgress,
  BatchRequestDto,
  BatchResultDto,
  BatchRowDto,
  BoundExampleDto,
  DiskEstimateDto,
  LossDto,
  LossGroupDto,
  MergeRequestDto,
  OutputGroupDto,
  PrecheckGroupDto,
  RecipeSourceDto,
  ShapeChangeDto,
  ShapeInfoDto,
  SheetInfoDto,
  SheetParams,
};

/** 別名の名前の型の既定（Rust の `DEFAULT_SAVE_AS_NAME`）。 */
export const DEFAULT_SAVE_AS_NAME = '{名前} のコピー';

/** スプライトシートの名前の既定（Rust の `DEFAULT_SHEET_NAME`）。 */
export const DEFAULT_SHEET_NAME = '{名前} のシート';

/**
 * 事前チェック（描かずに確かめる）。`token` は呼び手が決める印で、進みのイベントと中止に使う。中止すると code が
 * `Conflict` の AppError で失敗する（Rust の Canceled。エディタを開く途中の中止と同じ）。
 */
export function precheckBatch(token: string, request: BatchRequestDto): Promise<BatchPrecheckDto> {
  return unwrap(commands.batchPrecheck(token, request));
}

/** 動いている事前チェックを止める（シートを閉じたとき）。 */
export function cancelPrecheck(token: string): Promise<null> {
  return unwrap(commands.batchPrecheckCancel(token));
}

/** 事前チェックの進み（`batch://precheck`）を受け取る。戻り値で受け取りをやめる。 */
export function onPrecheckProgress(
  handler: (event: BatchPrecheckProgress) => void,
): Promise<() => void> {
  return events.batchPrecheck.listen((e) => handler(e.payload));
}

/** 一括のジョブを積む（ジョブの ID）。 */
export function startBatch(request: BatchRequestDto): Promise<number> {
  return unwrap(commands.batchStart(request));
}

/** 一括の結果（結果のシート）。 */
export function batchResult(job: number): Promise<BatchResultDto> {
  return unwrap(commands.batchResult(job));
}

/** 一括の行（見えている分だけ。`state`・`code` で絞る）。 */
export function batchRows(
  job: number,
  filter: { state?: string; code?: string },
  offset: number,
  limit: number,
): Promise<BatchRowDto[]> {
  return unwrap(commands.batchRows(job, filter.state ?? null, filter.code ?? null, offset, limit));
}

/** 一括の［元に戻す…］を積む（ジョブの ID）。 */
export function undoBatch(job: number): Promise<number> {
  return unwrap(commands.batchUndo(job));
}

/**
 * N → 1（スプライトシート）の事前チェック（元ごとの飛ばす理由と、シートの見込み `sheet`）。進み・中止は
 * `precheckBatch` と同じ（`onPrecheckProgress`・`cancelPrecheck`）。
 */
export function precheckMerge(token: string, request: MergeRequestDto): Promise<BatchPrecheckDto> {
  return unwrap(commands.mergePrecheck(token, request));
}

/** N → 1（スプライトシート）の一括を積む（ジョブの ID）。結果・［元に戻す…］は `batchResult`・`undoBatch`。 */
export function startMerge(request: MergeRequestDto): Promise<number> {
  return unwrap(commands.mergeStart(request));
}
