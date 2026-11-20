import { useEffect, useRef } from 'react';

import glass from '../../components/glass/glass.module.css';
import { ChevronDownIcon } from '../../components/icons/ChevronDownIcon';
import { ChevronLeftIcon } from '../../components/icons/ChevronLeftIcon';
import { InfoIcon } from '../../components/icons/InfoIcon';
import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { ItemInspector } from '../library/inspector/ItemInspector';
import styles from './Editor.module.css';
import { useEditorKeys } from './editorKeys';
import { type EditorKindUi, editorKindModule } from './editorKinds';
import {
  cancelEditorSave,
  openRevert,
  openSaveAs,
  openSaveRecipe,
  openVersions,
  pauseEditorSave,
  requestCloseEditor,
  requestSave,
  resumeEditorSave,
} from './editorSave';
import { EditorSheets } from './EditorSheets';
import {
  canSaveEditor,
  editorUndo,
  reopenEditor,
  toggleEditorInfo,
  useEditorStore,
} from './editorStore';

const t = ja.editor;

/**
 * エディタの殻（design.md §3.1「画像エディタ」。M12 の段 5、M13 の段 6a で種類ごとの部品と分けた）：main の窓いっぱいの層
 * （シアターと同じ地・余白・角丸）。上部に閉じる・名前・道具のカプセル・「情報」・保存、表示の面の下にガラスのカプセル、
 * 左下・右下に表示の情報、右にガラスのパネル（操作・情報）。表示の面・道具・カプセル・左下と右下・「操作」は種類ごとの
 * 部品（`EditorKindUi`）。kura:// は main の WebView だけが受けるので別窓にしない。同時に開くのは 1 件。
 */
export function Editor() {
  const phase = useEditorStore((s) => s.phase);
  const openId = useEditorStore((s) => s.openId);
  const kind = useEditorStore((s) => s.kind);
  const stageRef = useRef<HTMLElement>(null);
  useEditorKeys(phase !== 'closed');
  // 開いたらフォーカスをエディタに移す（下のライブラリを Tab でたどらない。閉じたら開く前の所へ戻す）
  useEffect(() => {
    if (openId > 0) stageRef.current?.focus({ preventScroll: true });
  }, [openId]);
  const ui = editorKindModule(kind)?.ui;
  if (phase === 'closed' || !ui) {
    return null;
  }
  return (
    <div
      className={styles.editor}
      data-editor=""
      role="region"
      aria-label={kind === 'audio' ? t.audio.label : t.label}
    >
      <section
        ref={stageRef}
        className={styles.stage}
        aria-label={kind === 'audio' ? t.audio.wave : t.canvas}
        tabIndex={-1}
      >
        <ui.Stage />
        <TopBar ui={ui} />
        {phase === 'open' && <ui.Capsule />}
        <ui.Status />
        <FailureCard />
      </section>
      <SidePanel ui={ui} />
      <EditorSheets />
    </div>
  );
}

/** 保存できるか（`canSaveEditor`：⌘S・ファイル › 保存と同じ条件）。 */
function useCanSave(): boolean {
  return useEditorStore(canSaveEditor);
}

/** ▾ の項目：保存の間は一時停止（一時停止の間は再開）・中止を先頭に。別名で保存…・元に戻す…は保存の間は押せない。 */
function saveMenu(state: 'idle' | 'running' | 'paused'): MenuEntry[] {
  const busy = state !== 'idle';
  const control: MenuEntry[] = busy
    ? [
        state === 'paused'
          ? { label: t.saveResume, onSelect: resumeEditorSave }
          : { label: t.savePause, onSelect: pauseEditorSave },
        { label: t.saveCancel, onSelect: cancelEditorSave },
        'separator',
      ]
    : [];
  return [
    ...control,
    { label: t.saveAs, shortcut: '⌘⇧S', disabled: busy, onSelect: openSaveAs },
    'separator',
    { label: t.revert, disabled: busy, onSelect: openRevert },
    { label: t.versions, onSelect: openVersions },
    'separator',
    { label: ja.saveRecipe.menu, onSelect: openSaveRecipe },
  ];
}

/** レシピの見本の主ボタン「レシピを保存」（⌘S。応えの来ていない手があれば待つ。M14 段 7）。 */
function RecipeSaveControls() {
  const ready = useEditorStore((s) => s.phase === 'open' && !s.pendingSteps && !s.sheet);
  return (
    <div className={styles.saveArea}>
      <button
        type="button"
        className={styles.saveButton}
        data-recipe=""
        title={t.saveKey}
        disabled={!ready}
        onClick={() => requestSave()}
      >
        <span className={styles.saveLabel}>{ja.recipeEditor.save}</span>
      </button>
    </div>
  );
}

