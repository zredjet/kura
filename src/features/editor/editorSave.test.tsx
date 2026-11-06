import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import { type AssetDto, getAsset } from '../../ipc/asset';
import {
  closeEdit,
  discardEditDraft,
  type EditPendingSaveDto,
  type EditStatusDto,
  type EditStepResultDto,
  type EditVersionDto,
  type EditImageView,
  type EditWritableFormatDto,
  listEditFormats,
  listEditVersions,
  listPendingSaves,
  openEdit,
  saveEdit,
  stepEdit,
  viewEdit,
} from '../../ipc/edit';
import { getItems, type ItemSummaryDto } from '../../ipc/items';
import {
  cancelJob,
  type JobDto,
  type JobProgress,
  listJobs,
  pauseJob,
  resumeJob,
} from '../../ipc/jobs';
import { IpcError } from '../../ipc/result';
import { invalidateItems } from '../../stores/itemCache';
import { shownSaves, useJobsStore } from '../../stores/jobsStore';
import { useNoticeStore } from '../../stores/noticeStore';
import { useSettingsStore } from '../../stores/settingsStore';
import { AUDIO_FORMATS } from './audio/audioFormats';
import {
  answerUnsaved,
  askRestore,
  confirmRevert,
  confirmWarnings,
  defaultFormatChoice,
  dismissSheet,
  formatChoices,
  onEditorJob as handleEditorJob,
  openRevert,
  openSaveAs,
  openVersions,
  requestCloseEditor,
  requestSave,
  SLOW_SAVE_MS,
  submitSaveAs,
  watchWindowClose,
} from './editorSave';
import { openEditor } from './editorKinds';
import { EditorSheets } from './EditorSheets';
import {
  closeEditor,
  editorRedo,
  editorUndo,
  openEditorAs,
  pushEditorSteps,
  refreshEditorView,
  reopenEditor,
  useEditorStore,
} from './editorStore';
import { IMAGE_EDITOR } from './image/imageKind';
import { FormatOutputRow } from './OpsPanel';
import { DEFAULT_TOOLS, useToolStore } from './image/toolStore';

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
  listEditFormats: vi.fn(),
  listEditVersions: vi.fn(async () => []),
  listPendingSaves: vi.fn(async () => []),
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
  pauseJob: vi.fn(async () => null),
  resumeJob: vi.fn(async () => null),
  cancelJob: vi.fn(async () => null),
}));

vi.mock('../../stores/libraryStore', () => ({
  useLibraryStore: { getState: () => ({ loadList: vi.fn(), refresh: vi.fn() }) },
}));

vi.mock('../../stores/itemCache', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../stores/itemCache')>()),
  invalidateItems: vi.fn(),
}));

const t = ja.editor;

/** 保存のジョブのイベント（本物と同じく、jobsStore が受けてからエディタへ回す）。 */
function onEditorJob(event: JobProgress): void {
  useJobsStore.getState().apply(event);
  handleEditorJob(event);
}

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

function viewDto(over: Partial<EditImageView> = {}): EditImageView {
  return {
    type: 'image',
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

function opened(
  draft: Partial<Draft> = {},
  over: Partial<EditStepResultDto> = {},
  originalFormat = 'png',
) {
  return {
    session: 'aa01',
    itemId: 7,
    baseVersion: 0,
    source: { kind: 'image', width: 64, height: 32, frames: null, format: 'png', audio: null },
    originalFormat,
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
    lengths: null,
  };
}

/** 音声の保存を積んだ結果（`edit_save` の保存の前後の長さ。[フレーム, レート]）。 */
function savingLengths(before: [number, number], after: [number, number]) {
  vi.mocked(saveEdit).mockResolvedValue({
    ...started(),
    lengths: {
      before: { frames: before[0], rate: before[1] },
      after: { frames: after[0], rate: after[1] },
    },
  } as Awaited<ReturnType<typeof saveEdit>>);
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

/** 音声のアイテム（WAV・2 秒）。 */
function audioItem(over: Partial<ItemSummaryDto> = {}): ItemSummaryDto {
  return item({
    ext: 'wav',
    kind: 'audio',
    width: null,
    height: null,
    durationMs: 2000,
    title: 'loop',
    ...over,
  });
}

/**
 * 音声のアイテムを未保存で開く。核の保存の流れ（保存を積む・形式のシート・大きさの確かめ）だけを見るので、表示は画像の
 * アダプタで受け、種類だけを音声にする（音声の表示の流れは `audio/audioStore.test.ts`）。
 */
async function openAudioDirty(over: Partial<ItemSummaryDto> = {}, originalFormat?: string) {
  const audio = audioItem(over);
  vi.mocked(openEdit).mockResolvedValue(
    opened({}, {}, originalFormat ?? AUDIO_FORMATS.formatOfExt(audio.ext) ?? audio.ext),
  );
  await act(() => openEditorAs({ ...IMAGE_EDITOR.adapter, kind: 'audio' }, audio));
  await flush();
  vi.mocked(viewEdit).mockClear();
}

/** 形式の表の行（Rust の `EditWritableFormatDto`）。 */
function formatRow(
  format: string,
  available: boolean,
  over: Partial<EditWritableFormatDto> = {},
): EditWritableFormatDto {
  return {
    format,
    ext: format,
    available,
    missingTool: null,
    lossless: false,
    loopPoints: false,
    markers: false,
    highBitDepth: false,
    ...over,
  };
}

/** ffmpeg のない組の音声の形式の一覧（mp3 は書けない、代わりは ogg）。 */
const AUDIO_ROWS: EditWritableFormatDto[] = [
  formatRow('wav', true, { lossless: true, loopPoints: true, markers: true, highBitDepth: true }),
  formatRow('aiff', true, { lossless: true, loopPoints: true, markers: true, highBitDepth: true }),
  formatRow('flac', true, { lossless: true, loopPoints: true, highBitDepth: true }),
  formatRow('mp3', false, { missingTool: 'ffmpeg' }),
  formatRow('m4a', false),
  formatRow('aac', false),
  formatRow('ogg', true, { loopPoints: true }),
  formatRow('opus', false, { loopPoints: true }),
];

const formatUnavailable = () =>
  new IpcError({ code: 'FormatUnavailable', message: 'mp3 には書けません（既定は ogg）' });

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
  vi.mocked(listPendingSaves).mockReset();
  vi.mocked(listPendingSaves).mockResolvedValue([]);
  useToolStore.setState({ ...DEFAULT_TOOLS });
  useNoticeStore.setState({ message: null, action: null });
  useJobsStore.setState({ imports: {}, dismissed: {}, saves: {}, endedSaves: {} });
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

  it('保存の間に手を足して閉じようとし、ジョブが失敗したら、失敗のシートに替え、閉じるのを待つ呼び手に false を返す', async () => {
    await openDirty();
    requestSave();
    await flush();
    // 保存を積んだあとに手を足した（保存の rev より先の頭）：閉じると未保存のシート
    const status = useEditorStore.getState().status;
    if (!status) throw new Error('状態がない');
    useEditorStore.setState({ status: { ...status, head: status.head + 1 } });
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('window').then((v) => (closed = v));
    });
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'unsaved' });
    vi.mocked(listJobs).mockResolvedValue([
      job({ errorCode: 'Conflict', message: '開いたあとに別の保存で変わりました' }),
    ]);
    act(() => onEditorJob(event('failed', { message: '開いたあとに別の保存で変わりました' })));
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'saveFailed', code: 'Conflict' });
    expect(closed).toBe(false);
    expect(useEditorStore.getState().phase).toBe('open');
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

