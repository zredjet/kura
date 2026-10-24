import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { type AssetDto, getAsset } from '../../ipc/asset';
import {
  closeEdit,
  discardEditDraft,
  type EditStatusDto,
  type EditStepResultDto,
  type EditVersionDto,
  type EditViewDto,
  listEditVersions,
  openEdit,
  saveEdit,
  stepEdit,
  viewEdit,
} from '../../ipc/edit';
import { getItems, type ItemSummaryDto } from '../../ipc/items';
import { type JobDto, listJobs } from '../../ipc/jobs';
import { IpcError } from '../../ipc/result';
import { invalidateItems } from '../../stores/itemCache';
import { useNoticeStore } from '../../stores/noticeStore';
import {
  answerUnsaved,
  confirmWarnings,
  onEditorJob,
  openSaveAs,
  requestCloseEditor,
  requestSave,
  SLOW_SAVE_MS,
  submitSaveAs,
  watchWindowClose,
} from './editorSave';
import { EditorSheets } from './EditorSheets';
import { closeEditor, openEditor, pushEditorSteps, useEditorStore } from './editorStore';
import { DEFAULT_TOOLS, useToolStore } from './toolStore';

vi.mock('@tauri-apps/api/core', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@tauri-apps/api/core')>()),
  isTauri: vi.fn(() => false),
}));

vi.mock('@tauri-apps/api/window', () => ({ getCurrentWindow: vi.fn() }));

vi.mock('../../ipc/asset', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/asset')>()),
  getAsset: vi.fn(),
}));

vi.mock('../../ipc/edit', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/edit')>()),
  openEdit: vi.fn(),
  stepEdit: vi.fn(),
  viewEdit: vi.fn(),
  sampleEdit: vi.fn(),
  closeEdit: vi.fn(async () => null),
  saveEdit: vi.fn(),
  listEditVersions: vi.fn(async () => []),
  discardEditDraft: vi.fn(async () => true),
}));

vi.mock('../../ipc/items', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/items')>()),
  getItems: vi.fn(async () => []),
  getItemDetail: vi.fn(async () => ({ originalName: 'hero.png' })),
}));

vi.mock('../../ipc/jobs', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../ipc/jobs')>()),
  listJobs: vi.fn(async () => []),
}));

vi.mock('../../stores/libraryStore', () => ({
  useLibraryStore: { getState: () => ({ loadList: vi.fn(), refresh: vi.fn() }) },
}));

vi.mock('../../stores/itemCache', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../stores/itemCache')>()),
  invalidateItems: vi.fn(),
}));

const t = ja.editor;

function item(over: Partial<ItemSummaryDto> = {}): ItemSummaryDto {
  return {
    id: 7,
    hash: 'ab'.repeat(32),
    ext: 'png',
    kind: 'image',
    width: 64,
    height: 32,
    title: 'hero',
    thumbState: 'ready',
    thumbRev: 0,
    alpha: 'binary',
    pixelArt: true,
    ...over,
  } as ItemSummaryDto;
}

function status(over: Partial<EditStatusDto> = {}): EditStatusDto {
  return {
    rev: 2,
    head: 2,
    provisional: false,
    canUndo: true,
    canRedo: false,
    dirty: true,
    saving: null,
    needsReopen: false,
    ...over,
  };
}

function viewDto(over: Partial<EditViewDto> = {}): EditViewDto {
  return {
    rev: 2,
    level: 0,
    fallback: null,
    fullWidth: 64,
    fullHeight: 32,
    width: 64,
    height: 32,
    frames: [{ index: 0, x: 0, y: 0, w: 64, h: 32, durationMs: null }],
    format: 'rgba8',
    tile: 256,
    changed: { type: 'tiles', frames: [] },
    metaChanged: false,
    warnings: [],
    drawnNodes: 0,
    ms: 1,
    ...over,
  };
}

