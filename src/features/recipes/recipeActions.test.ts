import { describe, expect, it, vi } from 'vitest';

import { kindWord } from '../../ipc/kinds';
import { fileStem, sampleQuery } from './recipeActions';

vi.mock('../../ipc/recipes', () => ({}));

describe('recipeActions', () => {
  it('見本の候補は種類で絞って探す（種類の語は kinds.ts から）', () => {
    expect(sampleQuery('audio', '')).toBe(`種類:${kindWord('audio')}`);
    expect(sampleQuery('image', ' hero ')).toBe(`種類:${kindWord('image')} hero`);
  });

  it('書き出しの既定の名前はファイル名に使えない文字を除く', () => {
    expect(fileStem('a/b:c')).toBe('a-b-c');
    expect(fileStem('  ')).toBe('recipe');
  });
});