describe('書けない形式（FormatUnavailable）', () => {
  it('選択肢は種類の形式の表の書ける形式（表の順）、既定は代わりの既定（書けなければ最初の書ける形式）', () => {
    const choices = formatChoices(AUDIO_FORMATS, AUDIO_ROWS);
    expect(choices.map((c) => c.format)).toEqual(['wav', 'aiff', 'flac', 'ogg', 'mp3', 'm4a']);
    expect(formatChoices(null, AUDIO_ROWS)).toEqual([]);
    expect(defaultFormatChoice(choices, 'ogg')).toBe('ogg');
    expect(defaultFormatChoice(choices, 'mp3')).toBe('wav');
    expect(defaultFormatChoice(choices, null)).toBe('wav');
    expect(
      defaultFormatChoice(
        choices.map((c) => ({ ...c, available: false })),
        'ogg',
      ),
    ).toBeNull();
  });

  it('保存が断られたら書ける形式を読んで選ぶシートを出し、［保存］で選んだ形式を渡して積む（画像は PNG が既定）', async () => {
    await openDirty();
    // 出力の形式を WebP にしてあるが、この環境では書けない
    act(() =>
      useEditorStore.setState((s) => ({
        doc: {
          ...s.doc,
          outputs: s.doc.outputs.map((o) => ({ ...o, format: { type: 'to', format: 'webp' } })),
        },
      })),
    );
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: [
        formatRow('png', true, { lossless: true }),
        formatRow('jpeg', true),
        formatRow('gif', true, { lossless: true }),
        formatRow('webp', false, { lossless: true }),
        formatRow('heic', false),
      ],
      current: 'webp',
      fallback: 'png',
    });
    render(<EditorSheets />);
    requestSave();
    await flush();
    expect(listEditFormats).toHaveBeenCalledWith('aa01');
    expect(useEditorStore.getState().sheet).toMatchObject({
      kind: 'format',
      mode: { type: 'overwrite' },
      closeAfter: null,
      fallback: 'png',
      unavailable: 'webp',
    });
    expect(
      screen.getByRole('heading', { name: t.formatUnavailableTitle('WebP') }),
    ).toBeInTheDocument();
    expect(screen.getByText(t.formatUnavailableBodyOther('WebP'))).toBeInTheDocument();
    // 選択肢は画像の形式の表の書ける形式（表の順）。書けないものは押せない
    const radios = screen.getAllByRole('radio');
    expect(radios.map((r) => r.getAttribute('value'))).toEqual(['png', 'jpeg', 'gif', 'webp']);
    expect(screen.getByRole('radio', { name: /PNG/ })).toBeChecked();
    expect(screen.getByRole('radio', { name: /WebP/ })).toBeDisabled();
    expect(screen.getByRole('radio', { name: /WebP/ })).toHaveAccessibleName(
      `WebP ${t.formatNotWritable}`,
    );
    fireEvent.click(screen.getByRole('radio', { name: /JPEG/ }));
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    expect(saveEdit).toHaveBeenLastCalledWith(
      'aa01',
      { type: 'overwrite' },
      { type: 'to', format: 'jpeg' },
    );
    expect(useEditorStore.getState().sheet).toBeNull();
    expect(useEditorStore.getState().save).toMatchObject({ job: 9, mode: 'overwrite' });
  });

  it('音声：MP3 は ffmpeg が要る印で、既定は Ogg Vorbis。選んだ形式で失うものを出し、別名で保存も同じ向きで積み直す', async () => {
    await openAudioDirty({ ext: 'mp3' });
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    render(<EditorSheets />);
    requestSave();
    await flush();
    expect(saveEdit).toHaveBeenCalledWith('aa01', { type: 'overwrite' }, null);
    expect(
      screen.getByRole('heading', { name: t.formatUnavailableTitle('MP3') }),
    ).toBeInTheDocument();
    expect(screen.getByText(t.formatUnavailableBody('MP3'))).toBeInTheDocument();
    const radios = screen.getAllByRole('radio');
    expect(radios.map((r) => r.getAttribute('value'))).toEqual([
      'wav',
      'aiff',
      'flac',
      'ogg',
      'mp3',
      'm4a',
    ]);
    expect(screen.getByRole('radio', { name: /Ogg Vorbis/ })).toBeChecked();
    expect(screen.getByRole('radio', { name: /MP3/ })).toBeDisabled();
    expect(screen.getByRole('radio', { name: /MP3/ })).toHaveAccessibleName(`MP3 ${t.needsFfmpeg}`);
    expect(screen.getByRole('radio', { name: /M4A/ })).toHaveAccessibleName(
      `M4A ${t.formatNotWritable}`,
    );
    // 失うもの：Ogg Vorbis は非可逆
    expect(screen.getByText(t.audio.lossLossy('Ogg Vorbis'))).toBeInTheDocument();
    fireEvent.click(screen.getByRole('radio', { name: /FLAC/ }));
    expect(screen.queryByText(t.audio.lossLossy('Ogg Vorbis'))).toBeNull();
    fireEvent.click(screen.getByRole('radio', { name: /Ogg Vorbis/ }));
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    expect(saveEdit).toHaveBeenLastCalledWith(
      'aa01',
      { type: 'overwrite' },
      { type: 'to', format: 'ogg' },
    );

    // 別名で保存が断られたら、同じ名前の別名で積み直す
    act(() => {
      useJobsStore.getState().apply(event('done'));
      useEditorStore.setState({ save: null, status: status() });
    });
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    act(() => submitSaveAs('写し'));
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({
      kind: 'format',
      mode: { type: 'saveAs', title: '写し', stem: '写し' },
    });
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    expect(saveEdit).toHaveBeenLastCalledWith(
      'aa01',
      { type: 'saveAs', title: '写し', stem: '写し' },
      { type: 'to', format: 'ogg' },
    );
    expect(useEditorStore.getState().save).toMatchObject({ mode: 'saveAs', title: '写し' });
  });

  it('MP3 に書けない理由が形（96 kHz・3 チャンネル以上など）なら、ffmpeg の案内を出さずに形の理由を言う', async () => {
    // ffmpeg はある（MP3 の書き手は組にある）が、今の出力の形が MP3 で書けない
    await openAudioDirty({ ext: 'mp3' });
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS.map((r) => (r.format === 'mp3' ? { ...r, missingTool: null } : r)),
      current: 'mp3',
      fallback: 'ogg',
    });
    render(<EditorSheets />);
    requestSave();
    await flush();
    expect(
      screen.getByRole('heading', { name: t.formatUnavailableTitle('MP3') }),
    ).toBeInTheDocument();
    expect(screen.getByText(t.formatUnavailableBodyShape('MP3'))).toBeInTheDocument();
    expect(screen.queryByText(t.formatUnavailableBody('MP3'))).toBeNull();
    expect(screen.getByRole('radio', { name: /MP3/ })).toBeDisabled();
    expect(screen.getByRole('radio', { name: /MP3/ })).toHaveAccessibleName(
      `MP3 ${t.formatNotWritable}`,
    );
    expect(screen.queryByText(t.needsFfmpeg)).toBeNull();
    expect(screen.getByRole('radio', { name: /Ogg Vorbis/ })).toBeChecked();
  });

  it('閉じる前の保存：［キャンセル］なら閉じない、［保存］なら選んだ形式で保存し終えてから閉じる', async () => {
    await openAudioDirty({ ext: 'mp3' });
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    render(<EditorSheets />);
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('library').then((v) => (closed = v));
    });
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    act(() => answerUnsaved('save'));
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({
      kind: 'format',
      closeAfter: 'library',
    });
    expect(closed).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: ja.common.cancel }));
    await flush();
    expect(closed).toBe(false);
    expect(useEditorStore.getState().phase).toBe('open');

    act(() => {
      void requestCloseEditor('library').then((v) => (closed = v));
    });
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    act(() => answerUnsaved('save'));
    await flush();
    closed = null;
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    expect(saveEdit).toHaveBeenLastCalledWith(
      'aa01',
      { type: 'overwrite' },
      { type: 'to', format: 'ogg' },
    );
    expect(closed).toBeNull();
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(closed).toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');
  });

  it('「元と同じ」の保存が断られたら、Rust の解いた形式（元の MP3）を出す（今の拡張子の WAV ではない）', async () => {
    // MP3 の元を WAV で保存したあと（今の拡張子は wav）、出力を「元と同じ」に戻して保存する
    await openAudioDirty({ ext: 'wav' }, 'mp3');
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    render(<EditorSheets />);
    requestSave();
    await flush();
    expect(saveEdit).toHaveBeenCalledWith('aa01', { type: 'overwrite' }, null);
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'format', unavailable: 'mp3' });
    expect(
      screen.getByRole('heading', { name: t.formatUnavailableTitle('MP3') }),
    ).toBeInTheDocument();
    expect(screen.getByText(t.formatUnavailableBody('MP3'))).toBeInTheDocument();
    expect(screen.queryByText(t.formatUnavailableTitle('WAV'))).toBeNull();
  });

  it('元の形式（Rust の originalFormat）に書けなければ、今の拡張子が書ける形式でも書ける形式を渡して保存する', async () => {
    // HEIC の元を PNG で保存したあと（今の拡張子は png）。「元と同じ」は HEIC なので PNG を渡す
    vi.mocked(openEdit).mockResolvedValue(opened({}, { status: status({ dirty: false }) }, 'heic'));
    await act(() => openEditor(item({ ext: 'png' })));
    await flush();
    requestSave();
    await flush();
    expect(saveEdit).toHaveBeenCalledWith(
      'aa01',
      { type: 'overwrite' },
      { type: 'to', format: 'png' },
    );
  });

  it('⌘S の応え待ちに閉じる要求が来たら、形式を選ぶシートが閉じる要求を引き継ぐ：［保存］なら保存し終えてから閉じ、［キャンセル］なら閉じない', async () => {
    await openAudioDirty({ ext: 'mp3' });
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    render(<EditorSheets />);
    let reject: (error: unknown) => void = () => undefined;
    const pendingSave = () =>
      vi.mocked(saveEdit).mockImplementationOnce(
        () =>
          new Promise((_, rj) => {
            reject = rj;
          }),
      );
    let closed: boolean | null = null;

    // ［保存］：選んだ形式で保存し終えてから閉じる
    pendingSave();
    requestSave();
    await flush();
    expect(saveEdit).toHaveBeenCalledTimes(1);
    act(() => {
      void requestCloseEditor('window').then((v) => (closed = v));
    });
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'unsaved', then: 'window' });
    await act(async () => reject(formatUnavailable()));
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'format', closeAfter: 'window' });
    expect(closed).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    expect(saveEdit).toHaveBeenLastCalledWith(
      'aa01',
      { type: 'overwrite' },
      { type: 'to', format: 'ogg' },
    );
    expect(closed).toBeNull();
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(closed).toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');

    // ［キャンセル］：閉じるのを待つ呼び手に false を返す
    await openAudioDirty({ ext: 'mp3' });
    closed = null;
    pendingSave();
    requestSave();
    await flush();
    act(() => {
      void requestCloseEditor('window').then((v) => (closed = v));
    });
    await act(async () => reject(formatUnavailable()));
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'format', closeAfter: 'window' });
    fireEvent.click(screen.getByRole('button', { name: ja.common.cancel }));
    await flush();
    expect(closed).toBe(false);
    expect(useEditorStore.getState().phase).toBe('open');
    expect(useEditorStore.getState().sheet).toBeNull();
  });

  it('⌘S の応え待ちに閉じる要求が来て保存が断られたら（保存できませんでした）、閉じるのを待つ呼び手に false を返す', async () => {
    await openAudioDirty({ ext: 'mp3' });
    let reject: (error: unknown) => void = () => undefined;
    vi.mocked(saveEdit).mockImplementationOnce(
      () =>
        new Promise((_, rj) => {
          reject = rj;
        }),
    );
    requestSave();
    await flush();
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('window').then((v) => (closed = v));
    });
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'unsaved' });
    await act(async () =>
      reject(new IpcError({ code: 'Conflict', message: '版に戻したので開き直してください' })),
    );
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'saveFailed', code: 'Conflict' });
    expect(closed).toBe(false);
    expect(useEditorStore.getState().phase).toBe('open');
  });

  it('書ける形式を読めなければ、今の「保存できませんでした」のシート', async () => {
    await openAudioDirty({ ext: 'mp3' });
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    vi.mocked(listEditFormats).mockRejectedValueOnce(
      new IpcError({ code: 'NotFound', message: 'x' }),
    );
    render(<EditorSheets />);
    requestSave();
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({
      kind: 'saveFailed',
      code: 'FormatUnavailable',
    });
    expect(screen.getByRole('alert')).toHaveTextContent(t.saveFailures.FormatUnavailable ?? '');
  });
});