function result(over: Partial<EditStepResultDto> = {}): EditStepResultDto {
  return {
    status: status(),
    created: [],
    rejected: null,
    doc: {
      base: null,
      order: [],
      nodes: [],
      outputs: [
        {
          id: 0,
          from: { from: 'input', id: 0 },
          format: { type: 'keep' },
          expand: { type: 'one' },
        },
      ],
    },
    view: { type: 'shown', view: viewDto({ changed: { type: 'all' } }) },
    notices: [],
    ...over,
  } as EditStepResultDto;
}

type Draft = { exists: boolean; fresh: boolean; updatedAt: number | null; opened: boolean };

function opened(draft: Partial<Draft> = {}, over: Partial<EditStepResultDto> = {}) {
  return {
    session: 'aa01',
    itemId: 7,
    baseVersion: 0,
    source: { kind: 'image', width: 64, height: 32, frames: null, format: 'png' },
    pixelArt: true,
    draft: { exists: false, fresh: true, updatedAt: null, opened: false, ...draft },
    result: result(over),
  };
}

function job(over: Partial<JobDto> = {}): JobDto {
  return {
    id: 9,
    kind: 'edit',
    state: 'failed',
    progress: null,
    message: null,
    errorCode: null,
    resultJson: null,
    createdAt: 0,
    startedAt: null,
    finishedAt: null,
    ...over,
  };
}

const flush = () => act(() => new Promise((r) => setTimeout(r, 0)));

/** 保存を積んだ結果（ジョブ 9、頭 2 を保存している）。 */
function started() {
  return {
    job: 9,
    status: status({ saving: 9 }),
    doc: { base: 2, order: [], nodes: [], outputs: null },
  };
}

/** 保存のジョブのイベント。 */
function event(
  state: JobDto['state'],
  over: Partial<{
    progress: number | null;
    resultJson: string | null;
    message: string | null;
  }> = {},
) {
  return {
    id: 9,
    kind: 'edit' as const,
    state,
    progress: null,
    message: null,
    resultJson: null,
    ...over,
  };
}

/** Rust が保存の終わりを見たあとの取り直し（保存の印が付き、知らせ「保存しました」）。 */
function savedRefresh(notices = ['保存しました']) {
  vi.mocked(viewEdit).mockResolvedValue(
    result({
      status: status({ dirty: false, saving: null }),
      doc: { base: 2, order: [], nodes: [], outputs: null },
      view: { type: 'shown', view: viewDto() },
      notices,
    }),
  );
}

async function openDirty(draft: Partial<Draft> = {}) {
  vi.mocked(openEdit).mockResolvedValue(opened(draft));
  await act(() => openEditor(item()));
  await flush();
  vi.mocked(viewEdit).mockClear();
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(viewEdit).mockImplementation(async () =>
    result({
      status: useEditorStore.getState().status ?? status(),
      doc: { base: 2, order: [], nodes: [], outputs: null },
      view: { type: 'shown', view: viewDto() },
    }),
  );
  vi.mocked(stepEdit).mockImplementation(async () => result());
  vi.mocked(saveEdit).mockResolvedValue(started() as Awaited<ReturnType<typeof saveEdit>>);
  vi.mocked(closeEdit).mockImplementation(async () => null);
  vi.mocked(discardEditDraft).mockImplementation(async () => true);
  useToolStore.setState({ ...DEFAULT_TOOLS });
  useNoticeStore.setState({ message: null, action: null });
});

afterEach(async () => {
  vi.useRealTimers();
  await act(() => closeEditor());
});

