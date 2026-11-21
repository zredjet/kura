import {
  commands,
  type EngineDto,
  type ExportPresetDto,
  type ItemProjectDto,
  type ProjectDto,
} from './bindings';
import { unwrap } from './result';

export type { EngineDto, ItemProjectDto, ProjectDto };

/** 登録したプロジェクトの一覧（並びの順）。 */
export function listProjects(): Promise<ProjectDto[]> {
  return unwrap(commands.projectsList());
}

/**
 * フォルダをプロジェクトとして登録し、照らし直しを積む（`name` が null ならフォルダの名前）。
 * ほかのプロジェクトと重なれば `Conflict`、ライブラリの中・外側は `Invalid`。
 */
export function addProject(path: string, name: string | null = null): Promise<ProjectDto> {
  return unwrap(commands.projectAdd(path, name));
}

/** 名前を変える。 */
export function renameProject(id: number, name: string): Promise<null> {
  return unwrap(commands.projectRename(id, name));
}

/** 場所を選び直し（使った記録は残す）、照らし直しを積む。 */
export function relocateProject(id: number, path: string): Promise<ProjectDto> {
  return unwrap(commands.projectRelocate(id, path));
}

/** 登録を外す（使った記録も消える。フォルダには触れない）。 */
export function removeProject(id: number): Promise<null> {
  return unwrap(commands.projectRemove(id));
}

/** 登録したプロジェクト全部の照らし直しを積む（待っているものがあれば null）。 */
export function scanProjects(): Promise<number | null> {
  return unwrap(commands.projectsScan());
}

/** プロジェクトで使っているアイテム（ID の順）。 */
export function projectItems(id: number): Promise<number[]> {
  return unwrap(commands.projectItems(id));
}

/** アイテムを使っているプロジェクト（一致したファイルのパスつき）。 */
export function itemProjects(item: number): Promise<ItemProjectDto[]> {
  return unwrap(commands.itemProjects(item));
}

/** プロジェクトの持ち出しの既定（命名・形式・JSON。まだ決めていなければエンジンごとの既定）。 */
export function projectSettings(id: number): Promise<ExportPresetDto> {
  return unwrap(commands.projectSettings(id));
}

/** プロジェクトの持ち出しの既定を書く。 */
export function setProjectSettings(id: number, preset: ExportPresetDto): Promise<null> {
  return unwrap(commands.projectSettingsSet(id, preset));
}
