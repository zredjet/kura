import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import type { JobProgress } from '../../ipc/jobs';
import { pauseJob } from '../../ipc/jobs';
import { libraryInfo } from '../../ipc/library';
import { useJobsStore } from '../../stores/jobsStore';
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
  vi.mocked(libraryInfo).mockClear();
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
