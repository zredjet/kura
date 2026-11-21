import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { useMenuStore } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { setDragOutTarget } from '../../ipc/dragout';
import type { ProjectDto } from '../../ipc/projects';
import { useViewStore } from '../../stores/viewStore';
import { useCreditsSheetStore } from '../credits/creditsSheetStore';
import { emptyMessage } from '../library/EmptyView';
import { DragGhost } from '../library/DragGhost';
import { useDragStore } from '../library/dragState';
import { viewTitle } from '../library/viewTitle';
import { ProjectList } from '../sidebar/ProjectList';
import { usePresetSheetStore } from './presetSheetStore';
import { projectMenu } from './projectActions';
import { effectiveTarget, useProjectStore } from './projectStore';

const t = ja.projects;

function project(patch: Partial<ProjectDto>): ProjectDto {
  return {
    id: 1,
    name: 'My Game',
    root: '/Users/me/game',
    engine: 'godot',
    missing: false,
    scannedAt: 1,
    files: 3,
    items: 2,
    createdAt: 0,
    updatedAt: 0,
    ...patch,
  };
}

vi.mock('../../ipc/dragout', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/dragout')>()),
  setDragOutTarget: vi.fn(async () => null),
  dragOutTarget: vi.fn(async () => ({ chosen: 2, effective: 2 })),
}));
vi.mock('../../ipc/projects', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/projects')>()),
  listProjects: vi.fn(async () => []),
}));

const game = project({ id: 1, name: 'My Game' });
const lost = project({ id: 2, name: 'Old', missing: true, items: 0 });

beforeEach(() => {
  useProjectStore.setState({
    list: [game, lost],
    target: { chosen: 2, effective: 2 },
    renaming: null,
  });
  useViewStore.getState().setView({ kind: 'all' });
  useMenuStore.setState({ menu: null });
  usePresetSheetStore.setState({ project: null });
  useCreditsSheetStore.setState({ scope: null, name: null });
  useDragStore.setState({ payload: null, target: null, label: '' });
});

describe('ProjectList', () => {
  it('行に件数と印（見つからない・持ち出し先）を出し、押すと使っている素材の一覧を開く', () => {
    render(<ProjectList current={null} />);
    expect(screen.getByRole('img', { name: t.missing })).toBeInTheDocument();
    expect(screen.getByRole('img', { name: t.target })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /My Game/ }));
    expect(useViewStore.getState().view).toEqual({ kind: 'project', id: 1 });
  });

  it('右クリックで持ち出し先・設定・クレジットのメニューを開く', () => {
    render(<ProjectList current={null} />);
    fireEvent.contextMenu(screen.getByRole('button', { name: /My Game/ }));
    const menu = useMenuStore.getState().menu;
    expect(menu?.label).toBe(t.label('My Game'));
  });
});

describe('projectMenu', () => {
  it('選んだ持ち出し先は外す項目（チェック付き）、設定とクレジットのシートを開く', async () => {
    const entries = projectMenu(lost, () => undefined);
    const first = entries[0];
    expect(first).toMatchObject({ label: t.unsetTarget, checked: true });
    const reveal = entries.find((e) => e !== 'separator' && e.label === t.reveal);
    expect(reveal).toMatchObject({ disabled: true });
    const pick = (label: string) => {
      const e = projectMenu(game, () => undefined).find(
        (x) => x !== 'separator' && x.label === label,
      );
      if (!e || e === 'separator' || !('onSelect' in e)) throw new Error(label);
      e.onSelect();
    };
    pick(t.settings);
    expect(usePresetSheetStore.getState().project).toBe(1);
    pick(t.credits);
    expect(useCreditsSheetStore.getState()).toMatchObject({
      scope: { type: 'project', id: 1 },
      name: 'My Game',
    });
    pick(t.setTarget);
    await waitFor(() => expect(setDragOutTarget).toHaveBeenCalledWith(1));
  });
});

describe('見出し・空の案内・持ち出し先', () => {
  it('プロジェクトの一覧の見出しと空の案内、名前と形式に使う持ち出し先', () => {
    expect(viewTitle({ kind: 'project', id: 1 }, [], [], [game])).toBe('My Game');
    expect(viewTitle({ kind: 'project', id: 9 }, [], [], [game])).toBe(ja.library.projectTitle);
    expect(emptyMessage({ kind: 'project', id: 1 }, false).title).toBe(
      ja.library.emptyView.project,
    );
    expect(effectiveTarget([game, lost], { chosen: null, effective: 1 })).toBe(game);
    expect(effectiveTarget([game], { chosen: null, effective: null })).toBeNull();
  });
});

describe('DragGhost', () => {
  it('落とし先のない所でタイルを動かしているときだけ、持ち出し先を添える', () => {
    useDragStore.setState({
      payload: { kind: 'entries', ids: [1] },
      label: '1 項目',
      target: null,
    });
    const view = render(<DragGhost />);
    expect(screen.getByText(ja.dragOut.hint('Old'))).toBeInTheDocument();
    useDragStore.setState({ target: { kind: 'playlist', id: 3 } });
    view.rerender(<DragGhost />);
    expect(screen.queryByText(ja.dragOut.hint('Old'))).toBeNull();
    useProjectStore.setState({ target: { chosen: null, effective: null } });
    useDragStore.setState({ target: null });
    view.rerender(<DragGhost />);
    expect(screen.getByText(ja.dragOut.hintPlain)).toBeInTheDocument();
    useDragStore.setState({ payload: { kind: 'folder', id: 1 } });
    view.rerender(<DragGhost />);
    expect(screen.queryByText(ja.dragOut.hintPlain)).toBeNull();
  });
});