/** 主ボタン「保存」（⌘S。進捗はボタンの中。一時停止の間は「一時停止中 42%」）と ▾（このエディタで積んだ保存の操作）。 */
function SaveControls() {
  const open = useEditorStore((s) => s.phase === 'open');
  const save = useEditorStore((s) => s.save);
  const slow = useEditorStore((s) => s.save?.slow ?? false);
  const canSave = useCanSave();
  const paused = save?.state === 'paused';
  const percent =
    save?.progress != null ? Math.round(Math.min(1, Math.max(0, save.progress)) * 100) : null;
  const running = percent !== null ? t.savingPercent(String(percent)) : t.saving;
  const stopped = percent !== null ? t.savePausedPercent(String(percent)) : t.savePaused;
  const label = !save ? t.save : paused ? stopped : running;
  return (
    <div className={styles.saveArea}>
      <div className={styles.saveGroup}>
        <button
          type="button"
          className={styles.saveButton}
          title={t.saveKey}
          disabled={!canSave}
          aria-busy={(save !== null && !paused) || undefined}
          data-paused={paused || undefined}
          onClick={() => requestSave()}
        >
          {save && (
            <span
              className={styles.saveProgress}
              style={{ width: `${percent ?? 0}%` }}
              aria-hidden="true"
            />
          )}
          <span className={styles.saveLabel}>{label}</span>
        </button>
        <button
          type="button"
          className={styles.saveMore}
          aria-label={t.saveMore}
          title={t.saveMore}
          aria-haspopup="menu"
          disabled={!open}
          onClick={(e) =>
            openMenuBelow(
              e.currentTarget,
              t.saveMore,
              saveMenu(!save ? 'idle' : paused ? 'paused' : 'running'),
            )
          }
        >
          <ChevronDownIcon size={12} strokeWidth={2.4} />
        </button>
      </div>
      {slow && (
        <p className={styles.parkedHint} role="status">
          {t.parkedHint}
        </p>
      )}
    </div>
  );
}

/** 上部：閉じる・名前（と種類ごとの一行）・種類ごとの道具・右パネルの「情報」・保存。 */
function TopBar({ ui }: { ui: EditorKindUi }) {
  const item = useEditorStore((s) => s.item);
  const panel = useEditorStore((s) => s.panel);
  const recipe = useEditorStore((s) => s.recipe);
  return (
    <div className={styles.top} data-editor-top="" data-tauri-drag-region>
      <button
        type="button"
        className={styles.round}
        aria-label={t.close}
        title={t.withKey(t.close, 'esc')}
        onClick={() => void requestCloseEditor()}
      >
        <ChevronLeftIcon size={16} strokeWidth={2} />
      </button>
      <div className={styles.heading} data-tauri-drag-region>
        <span className={styles.title}>
          {recipe ? ja.recipeEditor.banner(recipe.name, item?.title ?? '') : (item?.title ?? '')}
        </span>
        <ui.Meta />
      </div>
      <ui.Tools />
      {/* レシピの見本は見本のアイテムの情報を出さない（アイテムには保存しないので） */}
      {!recipe && (
        <div className={styles.segments} role="group" aria-label={t.panels}>
          <button
            type="button"
            className={styles.segment}
            aria-label={t.info}
            title={t.withKey(t.info, '⌘I')}
            aria-pressed={panel === 'info'}
            onClick={toggleEditorInfo}
          >
            <InfoIcon size={16} />
          </button>
        </div>
      )}
      {recipe ? <RecipeSaveControls /> : <SaveControls />}
    </div>
  );
}

/** 開けなかった・表示できなかった・要求が通らなかったときの案内。 */
function FailureCard() {
  const phase = useEditorStore((s) => s.phase);
  const failure = useEditorStore((s) => s.failure);
  const canUndo = useEditorStore((s) => s.status?.canUndo ?? false);
  if (phase === 'opening') {
    return (
      <p className={styles.opening} role="status">
        {t.opening}
      </p>
    );
  }
  if (!failure) return null;
  const gone = failure.code === 'NotFound' || failure.code === 'Conflict';
  const title = failure.kind === 'open' ? t.openFailed : gone ? t.closedHint : t.viewFailed;
  return (
    <div className={`${glass.panel} ${styles.failure}`} role="alert">
      <p className={styles.failureTitle}>{title}</p>
      <p className={styles.failureMessage}>{failure.message}</p>
      <div className={styles.failureActions}>
        {failure.kind === 'view' && canUndo && (
          <button type="button" className={styles.pill} onClick={editorUndo}>
            {t.undo}
          </button>
        )}
        {(failure.kind === 'open' || gone) && (
          <button type="button" className={styles.pill} onClick={reopenEditor}>
            {failure.kind === 'open' ? t.retry : t.reopen}
          </button>
        )}
        <button type="button" className={styles.pill} onClick={() => void requestCloseEditor()}>
          {t.close}
        </button>
      </div>
    </div>
  );
}

/** 右のガラスのパネル：種類ごとの「操作」か、アイテムの情報（インスペクタ）。 */
function SidePanel({ ui }: { ui: EditorKindUi }) {
  // レシピの見本は情報を出さず、いつも操作
  const panel = useEditorStore((s) => (s.recipe ? 'ops' : s.panel));
  const item = useEditorStore((s) => s.item);
  return (
    <aside
      className={`${glass.panel} ${styles.panel}`}
      aria-label={panel === 'info' ? t.info : t.opsTitle}
    >
      {panel === 'info' && item ? (
        <div className={styles.info}>
          <ItemInspector key={item.id} id={item.id} editable={false} audition={false} />
        </div>
      ) : (
        <ui.Ops />
      )}
    </aside>
  );
}
