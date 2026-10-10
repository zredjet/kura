import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { ja } from '../i18n/ja';
import { App } from './App';

describe('App（3 ペインの骨組み）', () => {
  it('サイドバー・ツールバー・グリッド領域・インスペクタがある', () => {
    render(<App />);

    expect(screen.getByRole('navigation', { name: ja.sidebar.label })).toBeInTheDocument();
    expect(screen.getByRole('main')).toBeInTheDocument();
    expect(
      screen.getByRole('heading', { level: 1, name: ja.library.allItems }),
    ).toBeInTheDocument();
    expect(screen.getByRole('region', { name: ja.library.gridLabel })).toBeInTheDocument();
    expect(
      screen.getByRole('complementary', { name: ja.library.inspectorLabel }),
    ).toBeInTheDocument();
  });

  it('サイドバーの先頭行とツールバーがウィンドウのドラッグ領域になっている', () => {
    render(<App />);

    const nav = screen.getByRole('navigation', { name: ja.sidebar.label });
    expect(nav.firstElementChild).toHaveAttribute('data-tauri-drag-region');

    const heading = screen.getByRole('heading', { level: 1 });
    expect(heading).toHaveAttribute('data-tauri-drag-region');
    expect(heading.parentElement).toHaveAttribute('data-tauri-drag-region');
  });
});
