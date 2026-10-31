import { describe, expect, it } from 'vitest';

import formatsRs from '../../../../crates/kura-engine/src/codec/formats.rs?raw';

import { AUDIO_CAPS, AUDIO_FORMAT_EXTS, AUDIO_FORMATS, capsOf, lossesFor } from './audioFormats';

/** エンジンの形式の表の行（拡張子と、行の本文）を読む。 */
function engineRow(id: string): { exts: string[]; body: string } | null {
  const at = formatsRs.indexOf(`id: "${id}",`);
  if (at < 0) return null;
  const next = formatsRs.indexOf('FormatDesc {', at);
  const body = formatsRs.slice(at, next < 0 ? undefined : next);
  const exts = /exts: &\[([^\]]*)\]/.exec(body)?.[1] ?? '';
  return {
    exts: exts
      .split(',')
      .map((e) => e.trim().replace(/"/g, ''))
      .filter(Boolean),
    body,
  };
}

describe('音声の形式の表', () => {
  it('拡張子と書ける印はエンジンの形式の表の音声の行と同じ', () => {
    for (const [id, exts] of Object.entries(AUDIO_FORMAT_EXTS)) {
      const row = engineRow(id);
      expect(row, id).not.toBeNull();
      expect(row?.exts, id).toEqual(exts);
      const caps = AUDIO_CAPS[id];
      const body = row?.body ?? '';
      expect(/lossless: true/.test(body), `${id} lossless`).toBe(caps?.lossless);
      expect(/high_bit_depth: true/.test(body), `${id} high_bit_depth`).toBe(caps?.highBitDepth);
      expect(/loop_points: true/.test(body), `${id} loop_points`).toBe(caps?.loopPoints);
      expect(/markers: true/.test(body), `${id} markers`).toBe(caps?.markers);
    }
  });

  it('拡張子から形式・書ける形式・書けないときは Ogg Vorbis', () => {
    expect(AUDIO_FORMATS.formatOfExt('AIFF')).toBe('aiff');
    expect(AUDIO_FORMATS.formatOfExt('aif')).toBe('aiff');
    expect(AUDIO_FORMATS.extOfFormat('aiff')).toBe('aif');
    expect(AUDIO_FORMATS.canWrite('mp3')).toBe(true);
    expect(AUDIO_FORMATS.canWrite('opus')).toBe(false);
    expect(AUDIO_FORMATS.effectiveFormat({ type: 'keep' }, 'opus')).toEqual({
      format: 'ogg',
      forced: true,
    });
    expect(AUDIO_FORMATS.exportName('loop.wav', 'ogg')).toBe('loop.ogg');
  });

  it('書くのに ffmpeg が要るのは MP3 だけ（Rust の ffmpeg の書き手が書く形式。M4A は OS の AAC）', () => {
    expect(AUDIO_FORMATS.needsFfmpeg('mp3')).toBe(true);
    expect(AUDIO_FORMATS.needsFfmpeg('m4a')).toBe(false);
    expect(AUDIO_FORMATS.needsFfmpeg('ogg')).toBe(false);
    expect(AUDIO_FORMATS.needsFfmpeg(null)).toBe(false);
  });
});

describe('保存で失うもの', () => {
  const view = { loop: { start: 0, end: 10 }, markers: [{ pos: 1, name: 'a' }], bits: 24 };

  it('MP3 はループ点・マーカーを書けず、非可逆', () => {
    expect(lossesFor(capsOf('mp3'), view)).toEqual(['loop', 'markers', 'lossy']);
  });

  it('Ogg Vorbis はループ点を書けるが非可逆、FLAC はマーカーだけ、WAV は何も失わない', () => {
    expect(lossesFor(capsOf('ogg'), view)).toEqual(['markers', 'lossy']);
    expect(lossesFor(capsOf('flac'), view)).toEqual(['markers']);
    expect(lossesFor(capsOf('wav'), view)).toEqual([]);
  });

  it('ループ・マーカーがなければ数えない・知らない形式は空・高いビットを書けない可逆の形式', () => {
    expect(lossesFor(capsOf('mp3'), { loop: null, markers: [], bits: 16 })).toEqual(['lossy']);
    expect(lossesFor(capsOf('xyz'), view)).toEqual([]);
    expect(
      lossesFor(
        { lossless: true, highBitDepth: false, loopPoints: true, markers: true },
        { loop: null, markers: [], bits: 24 },
      ),
    ).toEqual(['bits']);
  });
});
