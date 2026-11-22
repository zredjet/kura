import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import {
  discardEditDraft,
  type EditImageView,
  type EditOpenedDto,
  type EditStatusDto,
  type EditStepResultDto,
  openEdit,
  openEditRecipe,
  viewEdit,
} from '../../ipc/edit';
import type { ItemSummaryDto } from '../../ipc/items';
import { createRecipe, replaceRecipe } from '../../ipc/recipes';
import { SaveRecipeSheet } from '../recipes/SaveRecipeSheet';
import { openEditor } from './editorKinds';
import { openRevert, openSaveAs, recipeSaved, requestSave } from './editorSave';
import { canSaveEditor, closeEditor, hasUnsavedChanges, useEditorStore } from './editorStore';
import { opName } from './opsCatalog';

vi.mock('../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/edit')>()),
  openEdit: vi.fn(),
  openEditRecipe: vi.fn(),
  stepEdit: vi.fn(),
  viewEdit: vi.fn(),
  closeEdit: vi.fn(async () => null),
  discardEditDraft: vi.fn(async () => true),
  listEditFormats: vi.fn(async () => []),
}));
vi.mock('../../ipc/recipes', () => ({
  createRecipe: vi.fn(async () => 11),
  replaceRecipe: vi.fn(async () => null),
  listRecipes: vi.fn(async () => []),
}));

const item = {
  id: 7,
  hash: 'ab'.repeat(32),
  ext: 'png',
  kind: 'image',
  width: 64,
  height: 32,
  durationMs: null,
  sizeBytes: 100,
  title: 'hero',
  rating: 0,
  sourceSite: null,
  importedAt: 0,
  capturedAt: null,
  thumbState: 'ready',
  proxyState: 'none',
  lastPlayedAt: null,
  thumbRev: 0,
  analysisState: 'ready',
  alpha: 'binary',
  pixelArt: true,
  pixelArtSetting: null,
  animated: false,
  frameCount: null,
  hasLoop: false,
} as ItemSummaryDto;

function status(over: Partial<EditStatusDto> = {}): EditStatusDto {
  return {
    rev: 3,
    head: 3,
    provisional: false,
    canUndo: false,
    canRedo: false,
    dirty: true,
    saving: null,
    needsReopen: false,
    ...over,
  };
}

const view: EditImageView = {
  type: 'image',
  rev: 3,
  level: 0,
  fallback: null,
  fullWidth: 64,
  fullHeight: 32,
  width: 64,
  height: 32,
  frames: [{ index: 0, x: 0, y: 0, w: 64, h: 32, durationMs: null }],
  format: 'rgba8',
  tile: 256,
  changed: { type: 'all' },
  metaChanged: true,
  warnings: [],
  drawnNodes: 0,
  ms: 1,
};

function result(): EditStepResultDto {
  return {
    status: status(),
    created: [],
    rejected: null,
    doc: {
      base: null,
      order: [2, 3, 4],
      nodes: [
        { id: 2, op: 'image.flip', v: 1, enabled: true, params: { axis: 'horizontal' }, log: null },
        {
          id: 3,
          op: 'paint.actions',
          v: 1,
          enabled: true,
          params: {},
          log: { count: 2, bytes: 10, digest: 'x' },
        },
        {
          id: 4,
          op: 'meta.set_grid',
          v: 1,
          enabled: true,
          params: { grid: { cellW: 8, cellH: 8, gapX: 0, gapY: 0, offsetX: 0, offsetY: 0 } },
          log: null,
        },
      ],
      outputs: [{ id: 0, from: { node: 4, port: 0 }, format: { type: 'keep' } }],
    },
    view: { type: 'shown', view },
    notices: [],
  } as unknown as EditStepResultDto;
}

