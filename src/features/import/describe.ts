import { ja } from '../../i18n/ja';
import type { FolderNodeDto } from '../../ipc/folders';
import type { ImportPreviewDto } from '../../ipc/import';
import {
  countsFromPlural,
  isPlayable,
  type ItemKind,
  KIND_ORDER,
  kindLabel,
} from '../../ipc/kinds';
import { formatCount } from '../../lib/format';

const t = ja.import;

/** 見出しの下の一行（「旅行2026 ほか 2 フォルダ · サブフォルダ 6」）。 */
export function describeSources(preview: ImportPreviewDto): string {
  const [first, ...rest] = preview.roots;
  const parts: string[] = [];
  if (first) {
    if (rest.length === 0) {
      parts.push(first.name);
    } else {
      const unit = rest.every((r) => r.isDir)
        ? t.sources.folders
        : rest.every((r) => !r.isDir)
          ? t.sources.files
          : t.sources.items;
      parts.push(t.sources.others(first.name, formatCount(rest.length), unit));
    }
  }
  if (preview.subfolders > 0) {
    parts.push(t.sources.subfolders(formatCount(preview.subfolders)));
  }
  if (preview.skipped > 0) {
    parts.push(t.sources.skipped(formatCount(preview.skipped)));
  }
  return parts.join(' · ');
}

/**
 * 種類別の件数の欄（Import.dc.html）：画像・動画・音楽は 0 件でも出し（モックの並び）、
 * フォント・3D は 1 件以上のときだけ出す（design.md §3.1）。
 */
export function kindStats(
  preview: ImportPreviewDto,
): { kind: ItemKind; label: string; count: number }[] {
  const counts = countsFromPlural(preview);
  return KIND_ORDER.filter((kind) => isPlayable(kind) || counts[kind] > 0).map((kind) => ({
    kind,
    label: kindLabel(kind),
    count: counts[kind],
  }));
}

/** 取り込むボタンの文言（重複の見込みを除いた数）。 */
export function confirmLabel(preview: ImportPreviewDto): string {
  const count = Math.max(0, preview.total - preview.duplicates);
  return count > 0 ? t.confirm(formatCount(count)) : t.confirmPlain;
}

/** 「フォルダ構成の再現」の例（「旅行2026 / 鎌倉 → 写真 / 2026 旅行 / 鎌倉」）。 */
export function reproduceExample(
  sampleDirs: readonly string[],
  destination: readonly string[] | null,
): string {
  if (sampleDirs.length === 0) {
    return '';
  }
  const target = destination ? [...destination, ...sampleDirs.slice(1)] : [...sampleDirs];
  return t.arrow(t.folderPath(sampleDirs), t.folderPath(target));
}

/** 「フォルダ名をタグに」の例（「旅行2026 / 鎌倉 → タグ「旅行2026」「鎌倉」」）。 */
export function folderTagExample(sampleDirs: readonly string[]): string {
  if (sampleDirs.length === 0) {
    return '';
  }
  return t.arrow(t.folderPath(sampleDirs), t.tagExample(sampleDirs));
}

/** 「フォルダごとにユニット」の例（「鎌倉 → ユニット「鎌倉」」）。 */
export function unitExample(sampleDirs: readonly string[]): string {
  const leaf = sampleDirs[sampleDirs.length - 1];
  return leaf ? t.arrow(leaf, t.unitExample(leaf)) : t.unitsHint;
}

/** 仮想フォルダの選択肢（木の順に、「写真 / 2026 旅行」の形）。 */
export function folderChoices(nodes: readonly FolderNodeDto[]): { id: number; path: string[] }[] {
  const children = new Map<number | null, FolderNodeDto[]>();
  for (const node of nodes) {
    const list = children.get(node.parentId) ?? [];
    list.push(node);
    children.set(node.parentId, list);
  }
  for (const list of children.values()) {
    list.sort((a, b) => (a.pos ?? 0) - (b.pos ?? 0) || a.id - b.id);
  }
  const out: { id: number; path: string[] }[] = [];
  const walk = (parent: number | null, path: string[]) => {
    for (const node of children.get(parent) ?? []) {
      const here = [...path, node.name];
      out.push({ id: node.id, path: here });
      walk(node.id, here);
    }
  };
  walk(null, []);
  return out;
}
