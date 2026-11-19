import {
  type Binding_Deserialize,
  commands,
  type NodeId,
  type RecipeDto_Serialize,
  type RecipeImportedDto,
  type RecipeNote,
  type RecipeSummaryDto,
} from './bindings';
import { unwrap } from './result';

/** 1 件のレシピ（文書・結び付け・旧版の印）。 */
export type RecipeDto = RecipeDto_Serialize;
/** 作る・差し替えるときに渡す結び付け。 */
export type RecipeBinding = Binding_Deserialize;

export type { RecipeImportedDto, RecipeNote, RecipeSummaryDto };

/** レシピの一覧（サイドバーの並びの順）。 */
export function listRecipes(): Promise<RecipeSummaryDto[]> {
  return unwrap(commands.recipesList());
}

/** 1 件のレシピ。 */
export function getRecipe(id: number): Promise<RecipeDto> {
  return unwrap(commands.recipeGet(id));
}

/** エディタで選んだもの（含めるノード・出力の形式を含めるか・結び付け）。 */
export interface RecipePick {
  /** エディタのセッション。 */
  session: string;
  /** 含めるノード（エディタの文書のノードの ID）。 */
  keep: NodeId[];
  /** 出力の形式を含めるか（偽なら「元と同じ」）。 */
  withFormat: boolean;
  bindings: RecipeBinding[];
}

/** エディタの頭の文書からレシピを作る（レシピとして保存…）。ID を返す。 */
export function createRecipe(name: string, description: string, pick: RecipePick): Promise<number> {
  return unwrap(
    commands.recipeCreate(
      pick.session,
      name,
      description,
      pick.keep,
      pick.withFormat,
      pick.bindings,
    ),
  );
}

/** レシピの中身をエディタの頭の文書から作り直して差し替える（見本で編集の保存。ID・名前・説明・並びはそのまま）。 */
export function replaceRecipe(id: number, pick: RecipePick): Promise<null> {
  return unwrap(
    commands.recipeReplace(id, pick.session, pick.keep, pick.withFormat, pick.bindings),
  );
}

/** 名前を変える（重なってよい）。 */
export function renameRecipe(id: number, name: string): Promise<null> {
  return unwrap(commands.recipeRename(id, name));
}

/** 説明を変える（空でよい）。 */
export function describeRecipe(id: number, description: string): Promise<null> {
  return unwrap(commands.recipeDescribe(id, description));
}

/** 並びを変える（`ids` の順。含まれないものは後ろに今の順で）。 */
export function reorderRecipes(ids: number[]): Promise<null> {
  return unwrap(commands.recipesReorder(ids));
}

/** 複製する（「{名前} のコピー」を元のすぐ後ろに）。新しい ID を返す。 */
export function duplicateRecipe(id: number): Promise<number> {
  return unwrap(commands.recipeDuplicate(id));
}

/** すぐ消す（画面は確かめてから）。 */
export function deleteRecipe(id: number): Promise<null> {
  return unwrap(commands.recipeDelete(id));
}

/** `.kurarecipe` を書き出す（保存のダイアログで選んだ絶対パス。ライブラリの外）。 */
export function exportRecipe(id: number, path: string): Promise<null> {
  return unwrap(commands.recipeExport(id, path));
}

/** `.kurarecipe` を読み込んで足す（開くダイアログで選んだ絶対パス）。旧版の操作を含めば `notes` に印。 */
export function importRecipe(path: string): Promise<RecipeImportedDto> {
  return unwrap(commands.recipeImport(path));
}