describe('保存（上書き）', () => {
  it('保存のジョブを積み、進捗を持ち、終わったら取り直して保存の印（Rust の「保存しました」）', async () => {
    await openDirty();
    requestSave();
    await flush();
    expect(saveEdit).toHaveBeenCalledWith('aa01', { type: 'overwrite' }, null);
    expect(useEditorStore.getState().save).toMatchObject({ job: 9, mode: 'overwrite', rev: 2 });
    // 保存している頭のままなら、閉じても保存は続くので「未保存」ではない
    act(() => onEditorJob(event('running', { progress: 0.5 })));
    expect(useEditorStore.getState().save?.progress).toBe(0.5);

    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(viewEdit).toHaveBeenCalled();
    expect(useEditorStore.getState().save).toBeNull();
    expect(useEditorStore.getState().status?.dirty).toBe(false);
    expect(useNoticeStore.getState().message).toBe('保存しました');
  });

  it('中身が変わらなければ「版だけを足しました」', async () => {
    await openDirty();
    requestSave();
    await flush();
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"unchanged","itemId":7}' })));
    await flush();
    expect(useNoticeStore.getState().message).toBe(t.unchanged);
  });

  it('ほかのジョブのイベントは見ない。保存が待っている間に積むと Busy の知らせ', async () => {
    await openDirty();
    requestSave();
    await flush();
    act(() => onEditorJob({ ...event('done'), id: 10 }));
    act(() => onEditorJob({ ...event('done'), kind: 'import' }));
    expect(useEditorStore.getState().save?.job).toBe(9);
    // ボタンのほかに ⌘S を押しても、2 本目は積まない
    requestSave();
    expect(saveEdit).toHaveBeenCalledTimes(1);
    expect(useNoticeStore.getState().message).toBe(t.saveBusy);
  });

  it('Rust が保存を断ったら（Busy）知らせだけ、ほかはシート', async () => {
    await openDirty();
    vi.mocked(saveEdit).mockRejectedValueOnce(
      new IpcError({ code: 'Busy', message: '待っています' }),
    );
    requestSave();
    await flush();
    expect(useNoticeStore.getState().message).toBe(t.saveBusy);
    expect(useEditorStore.getState().sheet).toBeNull();

    vi.mocked(saveEdit).mockRejectedValueOnce(
      new IpcError({ code: 'Conflict', message: '版に戻したので開き直してください' }),
    );
    requestSave();
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'saveFailed', code: 'Conflict' });
  });

  it('ジョブが Conflict で失敗したら、ジョブの記録の code で分かる言葉のシート', async () => {
    await openDirty();
    render(<EditorSheets />);
    requestSave();
    await flush();
    vi.mocked(listJobs).mockResolvedValue([
      job({ errorCode: 'Conflict', message: '開いたあとに別の保存で変わりました' }),
    ]);
    act(() => onEditorJob(event('failed', { message: '開いたあとに別の保存で変わりました' })));
    await flush();
    expect(useEditorStore.getState().save).toBeNull();
    expect(screen.getByRole('heading', { name: t.saveFailed })).toBeInTheDocument();
    expect(screen.getByRole('alert')).toHaveTextContent(t.saveFailures.Conflict ?? '');
    expect(screen.getByRole('alert')).toHaveTextContent('開いたあとに別の保存で変わりました');
    fireEvent.click(screen.getByRole('button', { name: t.ok }));
    expect(useEditorStore.getState().sheet).toBeNull();
  });

  it('Mismatch・TooLarge も分かる言葉。元に戻すの Conflict には別名で作ったアイテムの説明を添える', async () => {
    await openDirty();
    render(<EditorSheets />);
    act(() =>
      useEditorStore.setState({
        sheet: { kind: 'saveFailed', mode: 'overwrite', code: 'Mismatch', message: '' },
      }),
    );
    expect(screen.getByRole('alert')).toHaveTextContent(t.saveFailures.Mismatch ?? '');
    act(() =>
      useEditorStore.setState({
        sheet: { kind: 'saveFailed', mode: 'overwrite', code: 'TooLarge', message: '' },
      }),
    );
    expect(screen.getByRole('alert')).toHaveTextContent(t.saveFailures.TooLarge ?? '');
    act(() =>
      useEditorStore.setState({
        sheet: { kind: 'saveFailed', mode: 'revert', code: 'Conflict', message: '同じ中身' },
      }),
    );
    expect(screen.getByRole('alert')).toHaveTextContent(t.revertConflictHint);
  });

  it('警告があるときだけ確認のシートに警告を並べ、「保存」で積む', async () => {
    await openDirty();
    act(() =>
      useEditorStore.setState({
        warnings: [
          {
            node: null,
            param: null,
            code: 'metadataDropped',
            severity: 'warning',
            message: 'EXIF を書けません',
          },
        ],
      }),
    );
    render(<EditorSheets />);
    act(() => requestSave());
    expect(saveEdit).not.toHaveBeenCalled();
    expect(screen.getByRole('heading', { name: t.warningsTitle })).toBeInTheDocument();
    expect(
      screen.getByText(`${t.issues.metadataDropped}（EXIF を書けません）`),
    ).toBeInTheDocument();
    act(() => confirmWarnings());
    await flush();
    expect(saveEdit).toHaveBeenCalledWith('aa01', { type: 'overwrite' }, null);
  });

  it('最初の段から進まなければ「エディタを閉じると進みます」を出し、状態を取り直す（駐車ならキャッシュを空にする）', async () => {
    await openDirty();
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    requestSave();
    await vi.advanceTimersByTimeAsync(0);
    act(() => onEditorJob(event('running', { progress: 0.05 })));
    await act(() => vi.advanceTimersByTimeAsync(SLOW_SAVE_MS));
    expect(useEditorStore.getState().save?.slow).toBe(true);
    expect(viewEdit).toHaveBeenCalled();
    // 進んだら消す
    act(() => onEditorJob(event('running', { progress: 0.5 })));
    expect(useEditorStore.getState().save?.slow).toBe(false);
  });
});

