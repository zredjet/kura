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
