import {
  commands,
  type TagDetailDto,
  type TagPatchDto,
  type TagRefDto,
  type TagSuggestionDto,
} from './bindings';
import { unwrap } from './result';

export type { TagDetailDto, TagPatchDto, TagRefDto, TagSuggestionDto };

/** 入力に合うタグの候補。 */
export function suggestTags(prefix: string, limit = 8): Promise<TagSuggestionDto[]> {
  return unwrap(commands.tagsSuggest(prefix, limit));
}

/** アイテムに `add`（名前。なければ作る）を付け、`remove`（ID）を外す。 */
export function applyTags(itemIds: number[], add: string[], remove: number[]): Promise<number[]> {
  return unwrap(commands.tagsApply(itemIds, add, remove));
}

/** タグの編集画面の内容。 */
export function tagDetail(id: number): Promise<TagDetailDto> {
  return unwrap(commands.tagDetail(id));
}

/** 名前空間・名前・色・別名・親タグを変える。 */
export function updateTag(id: number, patch: TagPatchDto): Promise<null> {
  return unwrap(commands.tagUpdate(id, patch));
}

/** `from` を `into` に統合する。 */
export function mergeTag(from: number, into: number): Promise<null> {
  return unwrap(commands.tagMerge(from, into));
}

/** タグを消す。 */
export function deleteTag(id: number): Promise<null> {
  return unwrap(commands.tagDelete(id));
}

/** 「名前空間:名前」の表記。 */
export function tagLabel(tag: { namespace: string; name: string }): string {
  return tag.namespace ? `${tag.namespace}:${tag.name}` : tag.name;
}