describe('別名で保存', () => {
  it('名前のシートで積み、同じ中身のアイテムがあれば（existing）それを示してそこへ移れる', async () => {
    await openDirty();
    render(<EditorSheets />);
    act(() => openSaveAs());
    const input = screen.getByLabelText(t.saveAsName);
    expect(input).toHaveValue(t.saveAsDefault('hero'));
    expect(screen.getByText(t.saveAsFile('hero のコピー.png'))).toBeInTheDocument();
    fireEvent.change(input, { target: { value: 'a/b' } });
    expect(screen.getByRole('alert')).toHaveTextContent(t.saveAsBadName);
    expect(screen.getByRole('button', { name: t.save })).toBeDisabled();
    fireEvent.change(input, { target: { value: '夕方' } });
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    expect(saveEdit).toHaveBeenCalledWith(
      'aa01',
      { type: 'saveAs', title: '夕方', stem: '夕方' },
      null,
    );

    savedRefresh([]);
    act(() =>
      onEditorJob(
        event('done', {
          resultJson:
            '{"outcome":"existing","itemId":3,"role":"original","version":0,"trashed":false}',
        }),
      ),
    );
    await flush();
    const notice = useNoticeStore.getState();
    expect(notice.message).toBe(t.existing(t.existingRoles.original ?? ''));
    expect(notice.action?.label).toBe(t.showItem);
  });

  it('新しいアイテムができたら「…として保存しました」と表示のボタン', async () => {
    await openDirty();
    act(() => submitSaveAs('夕方'));
    await flush();
    savedRefresh([]);
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":12}' })));
    await flush();
    expect(useNoticeStore.getState().message).toBe(t.savedAs('夕方'));
    expect(useNoticeStore.getState().action?.label).toBe(t.showItem);
  });
});

describe('未保存で閉じる', () => {
  it('［キャンセル］なら閉じない、［保存しない］なら閉じて下書きを捨てる', async () => {
    await openDirty();
    render(<EditorSheets />);
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor().then((v) => (closed = v));
    });
    expect(screen.getByRole('heading', { name: t.unsavedTitle })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: ja.common.cancel }));
    await flush();
    expect(closed).toBe(false);
    expect(useEditorStore.getState().phase).toBe('open');

    act(() => {
      void requestCloseEditor().then((v) => (closed = v));
    });
    fireEvent.click(screen.getByRole('button', { name: t.dontSave }));
    await flush();
    expect(closed).toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');
    expect(closeEdit).toHaveBeenCalledWith('aa01');
    expect(discardEditDraft).toHaveBeenCalledWith(7);
  });

  it('［保存］なら保存し、保存が終わってから閉じる', async () => {
    await openDirty();
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('library').then((v) => (closed = v));
    });
    act(() => answerUnsaved('save'));
    await flush();
    expect(saveEdit).toHaveBeenCalledTimes(1);
    expect(useEditorStore.getState().phase).toBe('open');
    expect(closed).toBeNull();
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(closed).toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');
    expect(discardEditDraft).not.toHaveBeenCalled();
  });

  it('保存していない変更がなければ、聞かずに閉じる', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened({}, { status: status({ dirty: false }) }));
    await act(() => openEditor(item()));
    await expect(requestCloseEditor()).resolves.toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');
  });
});

