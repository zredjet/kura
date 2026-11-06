import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import type { JobProgress } from '../../ipc/jobs';
import type { SaveJob } from '../../stores/jobsStore';
import { progressView, saveProgressView } from './progressView';

function job(over: Partial<JobProgress>): JobProgress {
  return {
    id: 1,
    kind: 'import',
    state: 'running',
    progress: 0.3,
    message: '38 / 122 項目',
    resultJson: null,
    ...over,
  };
}

const result = (added: number, duplicates: number, failed: number) =>
  JSON.stringify({ cursor: 0, total: 0, added, duplicates, skipped: 0, failed, failures: [] });

describe('progressView', () => {
  it('実行中は進捗と一時停止・中止', () => {
    const view = progressView(job({}));
    expect(view).toMatchObject({
      title: '取り込み中',
      detail: '38 / 122 項目',
      progress: 0.3,
      actions: ['pause', 'cancel'],
      finished: false,
    });
  });

  it('待ち・一時停止', () => {
    expect(progressView(job({ state: 'queued', progress: null })).actions).toEqual(['cancel']);
    const paused = progressView(job({ state: 'paused' }));
    expect(paused.title).toBe('一時停止中');
    expect(paused.actions).toEqual(['resume', 'cancel']);
  });

  it('完了は件数と重複・失敗', () => {
    const view = progressView(job({ state: 'done', resultJson: result(1220, 6, 0) }));
    expect(view.title).toBe('1,220 項目を取り込みました');
    expect(view.detail).toBe('重複 6');
    expect(view.finished).toBe(true);
    expect(progressView(job({ state: 'done', resultJson: result(3, 0, 1) })).detail).toBe('失敗 1');
    expect(progressView(job({ state: 'done', resultJson: result(3, 0, 0) })).detail).toBeNull();
  });

  it('中身が違うので取り込まなかったもの（Aseprite でない .ase など。M11）は対象外として数える', () => {
    const rejected = JSON.stringify({
      cursor: 0,
      total: 0,
      added: 3,
      duplicates: 1,
      skipped: 2,
      failed: 0,
      failures: [],
      rejected: [
        { path: '/a/swatch.ase', reason: 'Aseprite のファイルではありません' },
        { path: '/a/b.ase', reason: 'Aseprite のファイルではありません' },
      ],
    });
    expect(progressView(job({ state: 'done', resultJson: rejected })).detail).toBe(
      '重複 1・対象外 2',
    );
  });

  it('失敗・中止', () => {
    expect(progressView(job({ state: 'failed', message: '書き込めません' }))).toMatchObject({
      title: '取り込みに失敗しました',
      detail: '書き込めません',
      actions: ['close'],
    });
    expect(progressView(job({ state: 'canceled', resultJson: result(20, 0, 0) })).detail).toBe(
      '20 項目は取り込み済みです',
    );
  });
});

describe('progressView（書き出し）', () => {
  const exported = (n: number, failed: number, manifest: string | null) =>
    JSON.stringify({ dest: '/Users/me/out', total: n + failed, exported: n, failed, manifest });

  it('実行中・完了・中止の言い方と、終わったら「Finder で表示」', () => {
    expect(progressView(job({ kind: 'export', message: '3 / 10 件' }))).toMatchObject({
      title: '書き出し中',
      detail: '3 / 10 件',
      actions: ['pause', 'cancel'],
    });
    expect(
      progressView(
        job({ kind: 'export', state: 'done', resultJson: exported(12, 1, 'kura-export.json') }),
      ),
    ).toMatchObject({
      title: '12 項目を書き出しました',
      detail: '失敗 1',
      actions: ['reveal', 'close'],
      finished: true,
    });
    expect(
      progressView(job({ kind: 'export', state: 'canceled', resultJson: exported(4, 0, null) })),
    ).toMatchObject({ detail: '4 項目は書き出し済みです', actions: ['reveal', 'close'] });
    expect(
      progressView(job({ kind: 'export', state: 'canceled', resultJson: exported(0, 0, null) }))
        .actions,
    ).toEqual(['close']);
  });
});

describe('saveProgressView（保存のジョブのカプセル）', () => {
  function save(over: Partial<SaveJob> = {}): SaveJob {
    return {
      id: 9,
      itemId: 7,
      itemTitle: 'long-60min',
      mode: 'overwrite',
      newTitle: null,
      state: 'running',
      progress: 0.42,
      message: '描いています',
      shown: true,
      ...over,
    };
  }

  it('保存中：名前・進捗・一時停止・中止', () => {
    expect(saveProgressView(save())).toEqual({
      title: '「long-60min」を保存しています',
      detail: '描いています',
      progress: 0.42,
      actions: ['pause', 'cancel'],
      finished: false,
    });
  });

  it('一時停止中：再開すると初めから書き直すことを添え、再開・中止', () => {
    const view = saveProgressView(save({ state: 'paused' }));
    expect(view).toMatchObject({
      title: '「long-60min」の保存を一時停止しています',
      detail: ja.savingJob.resumeNote,
      progress: 0.42,
      actions: ['resume', 'cancel'],
    });
    // 進捗の分からない一時停止でも棒は止まっている
    expect(saveProgressView(save({ state: 'paused', progress: null })).progress).toBe(0);
  });

  it('待っている間も止められる（棒は動き続ける）', () => {
    expect(saveProgressView(save({ state: 'queued', progress: null }))).toMatchObject({
      title: '「long-60min」の保存の順番を待っています',
      progress: null,
      actions: ['pause', 'cancel'],
    });
  });

  it('別名で保存は新しい名前、版に戻すは「戻す」の言葉', () => {
    expect(saveProgressView(save({ mode: 'saveAs', newTitle: '夕方' })).title).toBe(
      '「夕方」を保存しています',
    );
    expect(saveProgressView(save({ mode: 'revert' })).title).toBe('「long-60min」を戻しています');
    expect(saveProgressView(save({ mode: 'revert', state: 'paused' })).title).toBe(
      '「long-60min」を戻すのを一時停止しています',
    );
    expect(saveProgressView(save({ mode: 'revert', state: 'queued' })).title).toBe(
      '「long-60min」を戻す順番を待っています',
    );
  });

  it('アイテムの行がなくても出せる', () => {
    expect(saveProgressView(save({ itemTitle: null })).title).toBe(
      `「${ja.savingJob.unnamed}」を保存しています`,
    );
  });
});
