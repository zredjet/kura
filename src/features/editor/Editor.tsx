import { type ReactNode, useEffect, useRef } from 'react';

import glass from '../../components/glass/glass.module.css';
import { BucketIcon } from '../../components/icons/BucketIcon';
import { ChevronDownIcon } from '../../components/icons/ChevronDownIcon';
import { ChevronLeftIcon } from '../../components/icons/ChevronLeftIcon';
import { CropIcon } from '../../components/icons/CropIcon';
import { EraserIcon } from '../../components/icons/EraserIcon';
import { EyedropperIcon } from '../../components/icons/EyedropperIcon';
import { HandIcon } from '../../components/icons/HandIcon';
import { InfoIcon } from '../../components/icons/InfoIcon';
import { MinusIcon } from '../../components/icons/MinusIcon';
import { PenIcon } from '../../components/icons/PenIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { openMenuBelow } from '../../components/Menu/menuStore';
import { ja } from '../../i18n/ja';
import { formatCount } from '../../lib/format';
import { ItemInspector } from '../library/inspector/ItemInspector';
import { formatOfExt } from './formats';
import styles from './Editor.module.css';
import { EditorCanvas } from './EditorCanvas';
import { useEditorKeys } from './editorKeys';
import {
  openRevert,
  openSaveAs,
  openVersions,
  requestCloseEditor,
  requestSave,
} from './editorSave';
import { EditorSheets } from './EditorSheets';
import {
  canSaveEditor,
  editorUndo,
  editorZoom,
  reopenEditor,
  selectTool,
  toggleEditorInfo,
  useEditorStore,
} from './editorStore';
import { OpsPanel } from './OpsPanel';
import { ToolSettings } from './ToolSettings';
import { EDITOR_TOOLS, type EditorTool, useToolStore } from './toolStore';
import { formatCanBeTransparent, zoomPercent } from './viewMath';

const t = ja.editor;

const TOOL_ICONS: Record<EditorTool, ReactNode> = {
  hand: <HandIcon size={16} />,
  crop: <CropIcon size={16} />,
  pen: <PenIcon size={16} />,
  eraser: <EraserIcon size={16} />,
  fill: <BucketIcon size={16} />,
  eyedropper: <EyedropperIcon size={16} />,
};

/**
 * 画像エディタ（design.md §3.1「画像エディタ」。M12 の段 5）：main の窓いっぱいの層（シアターと同じ地・余白・角丸）。
 * 上部に閉じる・名前・道具のカプセル・「情報」、キャンバスの下に道具の設定のガラスのカプセル、左下に座標、右下に倍率、
 * 右にガラスのパネル（操作・情報）。kura:// は main の WebView だけが受けるので別窓にしない。同時に開くのは 1 件。
 */
export function Editor() {
  const phase = useEditorStore((s) => s.phase);
  const openId = useEditorStore((s) => s.openId);
  const stageRef = useRef<HTMLElement>(null);
  useEditorKeys(phase !== 'closed');
  // 開いたらフォーカスをエディタに移す（下のライブラリを Tab でたどらない。閉じたら開く前の所へ戻す）
  useEffect(() => {
    if (openId > 0) stageRef.current?.focus({ preventScroll: true });
  }, [openId]);
  if (phase === 'closed') {
    return null;
  }
  return (
    <div className={styles.editor} data-editor="" role="region" aria-label={t.label}>
      <section ref={stageRef} className={styles.stage} aria-label={t.canvas} tabIndex={-1}>
        <EditorCanvas />
        <TopBar />
        {phase === 'open' && <ToolSettings />}
        <StatusLine />
        <FailureCard />
      </section>
      <SidePanel />
      <EditorSheets />
    </div>
  );
}

/** 保存できるか（`canSaveEditor`：⌘S・ファイル › 保存と同じ条件）。 */
function useCanSave(): boolean {
  return useEditorStore(canSaveEditor);
}

/** 主ボタン「保存」（⌘S。進捗はボタンの中）と ▾（別名で保存…・元に戻す…・版の一覧…）。 */
function SaveControls() {
  const open = useEditorStore((s) => s.phase === 'open');
  const save = useEditorStore((s) => s.save);
  const canSave = useCanSave();
  const percent =
    save?.progress != null ? Math.round(Math.min(1, Math.max(0, save.progress)) * 100) : null;
  const label = save ? (percent !== null ? t.savingPercent(String(percent)) : t.saving) : t.save;
  return (
    <div className={styles.saveArea}>
      <div className={styles.saveGroup}>
        <button
          type="button"
          className={styles.saveButton}
          title={t.saveKey}
          disabled={!canSave}
          aria-busy={save !== null || undefined}
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
            openMenuBelow(e.currentTarget, t.saveMore, [
              { label: t.saveAs, shortcut: '⌘⇧S', disabled: save !== null, onSelect: openSaveAs },
              'separator',
              { label: t.revert, disabled: save !== null, onSelect: openRevert },
              { label: t.versions, onSelect: openVersions },
            ])
          }
        >
          <ChevronDownIcon size={12} strokeWidth={2.4} />
        </button>
      </div>
      {save?.slow && (
        <p className={styles.parkedHint} role="status">
          {t.parkedHint}
        </p>
      )}
    </div>
  );
}

