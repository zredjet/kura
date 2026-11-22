import { describe, expect, it, vi } from 'vitest';

import formatsRs from '../../crates/kura-engine/src/codec/formats.rs?raw';
import { MEDIA_EXTENSIONS } from './dialogs';

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

/** kura-engine の形式の表（`FORMATS`）の拡張子。取り込める拡張子の正本（docs/architecture.md §6）。 */
function engineExtensions(): string[] {
  const table = formatsRs.slice(formatsRs.indexOf('pub const FORMATS'));
  const exts: string[] = [];
  for (const m of table.matchAll(/exts:\s*&\[([^\]]*)\]/g)) {
    for (const e of (m[1] ?? '').matchAll(/"([^"]+)"/g)) {
      exts.push(e[1] ?? '');
    }
  }
  return exts;
}

describe('取り込むファイルを選ぶダイアログ', () => {
  it('選べる拡張子は形式の表と同じ（フォント・3D・素材の画像を含む）', () => {
    const engine = engineExtensions();
    expect(engine).toContain('ttf');
    expect(engine).toContain('glb');
    expect([...MEDIA_EXTENSIONS].sort()).toEqual([...engine].sort());
  });
});