describe('出力の形式の行：この環境で書けない形式（M13 段 7）', () => {
  afterEach(() => {
    useSettingsStore.setState({ open: false });
  });

  const mp3Option = (name: string) =>
    within(screen.getByRole('combobox', { name: t.output })).getByRole('option', { name });

  it('開いたときに edit_formats を読み、MP3 を書けなければ行の MP3 は押せず、今の形式が MP3 なら一言を出す', async () => {
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    await openAudioDirty({ ext: 'mp3' });
    expect(listEditFormats).toHaveBeenCalledWith('aa01');
    render(<FormatOutputRow />);
    expect(mp3Option(t.outputOptionUnavailable('MP3', t.needsFfmpeg))).toBeDisabled();
    expect(mp3Option(t.outputKeepUnavailable('MP3', t.needsFfmpeg))).toBeDisabled();
    expect(mp3Option('WAV')).toBeEnabled();
    expect(screen.getByText(t.formatUnavailableBody('MP3'))).toBeInTheDocument();
  });

  it('読めなければ今どおり形式の表だけ（MP3 も押せ、一言はない）', async () => {
    vi.mocked(listEditFormats).mockRejectedValueOnce(
      new IpcError({ code: 'NotFound', message: 'x' }),
    );
    await openAudioDirty({ ext: 'mp3' });
    expect(listEditFormats).toHaveBeenCalledTimes(1);
    expect(useEditorStore.getState().writableFormats).toBeNull();
    render(<FormatOutputRow />);
    expect(mp3Option('MP3')).toBeEnabled();
    expect(mp3Option(t.outputKeep('MP3'))).toBeEnabled();
    expect(screen.queryByText(t.formatUnavailableBody('MP3'))).toBeNull();
  });

  it('設定のシートを閉じたら読み直す（外部ツールの場所を変えたあと）', async () => {
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    await openAudioDirty({ ext: 'mp3' });
    render(<FormatOutputRow />);
    expect(mp3Option(t.outputOptionUnavailable('MP3', t.needsFfmpeg))).toBeDisabled();
    // 設定のシートで ffmpeg を指定した
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS.map((r) =>
        r.format === 'mp3' ? { ...r, available: true, missingTool: null } : r,
      ),
      current: 'mp3',
      fallback: null,
    });
    act(() => useSettingsStore.getState().show('downloads'));
    await flush();
    expect(listEditFormats).toHaveBeenCalledTimes(1);
    act(() => useSettingsStore.getState().close());
    await flush();
    expect(listEditFormats).toHaveBeenCalledTimes(2);
    expect(mp3Option('MP3')).toBeEnabled();
    expect(screen.queryByText(t.formatUnavailableBody('MP3'))).toBeNull();
  });

  describe('手のあと（出力の形が変わる）', () => {
    /** 開いたときの一覧（ffmpeg はあるが、今の形は MP3 に書けない。`mp3` が書けるかだけが変わる）。 */
    const rowsWith = (mp3: boolean) => ({
      formats: AUDIO_ROWS.map((r) =>
        r.format === 'mp3' ? { ...r, available: mp3, missingTool: null } : r,
      ),
      current: 'wav',
      fallback: mp3 ? null : 'ogg',
    });
    const answerWith = (mp3: boolean) =>
      vi.mocked(listEditFormats).mockResolvedValueOnce(rowsWith(mp3));
    const mp3Row = () =>
      useEditorStore.getState().writableFormats?.find((r) => r.format === 'mp3')?.available;
    /** 1 手送って結果を受ける（既定は確定の結果）。 */
    async function hand(over: Partial<EditStepResultDto> = {}) {
      vi.mocked(stepEdit).mockImplementationOnce(async () => result(over));
      act(() => {
        pushEditorSteps([{ type: 'undo' }]);
      });
      await flush();
    }

    it('書けるようになる向き：確定の結果のあとに読み直し、行が押せるようになる（レート変換・モノラルなど）', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      render(<FormatOutputRow />);
      expect(mp3Option(t.outputOptionUnavailable('MP3', t.formatNotWritable))).toBeDisabled();
      expect(listEditFormats).toHaveBeenCalledTimes(1);
      answerWith(true);
      await hand();
      expect(listEditFormats).toHaveBeenCalledTimes(2);
      expect(listEditFormats).toHaveBeenLastCalledWith('aa01');
      expect(mp3Option('MP3')).toBeEnabled();
    });

    it('書けなくなる向き：行が押せなくなり、今の形式なら理由の一言も出す', async () => {
      answerWith(true);
      await openAudioDirty({ ext: 'mp3' });
      render(<FormatOutputRow />);
      expect(mp3Option('MP3')).toBeEnabled();
      expect(screen.queryByText(t.formatUnavailableBodyShape('MP3'))).toBeNull();
      answerWith(false);
      await hand();
      expect(mp3Option(t.outputOptionUnavailable('MP3', t.formatNotWritable))).toBeDisabled();
      expect(screen.getByText(t.formatUnavailableBodyShape('MP3'))).toBeInTheDocument();
      // 戻せば理由も消える
      answerWith(true);
      await hand();
      expect(mp3Option('MP3')).toBeEnabled();
      expect(screen.queryByText(t.formatUnavailableBodyShape('MP3'))).toBeNull();
    });

    it('画像も同じ：縮めて書けるようになった形式が押せるようになる', async () => {
      const row = (webp: boolean) => ({
        formats: [
          formatRow('png', true),
          formatRow('webp', webp),
          formatRow('jpeg', true),
          formatRow('gif', true),
        ],
        current: 'png',
        fallback: null,
      });
      vi.mocked(listEditFormats).mockResolvedValueOnce(row(false));
      await openDirty();
      render(<FormatOutputRow />);
      expect(mp3Option(t.outputOptionUnavailable('WebP', t.formatNotWritable))).toBeDisabled();
      vi.mocked(listEditFormats).mockResolvedValueOnce(row(true));
      await hand();
      expect(mp3Option('WebP')).toBeEnabled();
    });

    it('連打：最新の頼みの応えだけを使い、古い応えは後から届いても捨てる', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      const answers: ((mp3: boolean) => void)[] = [];
      vi.mocked(listEditFormats).mockImplementation(
        () => new Promise((resolve) => answers.push((mp3) => resolve(rowsWith(mp3)))),
      );
      await hand();
      await hand();
      expect(answers).toHaveLength(2);
      // 新しい頼みの応えが先に届く
      answers[1]?.(true);
      await flush();
      expect(mp3Row()).toBe(true);
      // 古い頼みの応えが後から届いても使わない
      answers[0]?.(false);
      await flush();
      expect(mp3Row()).toBe(true);

      // 古い頼みの応えが先に届いても使わない（新しい応えを待つ）
      await hand();
      await hand();
      answers[2]?.(false);
      await flush();
      expect(mp3Row()).toBe(true);
      answers[3]?.(false);
      await flush();
      expect(mp3Row()).toBe(false);
    });

    it('応えを待っても手を止めない：読み直しの応えが届く前に次の手を送る', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      vi.mocked(listEditFormats).mockImplementation(() => new Promise(() => undefined));
      await hand();
      expect(listEditFormats).toHaveBeenCalledTimes(2);
      vi.mocked(stepEdit).mockClear();
      await hand();
      expect(stepEdit).toHaveBeenCalledTimes(1);
    });

    it('プレビューの途中・表示だけの結果では読まない。プレビューを確定したら読む', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      await hand({ status: status({ provisional: true }) });
      expect(listEditFormats).toHaveBeenCalledTimes(1);
      act(() => refreshEditorView());
      await flush();
      expect(viewEdit).toHaveBeenCalled();
      expect(listEditFormats).toHaveBeenCalledTimes(1);
      answerWith(true);
      await hand();
      expect(listEditFormats).toHaveBeenCalledTimes(2);
      expect(mp3Row()).toBe(true);
    });

    it('続けて手を送っている間は読まず、最後の結果のあとに読む（手と競らない）', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      const release: (() => void)[] = [];
      vi.mocked(stepEdit).mockImplementation(
        () =>
          new Promise((resolve) => {
            release.push(() => resolve(result()));
          }),
      );
      act(() => {
        pushEditorSteps([{ type: 'undo' }]);
        pushEditorSteps([{ type: 'undo' }]);
      });
      await flush();
      // 1 つ目の結果：ためた手が残っているので読まない
      release[0]?.();
      await flush();
      expect(listEditFormats).toHaveBeenCalledTimes(1);
      // 最後の結果のあとに読む
      answerWith(true);
      release[1]?.();
      await flush();
      expect(listEditFormats).toHaveBeenCalledTimes(2);
      expect(mp3Row()).toBe(true);
    });

    it('最後の手が届かなかったら、確かめ直すために読む', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      vi.mocked(stepEdit).mockRejectedValueOnce(new IpcError({ code: 'Internal', message: 'x' }));
      answerWith(true);
      act(() => {
        pushEditorSteps([{ type: 'undo' }]);
      });
      await flush();
      expect(listEditFormats).toHaveBeenCalledTimes(2);
      expect(mp3Row()).toBe(true);
    });

    it('読めなければ今の行のまま。同じ中身なら行を差し替えない（再描画しない）', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      const rows = useEditorStore.getState().writableFormats;
      vi.mocked(listEditFormats).mockRejectedValueOnce(
        new IpcError({ code: 'NotFound', message: 'x' }),
      );
      await hand();
      expect(useEditorStore.getState().writableFormats).toBe(rows);
      answerWith(false);
      await hand();
      expect(listEditFormats).toHaveBeenCalledTimes(3);
      expect(useEditorStore.getState().writableFormats).toBe(rows);
    });

    it('エディタが替わったあとに届いた応えは使わない', async () => {
      answerWith(false);
      await openAudioDirty({ ext: 'wav' });
      let answer: (v: Awaited<ReturnType<typeof listEditFormats>>) => void = () => undefined;
      vi.mocked(listEditFormats).mockImplementationOnce(
        () => new Promise((resolve) => (answer = resolve)),
      );
      await hand();
      answerWith(false);
      await openAudioDirty({ ext: 'wav', id: 8 });
      expect(mp3Row()).toBe(false);
      answer(rowsWith(true));
      await flush();
      expect(mp3Row()).toBe(false);
    });
  });

  it('閉じたあと・開き直したあとに届いた一覧は使わない', async () => {
    let answer: (v: Awaited<ReturnType<typeof listEditFormats>>) => void = () => undefined;
    vi.mocked(listEditFormats).mockImplementationOnce(
      () => new Promise((resolve) => (answer = resolve)),
    );
    await openAudioDirty({ ext: 'mp3' });
    await act(() => closeEditor());
    answer({ formats: AUDIO_ROWS, current: 'mp3', fallback: 'ogg' });
    await flush();
    expect(useEditorStore.getState().writableFormats).toBeNull();
    // 閉じている間に設定のシートを閉じても読まない
    act(() => useSettingsStore.getState().show());
    act(() => useSettingsStore.getState().close());
    await flush();
    expect(listEditFormats).toHaveBeenCalledTimes(1);
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

  it('ファイル名の拡張子は元のファイルの形式から決める（画素の形式 source.format ではない）', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened({}, {}, 'jpeg'));
    await act(() => openEditor(item({ ext: 'jpg' })));
    await flush();
    render(<EditorSheets />);
    act(() => openSaveAs());
    expect(screen.getByText(t.saveAsFile('hero のコピー.jpg'))).toBeInTheDocument();
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

describe('シートを出している間の閉じる要求', () => {
  /**
   * 窓を閉じる・ライブラリを切り替える・‹ の閉じる要求を順に送り、どれも false で、シート（同じもの）とエディタが残る
   * ことを確かめる。
   */
  async function expectRefused(): Promise<void> {
    const before = useEditorStore.getState().sheet;
    expect(before).not.toBeNull();
    for (const then of ['window', 'library', 'none'] as const) {
      let answer: boolean | null = null;
      act(() => {
        void requestCloseEditor(then).then((v) => (answer = v));
      });
      await flush();
      expect(answer).toBe(false);
      expect(useEditorStore.getState().sheet).toBe(before);
      expect(useEditorStore.getState().phase).toBe('open');
    }
    expect(closeEdit).not.toHaveBeenCalled();
  }

  it('未保存のシート：閉じる要求を受けず、前の呼び手を待たせたまま。［保存］でもとの要求どおり保存し終えてから閉じる', async () => {
    await openDirty();
    render(<EditorSheets />);
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('library').then((v) => (closed = v));
    });
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'unsaved', then: 'library' });
    await expectRefused();
    expect(closed).toBeNull();
    act(() => answerUnsaved('save'));
    await flush();
    expect(saveEdit).toHaveBeenCalledTimes(1);
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(closed).toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');
  });

  it('形式を選ぶシート：⌘W で未保存のシートに替えず、closeAfter が生きていて［保存］で保存し終えてから閉じる', async () => {
    await openAudioDirty({ ext: 'mp3' });
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    render(<EditorSheets />);
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('window').then((v) => (closed = v));
    });
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    act(() => answerUnsaved('save'));
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'format', closeAfter: 'window' });
    await expectRefused();
    expect(closed).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    expect(saveEdit).toHaveBeenLastCalledWith(
      'aa01',
      { type: 'overwrite' },
      { type: 'to', format: 'ogg' },
    );
    expect(closed).toBeNull();
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(closed).toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');
  });

  it('⌘S の形式を選ぶシート（閉じる要求なし）：⌘W を受けず、［保存］で保存してもエディタは開いたまま', async () => {
    await openAudioDirty({ ext: 'mp3' });
    vi.mocked(listEditFormats).mockResolvedValue({
      formats: AUDIO_ROWS,
      current: 'mp3',
      fallback: 'ogg',
    });
    render(<EditorSheets />);
    vi.mocked(saveEdit).mockRejectedValueOnce(formatUnavailable());
    requestSave();
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'format', closeAfter: null });
    await expectRefused();
    fireEvent.click(screen.getByRole('button', { name: t.save }));
    await flush();
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(useEditorStore.getState().phase).toBe('open');
  });

  it('警告の確認のシート：閉じる要求を受けず、「保存」でもとの要求どおり保存し終えてから閉じる', async () => {
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
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('window').then((v) => (closed = v));
    });
    act(() => answerUnsaved('save'));
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'saveWarnings', closeAfter: 'window' });
    await expectRefused();
    expect(closed).toBeNull();
    act(() => confirmWarnings());
    await flush();
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(closed).toBe(true);
    expect(useEditorStore.getState().phase).toBe('closed');
  });

  it('失敗のシート・別名で保存・下書きのシートでも閉じる要求を受けない', async () => {
    // 失敗のシート
    await openDirty();
    render(<EditorSheets />);
    vi.mocked(saveEdit).mockRejectedValueOnce(
      new IpcError({ code: 'Conflict', message: '版に戻したので開き直してください' }),
    );
    requestSave();
    await flush();
    expect(useEditorStore.getState().sheet).toMatchObject({ kind: 'saveFailed' });
    await expectRefused();
    act(() => dismissSheet());

    // 別名で保存
    act(() => openSaveAs());
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'saveAs' });
    await expectRefused();
    act(() => dismissSheet());
    await act(() => closeEditor());
    vi.mocked(closeEdit).mockClear();

    // 下書きのシート
    await openDirty({ exists: true, fresh: true, updatedAt: Date.now() });
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'draft' });
    await expectRefused();
  });

  it('保存していない変更がなくても、元に戻す・版の一覧・版に戻すのシートの間は閉じない', async () => {
    vi.mocked(openEdit).mockResolvedValue(opened({}, { status: status({ dirty: false }) }));
    await act(() => openEditor(item()));
    await flush();
    act(() => openRevert());
    await expectRefused();
    act(() => dismissSheet());
    act(() => openVersions());
    await expectRefused();
    act(() => askRestore(1));
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'restore', version: 1 });
    await expectRefused();
    act(() => dismissSheet());
    // シートを閉じたら、これまでどおり聞かずに閉じる
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
    // 元のファイルの形式は Rust の originalFormat で決める（source.format は画素の形式）。PSD を開いたとき
    act(() => useEditorStore.setState({ item: item({ ext: 'psd' }), originalFormat: 'psd' }));
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

