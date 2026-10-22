import { describe, expect, it } from 'vitest';

import { ja } from '../../i18n/ja';
import { IpcError } from '../../ipc/result';
import { openFailure, openFailureOf } from './openFailure';

const t = ja.libraryOpen.failure;

describe('ライブラリを開けなかった理由（M11：新しい版にできないとき）', () => {
  it.each([
    ['LibraryTooNew', t.LibraryTooNew],
    ['NoSpace', t.NoSpace],
    ['BackupFailed', t.BackupFailed],
    ['UpgradeFailed', t.UpgradeFailed],
  ] as const)('%s は専用の見出しと、Rust の説明', (code, title) => {
    const message = `説明（${code}）。ライブラリは変えていません`;
    expect(openFailure({ code, message })).toEqual({ title, detail: message });
  });

  it('見出しは理由ごとに違い、どれも「開けませんでした」の汎用の見出しとは別', () => {
    const titles = [t.LibraryTooNew, t.NoSpace, t.BackupFailed, t.UpgradeFailed];
    expect(new Set(titles).size).toBe(4);
    expect(titles).not.toContain(t.other);
  });

  // 版 0（新しいライブラリを作る途中）で止まったときの説明も、そのまま出す（M11 の C3。初回の画面で作るのに失敗したとき・
  // サイドバーや設定から作るのに失敗して初回の画面に戻ったときのカード。2 回目の確認 P3）
  it.each([
    [
      'UpgradeFailed',
      t.UpgradeFailed,
      'ライブラリを新しい版にできませんでした（新しいライブラリを作る途中で止まりました。開き直すと続きを当てます。0009_analysis：disk I/O error）',
    ],
    [
      'NoSpace',
      t.NoSpace,
      'ディスクの空き容量が足りず、ライブラリを新しい版にできませんでした。空きを作ってから開き直してください（新しいライブラリを作る途中で止まりました。開き直すと続きを当てます。0008_jobs_named_checks：database or disk is full）',
    ],
  ] as const)('新しいライブラリを作る途中で止まった %s', (code, title, message) => {
    expect(openFailure({ code, message })).toEqual({ title, detail: message });
  });

  it('ほかの理由は汎用の見出し', () => {
    expect(openFailure({ code: 'Db', message: 'database is locked' })).toEqual({
      title: t.other,
      detail: 'database is locked',
    });
  });

  it('例外から読む（IpcError でないものは文字にする）', () => {
    expect(openFailureOf(new IpcError({ code: 'NoSpace', message: 'あと約 120 MB' }))).toEqual({
      title: t.NoSpace,
      detail: 'あと約 120 MB',
    });
    expect(openFailureOf(new Error('壊れています'))).toEqual({
      title: t.other,
      detail: '壊れています',
    });
  });
});
