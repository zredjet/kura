/**
 * エディタの要求の振り分け（React の外。docs/editing.md §12.8）。
 *
 * - 要求は 1 つだけ出す。出している間に来た手はためておき、表示の範囲（ビューポート）は最新の 1 つだけを残す。応えが
 *   来たら、ためた手をまとめて最新のビューポートで 1 回の `edit_step` にする（手がなければ `edit_view`）。
 * - 要求には通し番号を振る。閉じたあとの応えは捨てる（仮の筆はどの要求の結果で外すかを通し番号で決める）。
 * - 手のない表示の要求は、前に送ったビューポートと同じなら送らない（`force` で送る：表示が新しい要求に取って代わられた
 *   あとなど）。
 */

import type { EditStepDto, EditStepResultDto, EditViewportDto } from '../../ipc/edit';

/** ためる手（`tag` は呼び手の印：仮の筆など）。 */
export interface QueuedStep<T> {
  step: EditStepDto;
  tag?: T;
}

/** 送った要求。 */
export interface SentRequest<T> {
  seq: number;
  steps: QueuedStep<T>[];
  viewport: EditViewportDto;
}

/** 要求を出す口（`step` は手があるとき、`view` は表示だけ）。 */
export interface QueueIpc {
  step(steps: EditStepDto[], viewport: EditViewportDto): Promise<EditStepResultDto>;
  view(viewport: EditViewportDto): Promise<EditStepResultDto>;
}

export interface QueueHandlers<T> {
  /** 送るとき（仮の筆に通し番号を付ける）。 */
  sent?(req: SentRequest<T>): void;
  done(req: SentRequest<T>, result: EditStepResultDto): void;
  error(req: SentRequest<T>, error: unknown): void;
}

function sameViewport(a: EditViewportDto | null, b: EditViewportDto | null): boolean {
  return a !== null && b !== null && JSON.stringify(a) === JSON.stringify(b);
}

export class RequestQueue<T = unknown> {
  private steps: QueuedStep<T>[] = [];
  private viewport: EditViewportDto | null = null;
  private viewWanted = false;
  private forced = false;
  private lastSent: EditViewportDto | null = null;
  private flying: SentRequest<T> | null = null;
  private seq = 0;
  private closed = false;

  constructor(
    private readonly ipc: QueueIpc,
    private readonly handlers: QueueHandlers<T>,
  ) {}

  /** 要求を出しているか。 */
  get busy(): boolean {
    return this.flying !== null;
  }

  /** ためている手の数。 */
  get waiting(): number {
    return this.steps.length;
  }

  /** 最後に振った通し番号。 */
  get lastSeq(): number {
    return this.seq;
  }

  /** 手をためて送る（ビューポートは最新に替える）。 */
  push(steps: QueuedStep<T>[], viewport: EditViewportDto): void {
    if (this.closed) return;
    this.steps.push(...steps);
    this.viewport = viewport;
    this.flush();
  }

  /** 表示だけを頼む（前に送ったものと同じなら送らない。`force` で送る）。 */
  view(viewport: EditViewportDto, force = false): void {
    if (this.closed) return;
    this.viewport = viewport;
    this.viewWanted = true;
    this.forced ||= force;
    this.flush();
  }

  /** このビューポートはもう送ったことにする（開いたときの要求。同じなら頼み直さない）。 */
  assumeSent(viewport: EditViewportDto): void {
    this.lastSent = viewport;
  }

  /** 閉じる（ためた手を捨て、出している要求の応えも捨てる）。 */
  close(): void {
    this.closed = true;
    this.steps = [];
    this.flying = null;
  }

  private flush(): void {
    if (this.closed || this.flying || !this.viewport) return;
    if (this.steps.length === 0) {
      if (!this.viewWanted) return;
      if (!this.forced && sameViewport(this.viewport, this.lastSent)) {
        this.viewWanted = false;
        return;
      }
    }
    const req: SentRequest<T> = { seq: ++this.seq, steps: this.steps, viewport: this.viewport };
    this.steps = [];
    this.viewWanted = false;
    this.forced = false;
    this.lastSent = req.viewport;
    this.flying = req;
    this.handlers.sent?.(req);
    const call =
      req.steps.length > 0
        ? this.ipc.step(
            req.steps.map((s) => s.step),
            req.viewport,
          )
        : this.ipc.view(req.viewport);
    call.then(
      (result) => {
        if (this.closed || this.flying !== req) return;
        this.flying = null;
        this.handlers.done(req, result);
        this.flush();
      },
      (error: unknown) => {
        if (this.closed || this.flying !== req) return;
        this.flying = null;
        this.handlers.error(req, error);
        this.flush();
      },
    );
  }
}
