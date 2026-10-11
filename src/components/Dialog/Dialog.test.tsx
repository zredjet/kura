import { fireEvent, render, screen } from '@testing-library/react';
import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';

import { Dialog } from './Dialog';

function Nested({ onOuterClose }: { onOuterClose: () => void }) {
  const [inner, setInner] = useState(true);
  return (
    <Dialog labelledBy="outer" onClose={onOuterClose}>
      <h2 id="outer">外のシート</h2>
      {inner && (
        <Dialog labelledBy="inner" onClose={() => setInner(false)}>
          <h2 id="inner">中のシート</h2>
        </Dialog>
      )}
    </Dialog>
  );
}

describe('Dialog', () => {
  it('シートを重ねたとき、esc はいちばん上のシートだけを閉じる', () => {
    const onOuterClose = vi.fn();
    render(<Nested onOuterClose={onOuterClose} />);
    expect(screen.getByRole('dialog', { name: '中のシート' })).toBeInTheDocument();

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog', { name: '中のシート' })).toBeNull();
    expect(onOuterClose).not.toHaveBeenCalled();

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onOuterClose).toHaveBeenCalledOnce();
  });
});
