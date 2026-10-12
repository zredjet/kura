import { describe, expect, it } from 'vitest';

import type { DownloadJobDto } from '../ipc/downloads';
import type { JobProgress } from '../ipc/jobs';
import { countJobs, filterJobs, mergeEvent, useDownloadsStore } from './downloadsStore';

function job(
  id: number,
  state: DownloadJobDto['state'],
  finishedAt: number | null = null,
): DownloadJobDto {
  return {
    id,
    state,
    progress: null,
    message: null,
    errorCode: null,
    url: `https://example.org/${id}`,
    kind: 'download',
    siteLabel: 'YouTube',
    toolLabel: 'yt-dlp',
    ruleName: null,
    audioOnly: false,
    redownload: false,
    title: null,
    files: 0,
    added: 0,
    duplicates: 0,
    skipped: 0,
    tags: 0,
    itemIds: [],
    unitIds: [],
    createdAt: 0,
    finishedAt,
  };
}

function event(id: number, patch: Partial<JobProgress>): JobProgress {
  return {
    id,
    kind: 'download',
    state: 'running',
    progress: null,
    message: null,
    resultJson: null,
    ...patch,
  };
}

describe('useDownloadsStore.apply', () => {
  it('follows download and capture jobs but not others', () => {
    useDownloadsStore.setState({
      jobs: [job(1, 'running'), { ...job(2, 'running'), kind: 'capture' }],
    });
    const apply = useDownloadsStore.getState().apply;
    apply(event(2, { kind: 'capture', progress: 0.5 }));
    apply(event(1, { kind: 'import', progress: 0.9 }));
    const [a, b] = useDownloadsStore.getState().jobs;
    expect([a?.progress, b?.progress]).toEqual([null, 0.5]);
  });
});

describe('mergeEvent', () => {
  it('keeps values the event does not carry', () => {
    const a = mergeEvent(job(1, 'queued'), event(1, { progress: 0.4, message: '4 / 10 枚' }));
    expect([a.state, a.progress, a.message]).toEqual(['running', 0.4, '4 / 10 枚']);
    const b = mergeEvent(a, event(1, { resultJson: '{"title":"作品","added":2,"itemIds":[5,6]}' }));
    expect([b.progress, b.message, b.title, b.added, b.itemIds]).toEqual([
      0.4,
      '4 / 10 枚',
      '作品',
      2,
      [5, 6],
    ]);
    const c = mergeEvent(b, event(1, { state: 'done', resultJson: 'not json' }));
    expect([c.state, c.title]).toEqual(['done', '作品']);
  });
});

describe('filterJobs and countJobs', () => {
  const now = new Date(2026, 9, 11, 15, 0);
  const today = new Date(2026, 9, 11, 9, 0).getTime();
  const yesterday = new Date(2026, 9, 10, 23, 0).getTime();
  const jobs = [
    job(1, 'running'),
    job(2, 'queued'),
    job(3, 'paused'),
    job(4, 'done', today),
    job(5, 'done', yesterday),
    job(6, 'failed'),
    job(7, 'canceled'),
  ];

  it('filters by tab', () => {
    const ids = (tab: Parameters<typeof filterJobs>[1]) => filterJobs(jobs, tab).map((j) => j.id);
    expect(ids('all')).toEqual([1, 2, 3, 4, 5, 6, 7]);
    expect(ids('active')).toEqual([1, 2, 3]);
    expect(ids('done')).toEqual([4, 5]);
    expect(ids('failed')).toEqual([6, 7]);
  });

  it('counts the header numbers', () => {
    expect(countJobs(jobs, now)).toEqual({ active: 1, queued: 2, doneToday: 1, failed: 1 });
  });
});
