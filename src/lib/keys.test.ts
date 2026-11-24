import { describe, expect, it } from 'vitest';

import { controlOwnsKey, isKeyControl, isReplayed, isTextEntry, markReplayed } from './keys';

function input(type: string): HTMLInputElement {
  const el = document.createElement('input');
  el.type = type;
  return el;
}

describe('キーの受け方の判定', () => {
  it('文字を打つ所は文字の入力欄・テキストエリア・contenteditable だけ', () => {
    for (const type of ['text', 'search', 'number', 'email', 'url', 'password']) {
      expect(isTextEntry(input(type))).toBe(true);
    }
    expect(isTextEntry(document.createElement('textarea'))).toBe(true);
    for (const type of ['checkbox', 'range', 'radio', 'button', 'color']) {
      expect(isTextEntry(input(type))).toBe(false);
    }
    expect(isTextEntry(document.createElement('select'))).toBe(false);
    expect(isTextEntry(document.createElement('button'))).toBe(false);
    expect(isTextEntry(null)).toBe(false);
  });

  it('文字は打たないがキーを使う部品は、選択・スライダー・スイッチなど', () => {
    expect(isKeyControl(document.createElement('select'))).toBe(true);
    expect(isKeyControl(input('range'))).toBe(true);
    expect(isKeyControl(input('checkbox'))).toBe(true);
    expect(isKeyControl(input('text'))).toBe(false);
    expect(isKeyControl(document.createElement('button'))).toBe(false);
  });

  it('部品が自分で使うキー：スライダーは位置を動かすキーだけ', () => {
    const range = input('range');
    for (const key of [
      'ArrowLeft',
      'ArrowRight',
      'ArrowUp',
      'ArrowDown',
      'Home',
      'End',
      'PageUp',
    ]) {
      expect(controlOwnsKey(range, key)).toBe(true);
    }
    for (const key of [' ', 'Escape', 'f', ',', 'j']) {
      expect(controlOwnsKey(range, key)).toBe(false);
    }
    expect(controlOwnsKey(document.createElement('select'), 'f')).toBe(true);
    expect(controlOwnsKey(input('checkbox'), ' ')).toBe(true);
    expect(controlOwnsKey(input('checkbox'), 'ArrowRight')).toBe(false);
    expect(controlOwnsKey(input('radio'), 'ArrowDown')).toBe(true);
    expect(controlOwnsKey(input('button'), 'Enter')).toBe(true);
    expect(controlOwnsKey(input('text'), 'ArrowLeft')).toBe(false);
    expect(controlOwnsKey(document.createElement('button'), ' ')).toBe(false);
    expect(controlOwnsKey(null, ' ')).toBe(false);
  });

  it('メニューが送ったキーの印', () => {
    const e = new KeyboardEvent('keydown', { key: ' ' });
    expect(isReplayed(e)).toBe(false);
    markReplayed(e);
    expect(isReplayed(e)).toBe(true);
  });
});
