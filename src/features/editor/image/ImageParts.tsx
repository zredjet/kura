import { type ReactNode, useMemo } from 'react';

import { BucketIcon } from '../../../components/icons/BucketIcon';
import { CropIcon } from '../../../components/icons/CropIcon';
import { EraserIcon } from '../../../components/icons/EraserIcon';
import { EyedropperIcon } from '../../../components/icons/EyedropperIcon';
import { HandIcon } from '../../../components/icons/HandIcon';
import { MinusIcon } from '../../../components/icons/MinusIcon';
import { PenIcon } from '../../../components/icons/PenIcon';
import { PlusIcon } from '../../../components/icons/PlusIcon';
import { ja } from '../../../i18n/ja';
import { formatCount } from '../../../lib/format';
import styles from '../Editor.module.css';
import { useEditorStore } from '../editorStore';
import { OpsPanel } from '../OpsPanel';
import { formatOfExt } from './imageFormats';
import { editorZoom, selectTool, useImageStore } from './imageStore';
import { EDITOR_TOOLS, type EditorTool, useToolStore } from './toolStore';
import { formatCanBeTransparent, zoomPercent } from './viewMath';

/*
 * 画像エディタの殻に置く部品（design.md §3.1「画像エディタ」。M13 の段 6a で `Editor.tsx` から分けた）：上部の道具の
 * カプセルと名前の下の一行、左下の座標・右下の倍率、右パネルの「操作」。キャンバスは `EditorCanvas`、下のカプセルは
 * `ToolSettings`。
 */

const t = ja.editor;

const TOOL_ICONS: Record<EditorTool, ReactNode> = {
  hand: <HandIcon size={16} />,
  crop: <CropIcon size={16} />,
  pen: <PenIcon size={16} />,
  eraser: <EraserIcon size={16} />,
  fill: <BucketIcon size={16} />,
  eyedropper: <EyedropperIcon size={16} />,
};

/** 上部の道具のカプセル。 */
export function ImageTools() {
  const temporary = useImageStore((s) => s.temporary);
  const format = useImageStore((s) => s.view?.format ?? null);
  const open = useEditorStore((s) => s.phase === 'open');
  const tool = useToolStore((s) => s.tool);
  const shown = temporary ?? tool;
  return (
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
  );
}

/** 「64 × 64 · PNG · 未保存」。 */
export function ImageMeta() {
  const view = useImageStore((s) => s.view);
  const source = useEditorStore((s) => s.source);
  const item = useEditorStore((s) => s.item);
  const dirty = useEditorStore((s) => s.status?.dirty ?? false);
  const w = view?.fullWidth ?? source?.width ?? item?.width ?? null;
  const h = view?.fullHeight ?? source?.height ?? item?.height ?? null;
  // 元のファイルの形式（source.format は画素の形式で、ファイルの形式ではない）
  const format = (formatOfExt(item?.ext) ?? item?.ext ?? '').toUpperCase();
  const meta = [
    w !== null && h !== null ? t.size(formatCount(w), formatCount(h)) : null,
    format || null,
    dirty ? t.unsaved : null,
  ]
    .filter(Boolean)
    .join(' · ');
  return <span className={styles.meta}>{meta}</span>;
}

/** 左下に座標と「縮小して表示中」、右下に倍率。 */
export function ImageStatus() {
  const pointer = useImageStore((s) => s.pointer);
  const level = useImageStore((s) => s.view?.level ?? 0);
  const zoom = useImageStore((s) => s.zoom);
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

/** 右パネルの「操作」（引数の欄の既定の大きさは今の出力の大きさ。出力の行は形式の表から組む既定のもの）。 */
export function ImageOps() {
  // 大きさだけを見る（表示の結果のたびに右パネルを描き直さない）
  const w = useImageStore((s) => s.view?.fullWidth ?? null);
  const h = useImageStore((s) => s.view?.fullHeight ?? null);
  const size = useMemo(() => (w !== null && h !== null ? { w, h } : null), [w, h]);
  return <OpsPanel size={size} />;
}