describe('保存していない変更がないとき', () => {
  it('⌘S・ファイル › 保存は積まない（元の形式に書けなければ形式を変えて積む）', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened({}, { status: status({ dirty: false }) }));
    await act(() => openEditor(item()));
    await flush();
    requestSave();
    await flush();
    expect(saveEdit).not.toHaveBeenCalled();
    act(() =>
      useEditorStore.setState({
        source: { kind: 'image', width: 64, height: 32, frames: null, format: 'psd' },
      }),
    );
    requestSave();
    await flush();
    expect(saveEdit).toHaveBeenCalledWith(
      'aa01',
      { type: 'overwrite' },
      { type: 'to', format: 'png' },
    );
  });
});

describe('応えの来ていない手', () => {
  it('送った・ためた手も未保存に数え、聞かずに閉じない（ためた手を捨てない）', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened({}, { status: status({ dirty: false }) }));
    await act(() => openEditor(item()));
    await flush();
    vi.mocked(stepEdit).mockImplementation(() => new Promise(() => undefined));
    act(() => {
      pushEditorSteps([{ type: 'undo' }]);
      pushEditorSteps([{ type: 'redo' }]);
    });
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('window').then((v) => (closed = v));
    });
    await flush();
    expect(closed).toBeNull();
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'unsaved', then: 'window' });
    expect(useEditorStore.getState().phase).toBe('open');
    expect(stepEdit).toHaveBeenCalledTimes(1);
  });

  it('保存は、応えの来ていない手の結果を待ってから積む', async () => {
    await openDirty();
    let answer: ((r: EditStepResultDto) => void) | null = null;
    vi.mocked(stepEdit).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          answer = resolve;
        }),
    );
    act(() => {
      pushEditorSteps([{ type: 'undo' }]);
    });
    requestSave();
    await flush();
    expect(saveEdit).not.toHaveBeenCalled();
    await act(async () => {
      answer!(result({ status: status({ rev: 3, head: 3 }) }));
      await new Promise((r) => setTimeout(r, 0));
    });
    await flush();
    expect(saveEdit).toHaveBeenCalledTimes(1);
  });
});

describe('閉じる流れの順番', () => {
  it('［保存しない］は下書きを捨て終えてから閉じたことにする（窓を壊す・ライブラリを替える前に）', async () => {
    await openDirty();
    const order: string[] = [];
    // edit_close の応えは遅れて届く（そのあとに下書きを捨てる）
    vi.mocked(closeEdit).mockImplementation(async () => {
      order.push('close');
      await new Promise((r) => setTimeout(r, 0));
      return null;
    });
    vi.mocked(discardEditDraft).mockImplementation(async () => {
      order.push('discard');
      return true;
    });
    act(() => {
      void requestCloseEditor('library').then(() => order.push('settled'));
    });
    act(() => answerUnsaved('discard'));
    await flush();
    expect(order).toEqual(['close', 'discard', 'settled']);
  });

  it('［保存］のあと、保存の間に描いた分があれば閉じずにもう一度聞く', async () => {
    await openDirty();
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('library').then((v) => (closed = v));
    });
    act(() => answerUnsaved('save'));
    await flush();
    expect(saveEdit).toHaveBeenCalledTimes(1);
    // 保存したのは頭 2。保存の間に頭 3 まで進んだ
    vi.mocked(viewEdit).mockResolvedValue(
      result({
        status: status({ rev: 3, head: 3, dirty: true, saving: null }),
        doc: { base: 2, order: [], nodes: [], outputs: null },
        view: { type: 'shown', view: viewDto() },
        notices: ['保存しました'],
      }),
    );
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(closed).toBeNull();
    expect(useEditorStore.getState().phase).toBe('open');
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'unsaved', then: 'library' });
    act(() => answerUnsaved('discard'));
    await flush();
    expect(closed).toBe(true);
  });
});

