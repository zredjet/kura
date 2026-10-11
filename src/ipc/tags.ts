import {
  commands,
  type TagDetailDto,
  type TagListEntryDto,
  type TagPatchDto,
  type TagRefDto,
  type TagSuggestionDto,
} from './bindings';
import { unwrap } from './result';

export type { TagDetailDto, TagListEntryDto, TagPatchDto, TagRefDto, TagSuggestionDto };

/** すべてのタグと、付いている数（名前空間なしを後ろに、名前空間ごとに付いている数の多い順）。 */
export function listTags(): Promise<TagListEntryDto[]> {
  return unwrap(commands.tagsList());
}

/** 入力に合うタグの候補。 */
export function suggestTags(prefix: string, limit = 8): Promise<TagSuggestionDto[]> {
  return unwrap(commands.tagsSuggest(prefix, limit));
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