describe('保存の一時停止・再開・中止', () => {
  /** 上書きの保存を積み、途中まで進める。 */
  async function saving(progress = 0.42) {
    await openDirty();
    requestSave();
    await flush();
    act(() => onEditorJob(event('running', { progress })));
  }

  it('積んだ保存はカプセルが読む表にも載る（名前・向き）', async () => {
    await saving();
    expect(useJobsStore.getState().saves[9]).toMatchObject({
      itemId: 7,
      itemTitle: 'hero',
      mode: 'overwrite',
      state: 'running',
      progress: 0.42,
    });
    // 別名で保存は新しい名前
    act(() => onEditorJob(event('canceled')));
    await flush();
    useJobsStore.setState({ endedSaves: {} });
    act(() => submitSaveAs('夕方'));
    await flush();
    expect(useJobsStore.getState().saves[9]).toMatchObject({ mode: 'saveAs', newTitle: '夕方' });
  });

  it('一時停止：進捗はそのまま（イベントが進捗を持たなくても）。再開は頭から', async () => {
    await saving();
    act(() => onEditorJob(event('paused')));
    expect(useEditorStore.getState().save).toMatchObject({ state: 'paused', progress: 0.42 });
    act(() => onEditorJob(event('queued')));
    expect(useEditorStore.getState().save).toMatchObject({ state: 'queued', progress: null });
    act(() => onEditorJob(event('running')));
    expect(useEditorStore.getState().save?.progress).toBeNull();
    act(() => onEditorJob(event('running', { progress: 0.05 })));
    expect(useEditorStore.getState().save?.progress).toBe(0.05);
  });

  it('一時停止の間は「エディタを閉じると進みます」を出さない。再開したら見張り直す', async () => {
    await openDirty();
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    requestSave();
    await vi.advanceTimersByTimeAsync(0);
    act(() => onEditorJob(event('running', { progress: 0.05 })));
    act(() => onEditorJob(event('paused')));
    await act(() => vi.advanceTimersByTimeAsync(SLOW_SAVE_MS + 1));
    expect(useEditorStore.getState().save?.slow).toBe(false);
    // 出ているときに一時停止しても消える
    act(() => onEditorJob(event('queued')));
    act(() => onEditorJob(event('running', { progress: 0.05 })));
    await act(() => vi.advanceTimersByTimeAsync(SLOW_SAVE_MS));
    expect(useEditorStore.getState().save?.slow).toBe(true);
    act(() => onEditorJob(event('paused')));
    expect(useEditorStore.getState().save?.slow).toBe(false);
  });

  it('一時停止の間の ⌘S・別名で保存…は積まず、再開か中止を促す', async () => {
    await saving();
    act(() => onEditorJob(event('paused')));
    vi.mocked(saveEdit).mockClear();
    act(() => requestSave());
    await flush();
    expect(saveEdit).not.toHaveBeenCalled();
    expect(useNoticeStore.getState().message).toBe(t.saveBusyPaused);
    useNoticeStore.setState({ message: null, action: null });
    act(() => openSaveAs());
    expect(useEditorStore.getState().sheet).toBeNull();
    expect(useNoticeStore.getState().message).toBe(t.saveBusyPaused);
  });

  it('今の頭の上書きを一時停止したまま閉じても聞かない。閉じたあともカプセルに残り、再開して終われば知らせる', async () => {
    await saving();
    act(() => onEditorJob(event('paused')));
    await act(() => requestCloseEditor());
    expect(useEditorStore.getState().phase).toBe('closed');
    expect(discardEditDraft).not.toHaveBeenCalled();
    expect(shownSaves(useJobsStore.getState().saves).map((j) => j.id)).toEqual([9]);

    act(() => onEditorJob(event('queued')));
    act(() => onEditorJob(event('running', { progress: 0.5 })));
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(shownSaves(useJobsStore.getState().saves)).toEqual([]);
    expect(invalidateItems).toHaveBeenCalledWith([7]);
    expect(useNoticeStore.getState().message).toBe(t.savedClosed('hero'));
  });

  it('保存のあとに描いた分があれば閉じるときに聞く。一時停止の間の［保存］は断り、閉じない', async () => {
    await saving();
    act(() => onEditorJob(event('paused')));
    const s = useEditorStore.getState();
    act(() => useEditorStore.setState({ status: { ...(s.status ?? status()), head: 3 } }));
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor().then((v) => (closed = v));
    });
    expect(useEditorStore.getState().sheet?.kind).toBe('unsaved');
    vi.mocked(saveEdit).mockClear();
    act(() => answerUnsaved('save'));
    await flush();
    expect(saveEdit).not.toHaveBeenCalled();
    expect(useNoticeStore.getState().message).toBe(t.saveBusyPaused);
    expect(closed).toBe(false);
    expect(useEditorStore.getState().phase).toBe('open');
  });

  it('閉じる前の保存を一時停止したら、閉じるのをやめる（呼び手に false。再開して終わっても閉じない）', async () => {
    await openDirty();
    let closed: boolean | null = null;
    act(() => {
      void requestCloseEditor('library').then((v) => (closed = v));
    });
    act(() => answerUnsaved('save'));
    await flush();
    expect(closed).toBeNull();
    act(() => onEditorJob(event('paused')));
    await flush();
    expect(closed).toBe(false);
    expect(useEditorStore.getState().phase).toBe('open');

    act(() => onEditorJob(event('queued')));
    savedRefresh();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(useEditorStore.getState().phase).toBe('open');
    expect(useEditorStore.getState().save).toBeNull();
  });

  it('中止：失敗のシートを出さず、保存していない変更のまま次の保存を積める', async () => {
    await saving();
    act(() => onEditorJob(event('paused')));
    vi.mocked(viewEdit).mockClear();
    vi.mocked(viewEdit).mockResolvedValue(
      result({
        status: status({ dirty: true, saving: null }),
        doc: { base: 2, order: [], nodes: [], outputs: null },
        view: { type: 'shown', view: viewDto() },
        notices: ['保存を中止しました'],
      }),
    );
    act(() => onEditorJob(event('canceled')));
    await flush();
    expect(viewEdit).toHaveBeenCalled();
    const s = useEditorStore.getState();
    expect(s.save).toBeNull();
    expect(s.sheet).toBeNull();
    expect(s.status?.dirty).toBe(true);
    expect(useNoticeStore.getState().message).toBe('保存を中止しました');
    expect(shownSaves(useJobsStore.getState().saves)).toEqual([]);
    vi.mocked(saveEdit).mockClear();
    vi.mocked(saveEdit).mockResolvedValue({
      ...started(),
      job: 10,
      status: status({ saving: 10 }),
    } as Awaited<ReturnType<typeof saveEdit>>);
    act(() => requestSave());
    await flush();
    expect(saveEdit).toHaveBeenCalledTimes(1);
    expect(useEditorStore.getState().save?.job).toBe(10);
  });

  it('閉じたあとに中止した保存は何も知らせない（カプセルが消える）', async () => {
    await saving();
    act(() => onEditorJob(event('paused')));
    await act(() => requestCloseEditor());
    useNoticeStore.setState({ message: null, action: null });
    act(() => onEditorJob(event('canceled')));
    await flush();
    expect(shownSaves(useJobsStore.getState().saves)).toEqual([]);
    expect(useNoticeStore.getState().message).toBeNull();
  });

  it('Rust が Busy で断ったら（画面の知らない保存がある）、読み直してカプセルの表に載せる', async () => {
    await openDirty();
    vi.mocked(saveEdit).mockRejectedValueOnce(new IpcError({ code: 'Busy', message: '待って' }));
    vi.mocked(listPendingSaves).mockResolvedValueOnce([
      {
        job: 40,
        itemId: 7,
        itemTitle: 'hero',
        mode: { type: 'overwrite' },
        state: 'paused',
        progress: null,
        message: null,
      },
    ]);
    act(() => requestSave());
    await flush();
    await flush();
    expect(useNoticeStore.getState().message).toBe(t.saveBusy);
    expect(useJobsStore.getState().saves[40]).toMatchObject({ state: 'paused' });
  });

  it('アプリを開き直したあとの保存（DB から読んだもの）も、再開して終われば一覧を読み直して知らせる', async () => {
    vi.mocked(listPendingSaves).mockResolvedValueOnce([
      {
        job: 50,
        itemId: 7,
        itemTitle: 'long-60min',
        mode: { type: 'overwrite' },
        state: 'paused',
        progress: null,
        message: null,
      },
    ]);
    await act(() => useJobsStore.getState().reloadSaves());
    expect(shownSaves(useJobsStore.getState().saves).map((j) => j.id)).toEqual([50]);
    act(() => onEditorJob({ ...event('queued'), id: 50 }));
    act(() =>
      onEditorJob({ ...event('done', { resultJson: '{"outcome":"saved","itemId":7}' }), id: 50 }),
    );
    await flush();
    expect(invalidateItems).toHaveBeenCalledWith([7]);
    expect(useNoticeStore.getState().message).toBe(t.savedClosed('long-60min'));
  });

  it('保存のイベントを取りこぼして Rust から終わりを知ったら、カプセルの表からも消す', async () => {
    await saving();
    vi.mocked(listJobs).mockResolvedValue([job({ id: 9, state: 'canceled' })]);
    vi.mocked(viewEdit).mockResolvedValue(
      result({
        status: status({ saving: null }),
        doc: { base: 2, order: [], nodes: [], outputs: null },
        view: { type: 'shown', view: viewDto() },
      }),
    );
    act(() => refreshEditorView());
    await flush();
    await flush();
    expect(useEditorStore.getState().save).toBeNull();
    expect(useJobsStore.getState().saves[9]).toBeUndefined();
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

  it('音声：上書きで長さが変わり、試聴のループがあれば、新しい長さに合うか確かめるよう知らせる（格子などは見ない）', async () => {
    await openAudioDirty();
    savingLengths([96_000, 48_000], [72_000, 48_000]);
    requestSave();
    await flush();
    expect(useEditorStore.getState().save).toMatchObject({
      before: { kind: 'audio', frames: 96_000, rate: 48_000 },
      after: { kind: 'audio', frames: 72_000, rate: 48_000 },
    });
    savedRefresh();
    vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 1500 })]);
    vi.mocked(getAsset).mockResolvedValue(asset({ loopRange: { start: 0, end: 48_000 } }));
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    await flush();
    expect(getAsset).toHaveBeenCalledWith(7);
    expect(useNoticeStore.getState().message).toBe(t.audio.assetSettingsCheckAudio);
    expect(useEditorStore.getState().item?.durationMs).toBe(1500);

    // 長さが変わっても、試聴のループがなければ知らせない（格子・ピボットは音声に効かない）
    act(() => useEditorStore.setState({ status: status({ dirty: true }) }));
    savingLengths([72_000, 48_000], [48_000, 48_000]);
    requestSave();
    await flush();
    savedRefresh();
    vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 1000 })]);
    vi.mocked(getAsset).mockResolvedValue(
      asset({
        grid: { cellW: 16, cellH: 16, offsetX: 0, offsetY: 0, gapX: 0, gapY: 0, count: null },
      }),
    );
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    await flush();
    expect(getAsset).toHaveBeenCalledTimes(2);
    expect(useNoticeStore.getState().message).toBe('保存しました');

    // 長さが同じなら素材の設定を読まない
    act(() => useEditorStore.setState({ status: status({ dirty: true }) }));
    savingLengths([48_000, 48_000], [48_000, 48_000]);
    requestSave();
    await flush();
    savedRefresh();
    vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 1000 })]);
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    await flush();
    expect(getAsset).toHaveBeenCalledTimes(2);
  });

  // ms の長さ（概要の durationMs）は丸めるので、前後が同じでも設定の区間の意味は変わる
  describe('音声の長さはフレームとレートで比べる', () => {
    const looping = () =>
      vi.mocked(getAsset).mockResolvedValue(asset({ loopRange: { start: 96_000, end: 192_000 } }));

    /** 上書きする（概要の長さは前後とも `durationMs`）。 */
    async function overwrite(
      before: [number, number],
      after: [number, number],
      durationMs: number,
    ) {
      await openAudioDirty({ durationMs });
      savingLengths(before, after);
      requestSave();
      await flush();
      savedRefresh();
      vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs })]);
      looping();
      act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
      await flush();
      await flush();
    }

    it('レート変換：480,000@48k → 441,000@44.1k（どちらも 10000 ms）でも知らせる', async () => {
      await overwrite([480_000, 48_000], [441_000, 44_100], 10_000);
      expect(getAsset).toHaveBeenCalledWith(7);
      expect(useNoticeStore.getState().message).toBe(t.audio.assetSettingsCheckAudio);
    });

    it('1 ms 未満の切り詰め：96,000 → 95,990（どちらも 2000 ms）でも知らせる', async () => {
      await overwrite([96_000, 48_000], [95_990, 48_000], 2000);
      expect(getAsset).toHaveBeenCalledWith(7);
      expect(useNoticeStore.getState().message).toBe(t.audio.assetSettingsCheckAudio);
    });

    it('フレームが同じでレートだけ変わっても知らせる', async () => {
      await overwrite([96_000, 48_000], [96_000, 44_100], 2000);
      expect(useNoticeStore.getState().message).toBe(t.audio.assetSettingsCheckAudio);
    });

    it('フレームとレートが同じなら、素材の設定を読まない', async () => {
      await overwrite([96_000, 48_000], [96_000, 48_000], 2000);
      expect(getAsset).not.toHaveBeenCalled();
      expect(useNoticeStore.getState().message).toBe('保存しました');
    });

    it('前後の長さが分からなければ（文書を描けない）、ms では比べず知らせない', async () => {
      await openAudioDirty({ durationMs: 2000 });
      requestSave();
      await flush();
      expect(useEditorStore.getState().save).toMatchObject({ before: null, after: null });
      savedRefresh();
      vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 1500 })]);
      looping();
      act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
      await flush();
      await flush();
      expect(getAsset).not.toHaveBeenCalled();
    });

    it('版に戻す：ms が同じでもフレームが変われば、開き直したあとに知らせる', async () => {
      await openAudioDirty({ durationMs: 2000 });
      savingLengths([95_990, 48_000], [96_000, 48_000]);
      act(() => confirmRevert(0));
      await flush();
      expect(saveEdit).toHaveBeenCalledWith('aa01', { type: 'revert', to: 0 }, null);
      vi.mocked(viewEdit).mockResolvedValue(
        result({
          status: status({ dirty: false, saving: null, needsReopen: true }),
          doc: { base: 2, order: [], nodes: [], outputs: null },
          view: { type: 'shown', view: viewDto() },
        }),
      );
      vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 2000 })]);
      vi.mocked(openEdit).mockClear();
      vi.mocked(openEdit).mockResolvedValue(
        opened({}, { status: status({ dirty: false }) }, 'wav'),
      );
      looping();
      act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
      await flush();
      await flush();
      await flush();
      expect(openEdit).toHaveBeenCalledTimes(1);
      expect(getAsset).toHaveBeenCalledWith(7);
      expect(useNoticeStore.getState().message).toBe(t.audio.assetSettingsCheckAudio);
    });

    it('版に戻す：フレームとレートが同じなら知らせない', async () => {
      await openAudioDirty({ durationMs: 2000 });
      savingLengths([96_000, 48_000], [96_000, 48_000]);
      act(() => confirmRevert(0));
      await flush();
      vi.mocked(viewEdit).mockResolvedValue(
        result({
          status: status({ dirty: false, saving: null, needsReopen: true }),
          doc: { base: 2, order: [], nodes: [], outputs: null },
          view: { type: 'shown', view: viewDto() },
        }),
      );
      vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 2000 })]);
      vi.mocked(openEdit).mockClear();
      vi.mocked(openEdit).mockResolvedValue(
        opened({}, { status: status({ dirty: false }) }, 'wav'),
      );
      looping();
      act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
      await flush();
      await flush();
      await flush();
      expect(openEdit).toHaveBeenCalledTimes(1);
      expect(getAsset).not.toHaveBeenCalled();
      expect(useNoticeStore.getState().message).toBe(t.reverted);
    });

    it('閉じたあとの保存：ms が同じでもフレームが変われば知らせる', async () => {
      await openAudioDirty({ durationMs: 10_000 });
      savingLengths([480_000, 48_000], [441_000, 44_100]);
      requestSave();
      await flush();
      await act(() => requestCloseEditor());
      expect(useEditorStore.getState().phase).toBe('closed');
      vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 10_000 })]);
      looping();
      act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
      await flush();
      await flush();
      expect(getAsset).toHaveBeenCalledWith(7);
      expect(useNoticeStore.getState().message).toBe(t.audio.assetSettingsCheckAudio);
    });

    it('閉じたあとの保存：フレームとレートが同じなら知らせない', async () => {
      await openAudioDirty({ durationMs: 2000 });
      savingLengths([96_000, 48_000], [96_000, 48_000]);
      requestSave();
      await flush();
      await act(() => requestCloseEditor());
      vi.mocked(getItems).mockResolvedValue([audioItem({ durationMs: 2000 })]);
      looping();
      act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
      await flush();
      await flush();
      expect(getAsset).not.toHaveBeenCalled();
      expect(useNoticeStore.getState().message).toBe(t.savedClosed('loop'));
    });
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

