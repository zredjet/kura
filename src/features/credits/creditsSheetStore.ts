import { create } from 'zustand';

import type { CreditsScopeDto } from '../../ipc/credits';

/** クレジットの一覧のシート（範囲と見出しの名前。M15 段 6）。 */
interface CreditsSheetState {
  scope: CreditsScopeDto | null;
  /** プロジェクトの名前（選んだものなら null）。 */
  name: string | null;
}

export const useCreditsSheetStore = create<CreditsSheetState>(() => ({ scope: null, name: null }));

/** クレジットの一覧を開く（プロジェクトか、選んだアイテムとユニット）。 */
export function openCreditsSheet(scope: CreditsScopeDto, name: string | null = null): void {
  useCreditsSheetStore.setState({ scope, name });
}

/** 閉じる。 */
export function closeCreditsSheet(): void {
  useCreditsSheetStore.setState({ scope: null, name: null });
}