function opened(recipe: EditOpenedDto['recipe']): EditOpenedDto {
  return {
    session: 'aa01',
    itemId: 7,
    baseVersion: 0,
    source: { kind: 'image', width: 64, height: 32, frames: null, format: 'png', audio: null },
    originalFormat: 'png',
    pixelArt: true,
    draft: { exists: false, fresh: true, updatedAt: null, opened: false },
    result: result(),
    recipe,
  } as EditOpenedDto;
}

const RECIPE = {
  id: 9,
  name: '裏返して格子',
  description: '',
  bindings: [{ node: 4, param: 'grid', from: { type: 'setting' as const, key: 'grid' as const } }],
  withFormat: false,
};

const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  vi.clearAllMocks();
  // 表示だけの要求は、今の状態のまま（文書は変わらない）
  vi.mocked(viewEdit).mockImplementation(
    async () =>
      ({
        ...result(),
        status: useEditorStore.getState().status ?? status(),
        doc: { base: 1, order: useEditorStore.getState().doc.order, nodes: [], outputs: null },
      }) as unknown as EditStepResultDto,
  );
});

afterEach(() => {
  void closeEditor();
});

describe('レシピの見本のモード（M14 段 7）', () => {
  it('見本で開く口を呼び、レシピを持つ。下書きのシートは出さず、レシピに保存した頭からの変化を未保存と数える', async () => {
    vi.mocked(openEditRecipe).mockResolvedValue({
      ...opened(RECIPE),
      draft: { exists: true, fresh: true, updatedAt: 1, opened: false },
    });
    await openEditor(item, { recipe: 9 });
    expect(openEditRecipe).toHaveBeenCalledWith(7, expect.anything(), 9);
    expect(openEdit).not.toHaveBeenCalled();
    const s = useEditorStore.getState();
    expect(s.recipe).toMatchObject({ id: 9, name: '裏返して格子', savedHead: 3 });
    expect(s.sheet).toBeNull();
    // Rust の dirty（アイテムの保存に対して）ではなく、レシピに保存した頭と比べる
    expect(hasUnsavedChanges(s)).toBe(false);
    expect(hasUnsavedChanges({ ...s, status: status({ head: 4 }) })).toBe(true);
    // アイテムには保存しない
    expect(canSaveEditor(s)).toBe(false);
  });

  it('⌘S・保存はレシピを保存のシート。別名で保存・元に戻すは開かない。閉じても見本の下書きを捨てない', async () => {
    vi.mocked(openEditRecipe).mockResolvedValue(opened(RECIPE));
    await openEditor(item, { recipe: 9 });
    await flush();
    openSaveAs();
    openRevert();
    expect(useEditorStore.getState().sheet).toBeNull();
    requestSave();
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'saveRecipe', closeAfter: null });
    useEditorStore.setState({ status: status({ head: 5 }) });
    const bindings = [
      {
        node: 4,
        param: 'grid',
        part: null,
        from: { type: 'setting' as const, key: 'pivot' as const },
      },
    ];
    recipeSaved(null, { bindings, withFormat: true, format: 'png' });
    expect(useEditorStore.getState().sheet).toBeNull();
    // 保存した結び付け・形式の選びを覚える（次のシートはここから始める）
    expect(useEditorStore.getState().recipe).toMatchObject({
      savedHead: 5,
      bindings,
      withFormat: true,
      savedFormat: 'png',
    });
    await closeEditor({ discardDraft: true });
    expect(discardEditDraft).not.toHaveBeenCalled();
  });

  it('ふだんの開き方ではレシピを持たない', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened(null));
    await openEditor(item);
    expect(useEditorStore.getState().recipe).toBeNull();
  });
});

