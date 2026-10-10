import { describe, expect, it } from 'vitest';

import { IpcError, isIpcError, unwrap } from './result';

describe('unwrap', () => {
  it('成功ならデータを返す', async () => {
    await expect(unwrap(Promise.resolve({ status: 'ok', data: 42 }))).resolves.toBe(42);
  });

  it('失敗なら code と message を持つ IpcError を投げる', async () => {
    const result = unwrap(
      Promise.resolve({
        status: 'error',
        error: { code: 'NotFound', message: '見つかりません: item 42' },
      }),
    );

    await expect(result).rejects.toBeInstanceOf(IpcError);
    await expect(result).rejects.toMatchObject({
      code: 'NotFound',
      message: '見つかりません: item 42',
    });
  });
});

describe('isIpcError', () => {
  const error = new IpcError({ code: 'Conflict', message: '競合しています' });

  it('code を指定しなければ IpcError かどうかだけを見る', () => {
    expect(isIpcError(error)).toBe(true);
    expect(isIpcError(new Error('other'))).toBe(false);
  });

  it('code を指定したら種類も一致するかを見る', () => {
    expect(isIpcError(error, 'Conflict')).toBe(true);
    expect(isIpcError(error, 'NotFound')).toBe(false);
  });
});