describe('閉じたあとに終わる保存', () => {
  it('保存の途中で閉じても、終わったら一覧を読み直して知らせる。失敗も知らせる', async () => {
    vi.mocked(saveEdit).mockResolvedValue({
      ...started(),
      job: 21,
      status: status({ saving: 21 }),
    } as Awaited<ReturnType<typeof saveEdit>>);
    await openDirty();
    requestSave();
    await flush();
    // 今の頭を保存している途中なので聞かずに閉じる（保存は続く）
    await act(() => requestCloseEditor());
    expect(useEditorStore.getState().phase).toBe('closed');
    act(() =>
      onEditorJob({ ...event('done', { resultJson: '{"outcome":"saved","itemId":7}' }), id: 21 }),
    );
    await flush();
    expect(invalidateItems).toHaveBeenCalledWith([7]);
    expect(useNoticeStore.getState().message).toBe(t.savedClosed('hero'));

    vi.mocked(saveEdit).mockResolvedValue({
      ...started(),
      job: 22,
      status: status({ saving: 22 }),
    } as Awaited<ReturnType<typeof saveEdit>>);
    await openDirty();
    requestSave();
    await flush();
    await act(() => requestCloseEditor());
    act(() => onEditorJob({ ...event('failed', { message: 'ディスクがいっぱいです' }), id: 22 }));
    expect(useNoticeStore.getState().message).toBe(
      t.saveFailedClosed('hero', 'ディスクがいっぱいです'),
    );
  });
});

describe('素材の設定の知らせ（M12 の決定 11）', () => {
  const asset = (settings: Partial<AssetDto['settings']>) =>
    ({
      settings: {
        pixelArt: null,
        pixelScale: null,
        grid: null,
        frameMs: null,
        pivot: null,
        loopRange: null,
        animTag: null,
        updatedAt: null,
        ...settings,
      },
    }) as AssetDto;

  it('上書きで大きさが変わり、格子などの素材の設定があれば、新しい大きさに合うか確かめるよう知らせる', async () => {
    await openDirty();
    requestSave();
    await flush();
    savedRefresh();
    vi.mocked(getItems).mockResolvedValue([item({ width: 32, height: 32 })]);
    vi.mocked(getAsset).mockResolvedValue(
      asset({
        grid: { cellW: 16, cellH: 16, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0, count: null },
      }),
    );
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    await flush();
    expect(getAsset).toHaveBeenCalledWith(7);
    expect(useNoticeStore.getState().message).toBe(t.assetSettingsCheck);
    // エディタのアイテムの概要も新しい今にする（次の保存の比べる大きさ）
    expect(useEditorStore.getState().item?.width).toBe(32);
  });

  it('大きさが同じ・素材の設定がなければ知らせない', async () => {
    await openDirty();
    requestSave();
    await flush();
    savedRefresh();
    vi.mocked(getItems).mockResolvedValue([item()]);
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    await flush();
    expect(getAsset).not.toHaveBeenCalled();
    expect(useNoticeStore.getState().message).toBe('保存しました');

    // もう一度描いて保存：大きさは変わったが、素材の設定がない
    act(() => useEditorStore.setState({ status: status({ dirty: true }) }));
    requestSave();
    await flush();
    savedRefresh();
    vi.mocked(getItems).mockResolvedValue([item({ width: 16 })]);
    vi.mocked(getAsset).mockResolvedValue(asset({}));
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    await flush();
    expect(getAsset).toHaveBeenCalledTimes(1);
    expect(useNoticeStore.getState().message).toBe('保存しました');
  });
});