/** 「64 × 64 · PNG · 未保存」。 */
function useMetaLine(): string {
  const view = useEditorStore((s) => s.view);
  const source = useEditorStore((s) => s.source);
  const item = useEditorStore((s) => s.item);
  const dirty = useEditorStore((s) => s.status?.dirty ?? false);
  const w = view?.fullWidth ?? source?.width ?? item?.width ?? null;
  const h = view?.fullHeight ?? source?.height ?? item?.height ?? null;
  // 元のファイルの形式（source.format は画素の形式で、ファイルの形式ではない）
  const format = (formatOfExt(item?.ext) ?? item?.ext ?? '').toUpperCase();
  return [
    w !== null && h !== null ? t.size(formatCount(w), formatCount(h)) : null,
    format || null,
    dirty ? t.unsaved : null,
  ]
    .filter(Boolean)
    .join(' · ');
}

/** 上部：閉じる・名前・道具のカプセル・右パネルの「情報」。 */
function TopBar() {
  const item = useEditorStore((s) => s.item);
  const panel = useEditorStore((s) => s.panel);
  const temporary = useEditorStore((s) => s.temporary);
  const format = useEditorStore((s) => s.view?.format ?? null);
  const open = useEditorStore((s) => s.phase === 'open');
  const tool = useToolStore((s) => s.tool);
  const meta = useMetaLine();
  const shown = temporary ?? tool;
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
        <span className={styles.title}>{item?.title ?? ''}</span>
        <span className={styles.meta}>{meta}</span>
      </div>
      <div className={styles.tools} role="group" aria-label={t.tools}>
        {EDITOR_TOOLS.map((id) => {
          const label = t.withKey(t.toolNames[id], t.toolKeys[id]);
          const unavailable = id === 'eraser' && format !== null && !formatCanBeTransparent(format);
          return (
            <button
              key={id}
              type="button"
              className={styles.tool}
              aria-label={label}
              title={unavailable ? t.eraserUnavailable : label}
              aria-pressed={shown === id}
              disabled={!open || unavailable}
              onClick={() => selectTool(id)}
            >
              {TOOL_ICONS[id]}
            </button>
          );
        })}
      </div>
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
      <SaveControls />
    </div>
  );
}

/** 左下に座標と「縮小して表示中」、右下に倍率。 */
function StatusLine() {
  const pointer = useEditorStore((s) => s.pointer);
  const level = useEditorStore((s) => s.view?.level ?? 0);
  const zoom = useEditorStore((s) => s.zoom);
  const open = useEditorStore((s) => s.phase === 'open');
  if (!open) return null;
  return (
    <>
      <div className={styles.coords} aria-live="off">
        {pointer && <span>{t.coords(formatCount(pointer[0]), formatCount(pointer[1]))}</span>}
        {level > 0 && <span className={styles.reduced}>{t.reduced}</span>}
      </div>
      <div className={styles.zoom} role="group" aria-label={t.zoom}>
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.zoomOut}
          title={t.zoomOut}
          onClick={() => editorZoom(-1)}
        >
          <MinusIcon size={14} strokeWidth={2} />
        </button>
        <span className={styles.zoomValue}>{zoomPercent(zoom)}</span>
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.zoomIn}
          title={t.zoomIn}
          onClick={() => editorZoom(1)}
        >
          <PlusIcon size={14} strokeWidth={2} />
        </button>
      </div>
    </>
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

/** 右のガラスのパネル：操作（並び・引数・出力の形式）か、アイテムの情報（インスペクタ）。 */
function SidePanel() {
  const panel = useEditorStore((s) => s.panel);
  const item = useEditorStore((s) => s.item);
  return (
    <aside
      className={`${glass.panel} ${styles.panel}`}
      aria-label={panel === 'info' ? t.info : t.opsTitle}
    >
      {panel === 'info' && item ? (
        <div className={styles.info}>
          <ItemInspector key={item.id} id={item.id} editable={false} />
        </div>
      ) : (
        <OpsPanel />
      )}
    </aside>
  );
}
