import { fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { describe, expect, it } from 'vitest';

import { TagInput } from './TagInput';

function Harness({ initial = [] as string[] }) {
  const [tags, setTags] = useState<string[]>(initial);
  return (
    <>
      <TagInput
        tags={tags}
        onChange={setTags}
        placeholder="タグを追加…"
        inputLabel="タグを追加"
        removeLabel={(t) => `タグ「${t}」を外す`}
      />
      <output>{tags.join('|')}</output>
    </>
  );
}

const value = () => screen.getByRole('status').textContent;

describe('TagInput', () => {
  it('Enter と読点で確定し、重複と空は足さない', () => {
    render(<Harness />);
    const input = screen.getByRole('textbox', { name: 'タグを追加' });
    fireEvent.change(input, { target: { value: '旅行' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    fireEvent.change(input, { target: { value: '作者:もりの、' } });
    fireEvent.change(input, { target: { value: '旅行' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    fireEvent.change(input, { target: { value: '  ' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(value()).toBe('旅行|作者:もりの');
  });

  it('変換中の Enter では確定しない', () => {
    render(<Harness />);
    const input = screen.getByRole('textbox', { name: 'タグを追加' });
    fireEvent.change(input, { target: { value: 'かまくら' } });
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
    expect(value()).toBe('');
    expect(input).toHaveValue('かまくら');
  });

  it('× と空のときの Backspace で外す', () => {
    render(<Harness initial={['旅行', '2026', '海']} />);
    fireEvent.click(screen.getByRole('button', { name: 'タグ「2026」を外す' }));
    expect(value()).toBe('旅行|海');
    fireEvent.keyDown(screen.getByRole('textbox'), { key: 'Backspace' });
    expect(value()).toBe('旅行');
  });
});