describe('窓を閉じる見張り', () => {
  function fakeWindow() {
    let handler: ((e: { preventDefault: () => void }) => void) | null = null;
    const unlisten = vi.fn();
    const win = {
      onCloseRequested: vi.fn(async (cb: (e: { preventDefault: () => void }) => void) => {
        handler = cb;
        return unlisten;
      }),
      destroy: vi.fn(async () => undefined),
    };
    vi.mocked(getCurrentWindow).mockReturnValue(win as never);
    return { win, unlisten, request: () => handler?.({ preventDefault: vi.fn() }) };
  }

  it('未保存の間だけ閉じる操作を受け、［保存しない］は下書きを捨ててから窓を壊す', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    const { win, unlisten, request } = fakeWindow();
    const stop = watchWindowClose();
    try {
      await openDirty();
      await flush();
      expect(win.onCloseRequested).toHaveBeenCalledTimes(1);
      const order: string[] = [];
      vi.mocked(closeEdit).mockImplementation(async () => {
        await new Promise((r) => setTimeout(r, 0));
        return null;
      });
      vi.mocked(discardEditDraft).mockImplementation(async () => {
        order.push('discard');
        return true;
      });
      win.destroy.mockImplementation(async () => {
        order.push('destroy');
      });
      act(() => request());
      expect(useEditorStore.getState().sheet).toEqual({ kind: 'unsaved', then: 'window' });
      act(() => answerUnsaved('discard'));
      await flush();
      expect(order).toEqual(['discard', 'destroy']);
      expect(unlisten).toHaveBeenCalled();
    } finally {
      stop();
      vi.mocked(isTauri).mockReturnValue(false);
    }
  });

  it('［キャンセル］なら窓を閉じず、未保存でなくなったら受け口を外す', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    const { win, unlisten, request } = fakeWindow();
    const stop = watchWindowClose();
    try {
      await openDirty();
      await flush();
      act(() => request());
      act(() => answerUnsaved('cancel'));
      await flush();
      expect(win.destroy).not.toHaveBeenCalled();
      expect(unlisten).not.toHaveBeenCalled();
      act(() => useEditorStore.setState({ status: status({ dirty: false }) }));
      expect(unlisten).toHaveBeenCalledTimes(1);
    } finally {
      stop();
      vi.mocked(isTauri).mockReturnValue(false);
    }
  });
});

describe('エディタのシートの重なり', () => {
  it('シートは body に出す（設定などルートのシートの下に隠れない）', async () => {
    await openDirty();
    render(
      <div data-testid="editor-layer">
        <EditorSheets />
      </div>,
    );
    act(() => {
      void requestCloseEditor('library');
    });
    const scrim = screen.getByRole('dialog').parentElement;
    expect(scrim?.parentElement).toBe(document.body);
    expect(screen.getByTestId('editor-layer')).toBeEmptyDOMElement();
    act(() => answerUnsaved('cancel'));
  });
});

describe('下書きのシート', () => {
  it('下書きがあれば「前回の編集の下書きがあります」、下書きから開くと fromDraft で開き直す', async () => {
    await openDirty({ exists: true, fresh: true, updatedAt: Date.now() });
    render(<EditorSheets />);
    expect(screen.getByRole('heading', { name: t.draftTitle })).toBeInTheDocument();
    expect(screen.getByText(t.draftHistory)).toBeInTheDocument();
    vi.mocked(openEdit).mockResolvedValue(opened({ exists: true, fresh: true, opened: true }));
    fireEvent.click(screen.getByRole('button', { name: t.draftOpen }));
    await flush();
    expect(openEdit).toHaveBeenLastCalledWith(7, expect.anything(), true);
    expect(useEditorStore.getState().sheet).toBeNull();
  });

  it('土台が変わっていれば下書きからは開けず、捨てるだけ', async () => {
    await openDirty({ exists: true, fresh: false, updatedAt: Date.now() });
    render(<EditorSheets />);
    expect(screen.getByText(t.draftStale)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: t.draftOpen })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: t.draftDiscard }));
    await flush();
    expect(discardEditDraft).toHaveBeenCalledWith(7);
    expect(useEditorStore.getState().sheet).toBeNull();
    expect(useEditorStore.getState().phase).toBe('open');
  });
});