describe('窓を閉じる見張り（シート）', () => {
  it('保存していない変更がなくても、シートを出している間は閉じる操作を受けて止め、窓を残す', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
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
    const stop = watchWindowClose();
    try {
      vi.mocked(openEdit).mockResolvedValue(opened({}, { status: status({ dirty: false }) }));
      await act(() => openEditor(item()));
      await flush();
      expect(win.onCloseRequested).not.toHaveBeenCalled();
      act(() => openVersions());
      await flush();
      expect(win.onCloseRequested).toHaveBeenCalledTimes(1);
      const preventDefault = vi.fn();
      act(() => handler?.({ preventDefault }));
      await flush();
      expect(preventDefault).toHaveBeenCalled();
      expect(win.destroy).not.toHaveBeenCalled();
      expect(useEditorStore.getState().sheet).toEqual({ kind: 'versions' });
      expect(useEditorStore.getState().phase).toBe('open');
      // シートを閉じたら受け口を外す（未保存でなければ窓はそのまま閉じられる）
      act(() => dismissSheet());
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

describe('前の保存が終わっていないアイテムを開く（シートで止める。ADR-189）', () => {
  const savePending = () =>
    new IpcError({ code: 'SavePending', message: '前の保存（ジョブ 9）が終わっていません' });

  /** Rust の一覧の行（ジョブ 9、アイテム 7、一時停止）。 */
  function pending(over: Partial<EditPendingSaveDto> = {}): EditPendingSaveDto {
    return {
      job: 9,
      itemId: 7,
      itemTitle: 'hero',
      mode: { type: 'overwrite' },
      state: 'paused',
      progress: 0.42,
      message: null,
      ...over,
    };
  }

  /** 前の保存が待っているアイテムを開く（Rust は断り、一覧にその保存がある）。 */
  async function openHeld(row: EditPendingSaveDto | null = pending()) {
    vi.mocked(listPendingSaves).mockResolvedValue(row ? [row] : []);
    vi.mocked(openEdit).mockReset();
    vi.mocked(openEdit).mockRejectedValue(savePending());
    await act(() => openEditor(item()));
    await flush();
  }

  /** 前の保存が終わったあとの Rust（一覧にない。開ける）。 */
  function saveEnded(draft: Partial<Draft> = {}) {
    vi.mocked(listPendingSaves).mockResolvedValue([]);
    vi.mocked(openEdit).mockReset();
    vi.mocked(openEdit).mockResolvedValue(opened(draft));
  }

  it('シートを出して止める：ジョブの進捗と［再開］［中止］［閉じる］だけ。セッションはない', async () => {
    await openHeld();
    render(<EditorSheets />);
    const s = useEditorStore.getState();
    expect(s.phase).toBe('waiting');
    expect(s.sheet).toEqual({ kind: 'pendingSave' });
    expect(s.session).toBeNull();
    expect(screen.getByRole('heading', { name: t.pendingSaveTitle })).toBeInTheDocument();
    expect(screen.getByText(ja.savingJob.paused('hero'))).toBeInTheDocument();
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '42');
    const names = screen.getAllByRole('button').map((b) => b.textContent);
    expect(names.sort()).toEqual(
      [t.pendingSaveResume, t.pendingSaveCancel, ja.common.close].sort(),
    );
    fireEvent.click(screen.getByRole('button', { name: t.pendingSaveResume }));
    expect(resumeJob).toHaveBeenCalledWith(9);
    fireEvent.click(screen.getByRole('button', { name: t.pendingSaveCancel }));
    expect(cancelJob).toHaveBeenCalledWith(9);
    await act(() => closeEditor());
  });

  it('動いていれば［一時停止］。進捗を追い、止まったら［再開］に替わる', async () => {
    await openHeld(pending({ state: 'running', progress: null }));
    render(<EditorSheets />);
    fireEvent.click(screen.getByRole('button', { name: t.pendingSavePause }));
    expect(pauseJob).toHaveBeenCalledWith(9);
    act(() => onEditorJob(event('running', { progress: 0.5, message: '描いています' })));
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '50');
    expect(screen.getByText('描いています')).toBeInTheDocument();
    act(() => onEditorJob(event('paused', { progress: 0.5 })));
    expect(screen.getByRole('button', { name: t.pendingSaveResume })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: t.pendingSavePause })).toBeNull();
    expect(useEditorStore.getState().phase).toBe('waiting');
  });

  it('シートの間は手を受けない：手・取り消し・保存のどの口からも Rust に届かない', async () => {
    await openHeld();
    vi.mocked(stepEdit).mockClear();
    vi.mocked(saveEdit).mockClear();
    vi.mocked(discardEditDraft).mockClear();
    expect(pushEditorSteps([{ type: 'seal' }])).toBe(false);
    act(() => {
      editorUndo();
      editorRedo();
      requestSave();
      openSaveAs();
      openRevert();
      openVersions();
      refreshEditorView();
    });
    await flush();
    expect(stepEdit).not.toHaveBeenCalled();
    expect(saveEdit).not.toHaveBeenCalled();
    expect(viewEdit).not.toHaveBeenCalled();
    expect(discardEditDraft).not.toHaveBeenCalled();
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'pendingSave' });
    expect(useNoticeStore.getState().message).toBeNull();
  });

  it('閉じる要求は受けない（ADR-181）。窓を閉じる操作も止めて窓を残す', async () => {
    vi.mocked(isTauri).mockReturnValue(true);
    let handler: ((e: { preventDefault: () => void }) => void) | null = null;
    const win = {
      onCloseRequested: vi.fn(async (cb: (e: { preventDefault: () => void }) => void) => {
        handler = cb;
        return vi.fn();
      }),
      destroy: vi.fn(async () => undefined),
    };
    vi.mocked(getCurrentWindow).mockReturnValue(win as never);
    const stop = watchWindowClose();
    try {
      await openHeld();
      for (const then of ['window', 'library', 'none'] as const) {
        let answer: boolean | null = null;
        act(() => {
          void requestCloseEditor(then).then((v) => (answer = v));
        });
        await flush();
        expect(answer).toBe(false);
        expect(useEditorStore.getState().phase).toBe('waiting');
      }
      expect(win.onCloseRequested).toHaveBeenCalledTimes(1);
      const preventDefault = vi.fn();
      act(() => handler?.({ preventDefault }));
      await flush();
      expect(preventDefault).toHaveBeenCalled();
      expect(win.destroy).not.toHaveBeenCalled();
      expect(useEditorStore.getState().sheet).toEqual({ kind: 'pendingSave' });
    } finally {
      stop();
      vi.mocked(isTauri).mockReturnValue(false);
    }
  });

  it('［閉じる］・esc で閉じ、ジョブはそのままカプセルに残る（中止しない）', async () => {
    for (const close of [
      () => fireEvent.click(screen.getByRole('button', { name: ja.common.close })),
      () => fireEvent.keyDown(document, { key: 'Escape' }),
    ]) {
      await openHeld();
      const { unmount } = render(<EditorSheets />);
      vi.mocked(closeEdit).mockClear();
      vi.mocked(cancelJob).mockClear();
      close();
      await flush();
      expect(useEditorStore.getState().phase).toBe('closed');
      expect(closeEdit).not.toHaveBeenCalled();
      expect(cancelJob).not.toHaveBeenCalled();
      expect(shownSaves(useJobsStore.getState().saves).map((j) => j.id)).toEqual([9]);
      unmount();
      useJobsStore.setState({ saves: {}, endedSaves: {} });
    }
  });

  it('閉じたあとに終わっても開き直さない（知らせは出す）', async () => {
    await openHeld();
    await act(() => closeEditor());
    saveEnded();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    expect(openEdit).not.toHaveBeenCalled();
    expect(useEditorStore.getState().phase).toBe('closed');
    expect(useNoticeStore.getState().message).toBe(t.savedClosed('hero'));
  });

  it('再開して終わると開き直す：下書きが消えていれば（同じ文書）シートなしで開いて編集・保存できる', async () => {
    await openHeld();
    act(() => onEditorJob(event('queued')));
    expect(useEditorStore.getState().phase).toBe('waiting');
    act(() => onEditorJob(event('running', { progress: 0.5 })));
    saveEnded();
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    const s = useEditorStore.getState();
    expect(s.phase).toBe('open');
    expect(s.sheet).toBeNull();
    expect(useNoticeStore.getState().message).toBe(t.savedClosed('hero'));
    expect(openEdit).toHaveBeenCalledTimes(1);
    vi.mocked(saveEdit).mockClear();
    act(() => requestSave());
    await flush();
    expect(saveEdit).toHaveBeenCalledTimes(1);
  });

  it('再開して終わると開き直す：下書きが残っていればいつもの下書きのシート（Rust の fresh どおり）', async () => {
    await openHeld();
    saveEnded({ exists: true, fresh: true, updatedAt: 1000 });
    act(() => onEditorJob(event('done', { resultJson: '{"outcome":"saved","itemId":7}' })));
    await flush();
    render(<EditorSheets />);
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'draft' });
    expect(screen.getByRole('button', { name: t.draftOpen })).toBeInTheDocument();
    expect(discardEditDraft).not.toHaveBeenCalled();
  });

  it('失敗で終わっても開き直す（失敗は知らせる）', async () => {
    await openHeld();
    saveEnded({ exists: true, fresh: true, updatedAt: 1000 });
    act(() => onEditorJob(event('failed', { message: '書けません' })));
    await flush();
    expect(useEditorStore.getState().phase).toBe('open');
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'draft' });
    expect(useNoticeStore.getState().message).toBe(t.saveFailedClosed('hero', '書けません'));
  });

  it('中止すると開き直し（知らせない）、下書きから開ける', async () => {
    await openHeld();
    render(<EditorSheets />);
    saveEnded({ exists: true, fresh: true, updatedAt: 1000 });
    act(() => onEditorJob(event('canceled')));
    await flush();
    expect(useNoticeStore.getState().message).toBeNull();
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'draft' });
    vi.mocked(openEdit).mockResolvedValue(opened({ exists: true, fresh: true, opened: true }));
    fireEvent.click(screen.getByRole('button', { name: t.draftOpen }));
    await flush();
    expect(openEdit).toHaveBeenLastCalledWith(7, expect.anything(), true);
    expect(useEditorStore.getState().phase).toBe('open');
    expect(useEditorStore.getState().sheet).toBeNull();
  });

  it('開き直すときはアイテムの新しい概要で開く', async () => {
    await openHeld();
    saveEnded();
    vi.mocked(getItems).mockResolvedValueOnce([item({ width: 128 })]);
    act(() => onEditorJob(event('canceled')));
    await flush();
    expect(useEditorStore.getState().item?.width).toBe(128);
  });

  it('再起動のあと（画面の表が空）に開いても出る。ボタンの一言は Rust の一覧の値', async () => {
    useJobsStore.setState({ saves: {}, endedSaves: {} });
    await openHeld(pending({ job: 60, progress: null, itemTitle: 'long-60min' }));
    render(<EditorSheets />);
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'pendingSave' });
    expect(screen.getByText(ja.savingJob.paused('long-60min'))).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: t.pendingSaveResume }));
    expect(resumeJob).toHaveBeenCalledWith(60);
  });

  it('Busy の失敗のカードのやり直しでも出る（開くたびに Rust が確かめる）', async () => {
    vi.mocked(listPendingSaves).mockResolvedValue([pending()]);
    vi.mocked(openEdit).mockReset();
    vi.mocked(openEdit).mockRejectedValueOnce(
      new IpcError({ code: 'Busy', message: '混んでいます' }),
    );
    await act(() => openEditor(item()));
    await flush();
    expect(useEditorStore.getState().phase).toBe('error');
    vi.mocked(openEdit).mockRejectedValue(savePending());
    act(() => reopenEditor());
    await flush();
    expect(useEditorStore.getState().phase).toBe('waiting');
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'pendingSave' });
  });

  it('［下書きから開く］でも出る', async () => {
    await openDirty({ exists: true, fresh: true, updatedAt: 1000 });
    render(<EditorSheets />);
    vi.mocked(listPendingSaves).mockResolvedValue([pending()]);
    vi.mocked(openEdit).mockRejectedValue(savePending());
    fireEvent.click(screen.getByRole('button', { name: t.draftOpen }));
    await flush();
    expect(openEdit).toHaveBeenLastCalledWith(7, expect.anything(), true);
    expect(useEditorStore.getState().sheet).toEqual({ kind: 'pendingSave' });
  });

  it('断られたあとに聞くともう終わっていれば、すぐ開き直す', async () => {
    await openHeld(null);
    // 2 回目の開くも Rust が断った（この順は起きないはずの形）：繰り返さず、［閉じる］だけのシートで止まる
    expect(openEdit).toHaveBeenCalledTimes(2);
    expect(useEditorStore.getState().phase).toBe('waiting');
    await flush();
    expect(openEdit).toHaveBeenCalledTimes(2);
    render(<EditorSheets />);
    expect(screen.getAllByRole('button').map((b) => b.textContent)).toEqual([ja.common.close]);
    await act(() => closeEditor());

    // ふつうの形：2 回目は開ける
    vi.mocked(listPendingSaves).mockResolvedValue([]);
    vi.mocked(openEdit).mockReset();
    vi.mocked(openEdit).mockRejectedValueOnce(savePending());
    vi.mocked(openEdit).mockResolvedValueOnce(opened());
    await act(() => openEditor(item()));
    await flush();
    expect(openEdit).toHaveBeenCalledTimes(2);
    expect(useEditorStore.getState().phase).toBe('open');
  });

  it('同じエディタで積んだ保存は今のまま：閉じずに編集を続けられ、シートを出さない', async () => {
    await openDirty();
    requestSave();
    await flush();
    act(() => onEditorJob(event('paused', { progress: 0.4 })));
    expect(useEditorStore.getState().sheet).toBeNull();
    expect(useEditorStore.getState().phase).toBe('open');
    vi.mocked(stepEdit).mockClear();
    expect(pushEditorSteps([{ type: 'seal' }])).toBe(true);
    await flush();
    expect(stepEdit).toHaveBeenCalledTimes(1);
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
