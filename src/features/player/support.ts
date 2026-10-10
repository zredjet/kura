import type { ContainerDto } from '../../ipc/bindings';
import { setPlaybackSupport, type SupportEntryDto } from '../../ipc/player';

/** 調べる組（入れ物 × ffprobe のコーデック名 → canPlayType に渡す MIME）。docs/architecture.md §8。 */
export const PROBES: readonly { container: ContainerDto; codec: string; mime: string }[] = [
  // mp4・mov・m4a（QuickTime 系）
  { container: 'mp4', codec: 'h264', mime: 'video/mp4; codecs="avc1.640028"' },
  { container: 'mp4', codec: 'hevc', mime: 'video/mp4; codecs="hvc1.1.6.L123.B0"' },
  { container: 'mp4', codec: 'av1', mime: 'video/mp4; codecs="av01.0.08M.08"' },
  { container: 'mp4', codec: 'vp9', mime: 'video/mp4; codecs="vp09.00.40.08"' },
  { container: 'mp4', codec: 'mpeg4', mime: 'video/mp4; codecs="mp4v.20.9"' },
  { container: 'mp4', codec: 'aac', mime: 'audio/mp4; codecs="mp4a.40.2"' },
  { container: 'mp4', codec: 'mp3', mime: 'audio/mp4; codecs="mp4a.6B"' },
  { container: 'mp4', codec: 'alac', mime: 'audio/mp4; codecs="alac"' },
  { container: 'mp4', codec: 'flac', mime: 'audio/mp4; codecs="flac"' },
  { container: 'mp4', codec: 'opus', mime: 'audio/mp4; codecs="opus"' },
  { container: 'mp4', codec: 'ac3', mime: 'audio/mp4; codecs="ac-3"' },
  { container: 'mp4', codec: 'eac3', mime: 'audio/mp4; codecs="ec-3"' },
  // webm
  { container: 'webm', codec: 'vp8', mime: 'video/webm; codecs="vp8"' },
  { container: 'webm', codec: 'vp9', mime: 'video/webm; codecs="vp9"' },
  { container: 'webm', codec: 'av1', mime: 'video/webm; codecs="av01.0.08M.08"' },
  { container: 'webm', codec: 'opus', mime: 'audio/webm; codecs="opus"' },
  { container: 'webm', codec: 'vorbis', mime: 'audio/webm; codecs="vorbis"' },
  // ogg
  { container: 'ogg', codec: 'opus', mime: 'audio/ogg; codecs="opus"' },
  { container: 'ogg', codec: 'vorbis', mime: 'audio/ogg; codecs="vorbis"' },
  { container: 'ogg', codec: 'flac', mime: 'audio/ogg; codecs="flac"' },
  { container: 'ogg', codec: 'theora', mime: 'video/ogg; codecs="theora"' },
  // 単体の音声
  { container: 'mp3', codec: 'mp3', mime: 'audio/mpeg' },
  { container: 'flac', codec: 'flac', mime: 'audio/flac' },
  { container: 'wav', codec: 'pcm', mime: 'audio/wav; codecs="1"' },
  { container: 'aiff', codec: 'pcm', mime: 'audio/aiff' },
  { container: 'aac', codec: 'aac', mime: 'audio/aac' },
];

/** 再生可否の表を作る（`canPlay` は HTMLMediaElement.canPlayType。"maybe" も再生できるとみなす）。 */
export function buildSupportTable(canPlay: (mime: string) => string): SupportEntryDto[] {
  return PROBES.map(({ container, codec, mime }) => ({
    container,
    codec,
    playable: canPlay(mime) !== '',
  }));
}

/** 起動時に表を作って Rust に渡す。 */
export async function reportPlaybackSupport(): Promise<void> {
  const probe = document.createElement('video');
  await setPlaybackSupport(buildSupportTable((mime) => probe.canPlayType(mime)));
}
