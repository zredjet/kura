import { beforeEach, describe, expect, it } from 'vitest';

import type { JobProgress } from '../ipc/jobs';
import { selectCurrentImport, useJobsStore } from './jobsStore';

function event(over: Partial<JobProgress>): JobProgress {
  return {
    id: 1,
    kind: 'import',
    state: 'running',
    progress: null,
    message: null,
    resultJson: null,
    ...over,
  };
}

beforeEach(() => {
  useJobsStore.setState({ imports: {}, dismissed: {} });
});

describe('jobsStore', () => {
  it('取り込みのイベントだけを、前の進捗を引き継いで反映する', () => {
    const { apply } = useJobsStore.getState();
    apply(event({ progress: 0.4, message: '4 / 10 項目' }));
    apply(event({ state: 'paused' }));
    apply(event({ id: 2, kind: 'thumbnail' }));
    const job = useJobsStore.getState().imports[1];
    expect(job).toMatchObject({ state: 'paused', progress: 0.4, message: '4 / 10 項目' });
    expect(useJobsStore.getState().imports[2]).toBeUndefined();
  });

  it('消していないもののうち新しいものを出す', () => {
    const { apply, dismiss } = useJobsStore.getState();
    apply(event({ id: 3, state: 'done' }));
    apply(event({ id: 5, state: 'queued' }));
    expect(selectCurrentImport(useJobsStore.getState())?.id).toBe(5);
    dismiss(5);
    expect(selectCurrentImport(useJobsStore.getState())?.id).toBe(3);
    dismiss(3);
    expect(selectCurrentImport(useJobsStore.getState())).toBeNull();
  });
});