describe('レシピとして保存のシート', () => {
  it('描画は初めから外す。名前の既定は含める操作。作ると含める操作と結び付けを渡す', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened(null));
    await openEditor(item);
    await flush();
    useEditorStore.setState({
      sheet: { kind: 'saveRecipe', closeAfter: null },
      pendingSteps: false,
    });
    render(<SaveRecipeSheet closeAfter={null} />);
    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.map((b) => (b as HTMLInputElement).checked)).toEqual([true, false, true]);
    const flipName = opName('image.flip');
    const gridName = opName('meta.set_grid');
    expect(screen.getByRole('textbox', { name: ja.saveRecipe.name })).toHaveValue(
      `${flipName} → ${gridName}`,
    );
    // 格子を素材の格子に結ぶ
    const select = screen.getByRole('combobox');
    fireEvent.change(select, { target: { value: 'setting:grid' } });
    fireEvent.click(screen.getByRole('button', { name: ja.saveRecipe.save }));
    await waitFor(() => expect(createRecipe).toHaveBeenCalled());
    expect(createRecipe).toHaveBeenCalledWith(`${flipName} → ${gridName}`, '', {
      session: 'aa01',
      keep: [2, 4],
      withFormat: false,
      bindings: [{ node: 4, param: 'grid', part: null, from: { type: 'setting', key: 'grid' } }],
    });
    expect(useEditorStore.getState().sheet).toBeNull();
  });

  it('見本で編集では名前を聞かず、全部の手を含めて、そのレシピを差し替える（今の結び付けから始める）', async () => {
    vi.mocked(openEditRecipe).mockResolvedValue(opened(RECIPE));
    await openEditor(item, { recipe: 9 });
    await flush();
    useEditorStore.setState({
      sheet: { kind: 'saveRecipe', closeAfter: null },
      pendingSteps: false,
    });
    render(<SaveRecipeSheet closeAfter={null} />);
    expect(screen.queryByRole('textbox', { name: ja.saveRecipe.name })).toBeNull();
    expect(screen.getByRole('combobox')).toHaveValue('setting:grid');
    fireEvent.click(screen.getByRole('button', { name: ja.saveRecipe.save }));
    await waitFor(() => expect(replaceRecipe).toHaveBeenCalled());
    expect(replaceRecipe).toHaveBeenCalledWith(9, {
      session: 'aa01',
      keep: [2, 3, 4],
      withFormat: false,
      bindings: [{ node: 4, param: 'grid', part: null, from: { type: 'setting', key: 'grid' } }],
    });
    // 保存したら、次のシートは保存した結び付けから始める（2 回目の保存で 1 回目の選びが戻らない）
    expect(useEditorStore.getState().recipe?.bindings).toEqual([
      { node: 4, param: 'grid', part: null, from: { type: 'setting', key: 'grid' } },
    ]);
  });

  it('シートを開いたあとに応えの届いた手も、既定どおり含める', async () => {
    vi.mocked(openEditRecipe).mockResolvedValue(opened(RECIPE));
    await openEditor(item, { recipe: 9 });
    await flush();
    useEditorStore.setState({
      sheet: { kind: 'saveRecipe', closeAfter: null },
      pendingSteps: true,
    });
    const { rerender } = render(<SaveRecipeSheet closeAfter={null} />);
    expect(screen.getByRole('button', { name: ja.saveRecipe.save })).toBeDisabled();
    // 応えが届いて手が 1 つ増えた
    const doc = useEditorStore.getState().doc;
    useEditorStore.setState({
      pendingSteps: false,
      doc: {
        ...doc,
        order: [...doc.order, 5],
        nodes: {
          ...doc.nodes,
          5: {
            id: 5,
            op: 'image.flip',
            v: 1,
            enabled: true,
            params: { axis: 'vertical' },
            log: null,
          },
        },
      },
    });
    rerender(<SaveRecipeSheet closeAfter={null} />);
    const boxes = screen.getAllByRole('checkbox');
    expect(boxes.map((b) => (b as HTMLInputElement).checked)).toEqual([true, true, true, true]);
    fireEvent.click(screen.getByRole('button', { name: ja.saveRecipe.save }));
    await waitFor(() => expect(replaceRecipe).toHaveBeenCalled());
    expect(vi.mocked(replaceRecipe).mock.calls[0]?.[1].keep).toEqual([2, 3, 4, 5]);
  });
});
