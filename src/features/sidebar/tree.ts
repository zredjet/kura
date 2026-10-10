import type { FolderNodeDto } from '../../ipc/folders';

/** サイドバーに出すフォルダの 1 行。 */
export interface TreeRow {
  node: FolderNodeDto;
  depth: number;
  hasChildren: boolean;
  expanded: boolean;
}

/** 親 ID（いちばん上は null）→ 子フォルダ（並び順）。 */
export function childrenMap(nodes: readonly FolderNodeDto[]): Map<number | null, FolderNodeDto[]> {
  const map = new Map<number | null, FolderNodeDto[]>();
  for (const node of nodes) {
    const list = map.get(node.parentId) ?? [];
    list.push(node);
    map.set(node.parentId, list);
  }
  for (const list of map.values()) {
    list.sort((a, b) => (a.pos ?? 0) - (b.pos ?? 0) || a.id - b.id);
  }
  return map;
}

/** 開いているフォルダの中だけをたどった、表示する行の並び。 */
export function visibleRows(
  nodes: readonly FolderNodeDto[],
  isExpanded: (id: number) => boolean,
): TreeRow[] {
  const children = childrenMap(nodes);
  const rows: TreeRow[] = [];
  const seen = new Set<number>();
  const walk = (parent: number | null, depth: number) => {
    for (const node of children.get(parent) ?? []) {
      if (seen.has(node.id)) {
        continue;
      }
      seen.add(node.id);
      const hasChildren = (children.get(node.id)?.length ?? 0) > 0;
      const expanded = hasChildren && isExpanded(node.id);
      rows.push({ node, depth, hasChildren, expanded });
      if (expanded) {
        walk(node.id, depth + 1);
      }
    }
  };
  walk(null, 0);
  return rows;
}

/** いちばん上からのパス（名前の列）。 */
export function pathOf(nodes: readonly FolderNodeDto[], id: number): string[] {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const path: string[] = [];
  let current = byId.get(id);
  while (current && path.length <= nodes.length) {
    path.unshift(current.name);
    current = current.parentId === null ? undefined : byId.get(current.parentId);
  }
  return path;
}

/** そのフォルダと子孫フォルダの ID。 */
export function descendantsOf(nodes: readonly FolderNodeDto[], id: number): Set<number> {
  const children = childrenMap(nodes);
  const out = new Set<number>([id]);
  const stack = [id];
  while (stack.length > 0) {
    const current = stack.pop() as number;
    for (const child of children.get(current) ?? []) {
      if (!out.has(child.id)) {
        out.add(child.id);
        stack.push(child.id);
      }
    }
  }
  return out;
}

/** 行の上に落としたときの位置（上の端・中・下の端）。 */
export type DropPosition = 'before' | 'inside' | 'after';

/** 行の中の縦位置（0〜1）から落とす位置を決める（上下 25% は並べ替え、真ん中は中へ）。 */
export function dropPositionAt(ratio: number): DropPosition {
  if (ratio < 0.25) {
    return 'before';
  }
  if (ratio > 0.75) {
    return 'after';
  }
  return 'inside';
}

/** フォルダを `target` の位置に落としたときの移動先。自分自身や子孫の中へは null。 */
export function moveTarget(
  nodes: readonly FolderNodeDto[],
  dragged: number,
  target: number,
  position: DropPosition,
): { parentId: number | null; beforeId: number | null } | null {
  if (descendantsOf(nodes, dragged).has(target)) {
    return null;
  }
  if (position === 'inside') {
    return { parentId: target, beforeId: null };
  }
  const node = nodes.find((n) => n.id === target);
  if (!node) {
    return null;
  }
  const siblings = (childrenMap(nodes).get(node.parentId) ?? []).filter((n) => n.id !== dragged);
  if (position === 'before') {
    return { parentId: node.parentId, beforeId: target };
  }
  const index = siblings.findIndex((n) => n.id === target);
  return { parentId: node.parentId, beforeId: siblings[index + 1]?.id ?? null };
}
