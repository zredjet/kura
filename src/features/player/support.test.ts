import { describe, expect, it } from 'vitest';

import { buildSupportTable, PROBES } from './support';

describe('buildSupportTable', () => {
  it('canPlayType が空でなければ再生できるとみなす', () => {
    const answers: Record<string, string> = {
      'video/mp4; codecs="avc1.640028"': 'probably',
      'audio/ogg; codecs="vorbis"': '',
      'audio/flac': 'maybe',
    };
    const table = buildSupportTable((mime) => answers[mime] ?? '');
    expect(table).toHaveLength(PROBES.length);
    const playable = table.filter((e) => e.playable).map((e) => `${e.container}/${e.codec}`);
    expect(playable).toEqual(['mp4/h264', 'flac/flac']);
  });

  it('組は重複しない', () => {
    const keys = PROBES.map((p) => `${p.container}/${p.codec}`);
    expect(new Set(keys).size).toBe(keys.length);
  });
});
