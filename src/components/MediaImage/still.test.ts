import { describe, expect, it } from 'vitest';

import { summary } from '../../features/asset/testAssets';
import { stillOriginal } from './still';

const URL = 'kura://localhost/play/x';

// design.md §3.1「アニメのプレビュー」：帯を待つ間・作れないときは先頭のコマ。<img> に元の GIF・APNG・アニメの WebP を
// 置くと WebKit が自分で回す（prefers-reduced-motion も見ない）
describe('stillOriginal（アニメの画像は元のファイルを <img> に置かない）', () => {
  it('アニメの画像は、サムネイル（先頭のコマ）があれば元のファイルを出さない', () => {
    for (const ext of ['gif', 'png', 'webp', 'aseprite']) {
      expect(stillOriginal(summary({ ext, animated: true }), URL)).toBeUndefined();
    }
    // 作り直し中（stale）も今の絵を出す
    expect(
      stillOriginal(summary({ ext: 'gif', animated: true, thumbState: 'stale' }), URL),
    ).toBeUndefined();
  });

  it('動かない画像・サムネイルがまだないものは元のファイル', () => {
    expect(stillOriginal(summary({ ext: 'gif', animated: false }), URL)).toBe(URL);
    expect(stillOriginal(summary({ ext: 'gif', animated: true, thumbState: 'pending' }), URL)).toBe(
      URL,
    );
    expect(stillOriginal(summary({ ext: 'gif', animated: true, thumbState: 'failed' }), URL)).toBe(
      URL,
    );
  });

  it('解析で分かった素材の欄の値でもアニメとみなす（一覧の概要が古いとき）', () => {
    expect(stillOriginal(summary({ ext: 'gif', animated: false }), URL, true)).toBeUndefined();
  });
});
