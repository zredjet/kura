import { describe, expect, it } from 'vitest';

import { AUDIO_FORMATS } from './audio/audioFormats';
import { IMAGE_FORMATS } from './image/imageFormats';
import {
  canEdit,
  EDITOR_KINDS,
  editorKindOf,
  formatsFor,
  OPENABLE,
  opInputsOf,
} from './kindTraits';

describe('エディタの種類', () => {
  it('開ける種類だけを選ぶ（今は画像だけ）', () => {
    expect(editorKindOf({ kind: 'image' })).toBe('image');
    expect(editorKindOf({ kind: 'audio' })).toBeNull();
    expect(editorKindOf({ kind: 'video' })).toBeNull();
    expect(editorKindOf(null)).toBeNull();
    expect(canEdit({ kind: 'image' })).toBe(true);
    expect(canEdit(undefined)).toBe(false);
  });

  it('形式の表はアイテムの種類から引く（エディタのない種類は null）', () => {
    expect(formatsFor({ kind: 'image' })).toBe(IMAGE_FORMATS);
    expect(formatsFor({ kind: 'font' })).toBeNull();
    expect(opInputsOf('image')).toEqual(['image', 'frames']);
  });

  // M13 段 6：音声のエディタの種類・形式・足せる操作は先に入るが、開く口は G9 まで閉じたまま
  it('音声はエディタの種類にあるが、開けるのは G9 から（canEdit は偽）', () => {
    expect(EDITOR_KINDS).toEqual(['image', 'audio']);
    expect(OPENABLE).toEqual(['image']);
    expect(editorKindOf({ kind: 'audio' })).toBeNull();
    expect(canEdit({ kind: 'audio' })).toBe(false);
    expect(formatsFor({ kind: 'audio' })).toBe(AUDIO_FORMATS);
    expect(opInputsOf('audio')).toEqual(['audio']);
  });
});
