import { fireEvent, render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { END_GUARD_MS } from '../../lib/seek';
import { TimeJump } from './TimeJump';

function setup(durationMs: number | null = 120_000) {
  const onJump = vi.fn();
  render(<TimeJump positionMs={83_449} durationMs={durationMs} onJump={onJump} />);
  return { onJump };
}

function open(): HTMLInputElement {
  fireEvent.click(screen.getByRole('button', { name: ja.seek.jump('1:23') }));
  return screen.getByLabelText(ja.seek.field);
}

describe('TimeJump', () => {
  it('ふだんは今の時刻のボタン、押すと 0.1 秒までの欄が全選択で開く', () => {
    setup();
    expect(screen.getByRole('button')).toHaveTextContent('1:23');
    const input = open();
    expect(input).toHaveValue('1:23.4');
    expect(input).toHaveFocus();
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe('1:23.4'.length);
  });

  it('Enter で飛び、ボタンに戻る', () => {
    const { onJump } = setup();
    const input = open();
    fireEvent.change(input, { target: { value: '0:07.5' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onJump).toHaveBeenCalledWith(7_500);
    expect(screen.getByRole('button')).toHaveFocus();
  });

  it('全角・秒だけでも読み、長さを超えたら終わりの手前', () => {
    const { onJump } = setup();
    let input = open();
    fireEvent.change(input, { target: { value: '９０．５' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onJump).toHaveBeenLastCalledWith(90_500);
    input = open();
    fireEvent.change(input, { target: { value: '9:00' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onJump).toHaveBeenLastCalledWith(120_000 - END_GUARD_MS);
  });

  it('読めなければ開いたまま aria-invalid', () => {
    const { onJump } = setup();
    const input = open();
    fireEvent.change(input, { target: { value: '1:99' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(onJump).not.toHaveBeenCalled();
    expect(input).toHaveAttribute('aria-invalid', 'true');
    expect(input).toHaveAttribute('title', ja.seek.badTime);
    fireEvent.change(input, { target: { value: '1:09' } });
    expect(input).not.toHaveAttribute('aria-invalid');
  });

  it('esc は取り消し、外へ伝えない', () => {
    const { onJump } = setup();
    const outer = vi.fn();
    window.addEventListener('keydown', outer);
    const input = open();
    fireEvent.change(input, { target: { value: '0:30' } });
    fireEvent.keyDown(input, { key: 'Escape' });
    window.removeEventListener('keydown', outer);
    expect(onJump).not.toHaveBeenCalled();
    expect(outer).not.toHaveBeenCalled();
    expect(screen.getByRole('button')).toHaveFocus();
  });

  it('欄を離れたら飛ばずに閉じる', () => {
    const { onJump } = setup();
    const input = open();
    fireEvent.change(input, { target: { value: '0:30' } });
    fireEvent.blur(input);
    expect(onJump).not.toHaveBeenCalled();
    expect(screen.getByRole('button')).toBeInTheDocument();
  });

  it('日本語入力の確定の Enter は読まない', () => {
    const { onJump } = setup();
    const input = open();
    fireEvent.change(input, { target: { value: '0:30' } });
    fireEvent.keyDown(input, { key: 'Enter', keyCode: 229 });
    expect(onJump).not.toHaveBeenCalled();
    expect(input).toBeInTheDocument();
  });

  it('長さがなければ押せない', () => {
    render(<TimeJump positionMs={0} durationMs={null} onJump={vi.fn()} disabled />);
    expect(screen.getByRole('button')).toBeDisabled();
  });
});