describe('版の一覧・元に戻す', () => {
  const versions: EditVersionDto[] = [
    {
      version: 0,
      kind: 'original',
      revertTo: null,
      ext: 'png',
      format: 'png',
      sizeBytes: 2048,
      width: 64,
      height: 32,
      durationMs: null,
      fileState: 'hold',
      createdAt: Date.now() - 3_600_000,
    },
    {
      version: 1,
      kind: 'edit',
      revertTo: null,
      ext: 'webp',
      format: 'webp',
      sizeBytes: 1024,
      width: 64,
      height: 32,
      durationMs: null,
      fileState: 'kept',
      createdAt: Date.now() - 1_800_000,
    },
    {
      version: 2,
      kind: 'revert',
      revertTo: 0,
      ext: 'png',
      format: 'png',
      sizeBytes: 2048,
      width: 64,
      height: 32,
      durationMs: null,
      fileState: 'hold',
      createdAt: Date.now(),
    },
  ];

  it('文字だけの一覧（今・照合が合わずに残した版の印）から途中の版に戻す', async () => {
    await openDirty();
    vi.mocked(listEditVersions).mockResolvedValue(versions);
    render(<EditorSheets />);
    act(() => useEditorStore.setState({ sheet: { kind: 'versions' } }));
    await flush();
    expect(screen.getByText(t.versionName('2'))).toHaveTextContent(t.versionCurrent);
    expect(screen.getByText(t.versionName('1'))).toHaveTextContent(t.versionKept);
    expect(screen.getByText(/版 0 に戻した · PNG · 64 × 32 · 2/)).toBeInTheDocument();
    // 今の版には戻すボタンがない
    expect(screen.getAllByRole('button', { name: t.versionRestore })).toHaveLength(2);
    fireEvent.click(screen.getAllByRole('button', { name: t.versionRestore })[0] as HTMLElement);
    await flush();
    expect(screen.getByRole('heading', { name: t.versionRestoreTitle('1') })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.versionRestore }));
    await flush();
    expect(saveEdit).toHaveBeenCalledWith('aa01', { type: 'revert', to: 1 }, null);
  });

  it('元に戻すが終わったら（needsReopen）下書きを捨てて開き直す', async () => {
    await openDirty();
    vi.mocked(listEditVersions).mockResolvedValue(versions);
    render(<EditorSheets />);
    act(() => useEditorStore.setState({ sheet: { kind: 'revert' } }));
    await flush();
    expect(screen.getByText(t.revertConflictHint)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.revertConfirm }));
    await flush();
    expect(saveEdit).toHaveBeenCalledWith('aa01', { type: 'revert', to: 0 }, null);
    vi.mocked(viewEdit).mockResolvedValue(
      result({
        status: status({ dirty: false, saving: null, needsReopen: true }),
        doc: { base: 2, order: [], nodes: [], outputs: null },
        view: { type: 'shown', view: viewDto() },
      }),
    );
    vi.mocked(getItems).mockResolvedValue([item({ ext: 'png' })]);
    vi.mocked(openEdit).mockClear();
    vi.mocked(openEdit).mockResolvedValue(opened({}, { status: status({ dirty: false }) }));
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    await flush();
    expect(discardEditDraft).toHaveBeenCalledWith(7);
    expect(openEdit).toHaveBeenCalledTimes(1);
    expect(useNoticeStore.getState().message).toBe(t.reverted);
  });

  it('まだ保存していなければ戻す版がない', async () => {
    await openDirty();
    vi.mocked(listEditVersions).mockResolvedValue([]);
    render(<EditorSheets />);
    act(() => useEditorStore.setState({ sheet: { kind: 'revert' } }));
    await flush();
    expect(screen.getByText(t.versionsEmpty)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: t.revertConfirm })).toBeDisabled();
  });
});
