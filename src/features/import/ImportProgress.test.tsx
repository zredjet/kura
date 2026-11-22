import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ja } from '../../i18n/ja';
import type { JobProgress } from '../../ipc/jobs';
import { cancelJob, pauseJob, resumeJob } from '../../ipc/jobs';
import { libraryInfo } from '../../ipc/library';
import { SAVE_CAPSULE_DELAY_MS, type SaveJob, useJobsStore } from '../../stores/jobsStore';
import { ImportProgress } from './ImportProgress';

vi.mock('../../ipc/jobs', () => ({
  pauseJob: vi.fn(async () => null),
  resumeJob: vi.fn(async () => null),
  cancelJob: vi.fn(async () => null),
}));
vi.mock('../../ipc/library', () => ({
  libraryInfo: vi.fn(async () => ({ name: 'L', path: '/L', itemCount: 3, integrity: null })),
}));

function show(job: Partial<JobProgress>) {
  useJobsStore.setState({
    imports: {
      9: {
        id: 9,
        kind: 'import',
        state: 'running',
        progress: 0.3,
        message: '38 / 122 項目',
        resultJson: null,
        ...job,
      },
    },
    dismissed: {},
  });
}

beforeEach(() => {
  vi.mocked(pauseJob).mockClear();
  vi.mocked(resumeJob).mockClear();
  vi.mocked(cancelJob).mockClear();
  vi.mocked(libraryInfo).mockClear();
  useJobsStore.setState({ imports: {}, dismissed: {}, saves: {}, endedSaves: {} });
});

describe('ImportProgress', () => {
  it('実行中は進捗と一時停止・中止を出す', () => {
    show({});
    render(<ImportProgress />);
    expect(screen.getByRole('status')).toHaveTextContent('取り込み中');
    expect(screen.getByText('38 / 122 項目')).toBeInTheDocument();
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '30');
    fireEvent.click(screen.getByRole('button', { name: '一時停止' }));
    expect(pauseJob).toHaveBeenCalledWith(9);
    expect(screen.getByRole('button', { name: '中止' })).toBeInTheDocument();
  });

  it('完了したら件数を読み直し、閉じるで消える', () => {
    show({ state: 'done', resultJson: JSON.stringify({ added: 122, duplicates: 6, failed: 0 }) });
    render(<ImportProgress />);
    expect(screen.getByRole('status')).toHaveTextContent('122 項目を取り込みました');
    expect(libraryInfo).toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: '閉じる' }));
    expect(screen.queryByRole('status')).not.toBeInTheDocument();
  });
});

describe('ImportProgress：保存のジョブ', () => {
  function track(over: Partial<Omit<SaveJob, 'shown'>> = {}) {
    useJobsStore.getState().trackSave({
      id: 21,
      itemId: 7,
      itemTitle: 'long-60min',
      mode: 'overwrite',
      newTitle: null,
      state: 'running',
      progress: 0.42,
      message: '描いています',
      ...over,
    });
  }
  const edit = (over: Partial<JobProgress>): JobProgress => ({
    id: 21,
    kind: 'edit',
    state: 'running',
    progress: null,
    message: null,
    resultJson: null,
    ...over,
  });
  const apply = (event: JobProgress) => act(() => useJobsStore.getState().apply(event));
  const wait = (ms: number) => act(() => vi.advanceTimersByTime(ms));

  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('一瞬で終わる保存では出さない（1 秒たつ前に終わる）', () => {
    const { container } = render(<ImportProgress />);
    act(() => track());
    expect(container).toBeEmptyDOMElement();
    wait(SAVE_CAPSULE_DELAY_MS - 1);
    expect(container).toBeEmptyDOMElement();
    apply(edit({ state: 'done' }));
    wait(SAVE_CAPSULE_DELAY_MS);
    expect(container).toBeEmptyDOMElement();
  });

  it('1 秒動いたら名前・進捗を出し、一時停止・中止を押すとジョブを止める', () => {
    render(<ImportProgress />);
    act(() => track());
    wait(SAVE_CAPSULE_DELAY_MS);
    expect(screen.getByRole('group', { name: ja.savingJob.label })).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('「long-60min」を保存しています');
    expect(screen.getByText('描いています')).toBeInTheDocument();
    expect(screen.getByRole('progressbar')).toHaveAttribute('aria-valuenow', '42');
    fireEvent.click(screen.getByRole('button', { name: ja.progress.pause }));
    expect(pauseJob).toHaveBeenCalledWith(21);
    fireEvent.click(screen.getByRole('button', { name: ja.progress.cancel }));
    expect(cancelJob).toHaveBeenCalledWith(21);
  });

  it('一時停止中はすぐ出し、再開を押すとジョブを再開する（アプリを開き直したあとも）', () => {
    render(<ImportProgress />);
    act(() => track({ state: 'paused', progress: null }));
    expect(screen.getByRole('status')).toHaveTextContent(
      '「long-60min」の保存を一時停止しています',
    );
    expect(screen.getByText(ja.savingJob.resumeNote)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: ja.progress.pause })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: ja.progress.resume }));
    expect(resumeJob).toHaveBeenCalledWith(21);
    fireEvent.click(screen.getByRole('button', { name: ja.progress.cancel }));
    expect(cancelJob).toHaveBeenCalledWith(21);
  });

  it('終わったら完了の状態を出さずに消す（完了・失敗・中止）', () => {
    const { container } = render(<ImportProgress />);
    for (const state of ['done', 'failed', 'canceled'] as const) {
      act(() => track({ state: 'paused' }));
      expect(screen.getByRole('status')).toBeInTheDocument();
      apply(edit({ state }));
      expect(container).toBeEmptyDOMElement();
      useJobsStore.setState({ endedSaves: {} });
    }
  });

  it('取り込みのカプセルと並べて出す（どちらも押せる）', () => {
    show({});
    render(<ImportProgress />);
    act(() => track({ state: 'paused' }));
    const saving = screen.getByRole('group', { name: ja.savingJob.label });
    const importing = screen.getByRole('group', { name: ja.progress.label });
    fireEvent.click(within(saving).getByRole('button', { name: ja.progress.resume }));
    expect(resumeJob).toHaveBeenCalledWith(21);
    fireEvent.click(within(importing).getByRole('button', { name: ja.progress.pause }));
    expect(pauseJob).toHaveBeenCalledWith(9);
  });

  it('待っている保存が多くても 3 つまで（古い順。終わると次が出る）', () => {
    render(<ImportProgress />);
    act(() => {
      for (const id of [21, 22, 23, 24]) track({ id, state: 'paused' });
    });
    expect(screen.getAllByRole('group')).toHaveLength(3);
    apply(edit({ id: 21, state: 'canceled' }));
    expect(screen.getAllByRole('group')).toHaveLength(3);
    apply(edit({ id: 22, state: 'canceled' }));
    expect(screen.getAllByRole('group')).toHaveLength(2);
  });

  it('ミニプレーヤーがあれば、その上に積む', () => {
    const { container } = render(<ImportProgress raised />);
    act(() => track({ state: 'paused' }));
    expect(container.firstElementChild).toHaveAttribute('data-raised');
  });
});
