import { describe, expect, it } from 'vitest';

import { IMAGE_FORMATS } from './image/imageFormats';
import { canEdit, editorKindOf, formatsFor, opInputsOf } from './kindTraits';

describe('エディタの種類', () => {
  it('開ける種類だけを選ぶ（今は画像だけ）', () => {
    expect(editorKindOf({ kind: 'image' })).toBe('image');
    expect(editorKindOf({ kind: 'audio' })).toBeNull();
    expect(editorKindOf({ kind: 'video' })).toBeNull();
    expect(editorKindOf(null)).toBeNull();
    expect(canEdit({ kind: 'image' })).toBe(true);
    expect(canEdit(undefined)).toBe(false);
  });

  it('形式の表はアイテムの種類から引く（開けない種類は null）', () => {
    expect(formatsFor({ kind: 'image' })).toBe(IMAGE_FORMATS);
    expect(formatsFor({ kind: 'font' })).toBeNull();
    expect(opInputsOf('image')).toEqual(['image', 'frames']);
  });
});
