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
  it('開ける種類だけを選ぶ（画像と音声。動画・フォント・3D は開けない）', () => {
    expect(editorKindOf({ kind: 'image' })).toBe('image');
    expect(editorKindOf({ kind: 'video' })).toBeNull();
    expect(editorKindOf({ kind: 'font' })).toBeNull();
    expect(editorKindOf({ kind: 'model' })).toBeNull();
    expect(editorKindOf(null)).toBeNull();
    expect(canEdit({ kind: 'image' })).toBe(true);
    expect(canEdit({ kind: 'video' })).toBe(false);
    expect(canEdit(undefined)).toBe(false);
  });

  it('形式の表はアイテムの種類から引く（エディタのない種類は null）', () => {
    expect(formatsFor({ kind: 'image' })).toBe(IMAGE_FORMATS);
    expect(formatsFor({ kind: 'font' })).toBeNull();
    expect(opInputsOf('image')).toEqual(['image', 'frames']);
  });

  // M13 段 6 の G9：音声もエディタで開ける
  it('音声はエディタで開ける（canEdit は真）', () => {
    expect(EDITOR_KINDS).toEqual(['image', 'audio']);
    expect(OPENABLE).toEqual(['image', 'audio']);
    expect(editorKindOf({ kind: 'audio' })).toBe('audio');
    expect(canEdit({ kind: 'audio' })).toBe(true);
    expect(formatsFor({ kind: 'audio' })).toBe(AUDIO_FORMATS);
    expect(opInputsOf('audio')).toEqual(['audio']);
  });
});
