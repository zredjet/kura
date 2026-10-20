import { useEffect, useId, useMemo, useRef, useState } from 'react';

import { Button } from '../../components/Button/Button';
import { Dialog } from '../../components/Dialog/Dialog';
import { ja } from '../../i18n/ja';
import { type AssetDto, type AssetGridDto, setAssetSettings } from '../../ipc/asset';
import { fileUrl } from '../../ipc/media';
import { type Cell, gridCells, MAX_FRAMES } from '../../lib/asset/timeline';
import { formatCount } from '../../lib/format';
import { AnimPlayer, type AnimClock, type DrawFrame, gridSource, loadImage } from './AnimPlayer';
import styles from './Asset.module.css';

const t = ja.asset;

/** プレビューの枠の高さ（CSS px）。 */
const PREVIEW_HEIGHT = 160;
/** アニメの枠の下の操作の行（28px）と間隔（6px）。 */
const CONTROLS_HEIGHT = 34;
/** 枠の幅を測れないとき（シート 420px の中の 2 列の 1 つ）。 */
const PREVIEW_WIDTH = 182;
/** 格子がまだないときのセルの大きさの初め。 */
const DEFAULT_CELL = 16;

/** 入力欄の文字（空は「なし」）。 */
export interface GridForm {
  cellW: string;
  cellH: string;
  offsetX: string;
  offsetY: string;
  gapX: string;
  gapY: string;
  count: string;
  frameMs: string;
}

export function formOf(asset: AssetDto): GridForm {
  const g = asset.settings.grid;
  const n = (v: number | null | undefined, fallback = '') =>
    v === null || v === undefined ? fallback : String(v);
  return {
    cellW: n(g?.cellW, String(DEFAULT_CELL)),
    cellH: n(g?.cellH, String(DEFAULT_CELL)),
    offsetX: n(g?.offsetX, '0'),
    offsetY: n(g?.offsetY, '0'),
    gapX: n(g?.gapX, '0'),
    gapY: n(g?.gapY, '0'),
    count: n(g?.count),
    frameMs: n(asset.settings.frameMs),
  };
}

function int(text: string, min: number): number | null {
  const s = text.trim();
  if (!/^\d+$/.test(s)) return null;
  const v = Number(s);
  return Number.isSafeInteger(v) && v >= min ? v : null;
}

/** 入力から格子とコマの長さを読む（読めない欄があれば null）。空のコマの数は「すべて」、空のコマの長さは自動（0）。 */
export function parseGridForm(form: GridForm): { grid: AssetGridDto; frameMs: number } | null {
  const cellW = int(form.cellW, 1);
  const cellH = int(form.cellH, 1);
  const offsetX = int(form.offsetX, 0);
  const offsetY = int(form.offsetY, 0);
  const gapX = int(form.gapX, 0);
  const gapY = int(form.gapY, 0);
  const count = form.count.trim() === '' ? null : int(form.count, 1);
  const frameMs = form.frameMs.trim() === '' ? 0 : int(form.frameMs, 1);
  if (
    cellW === null ||
    cellH === null ||
    offsetX === null ||
    offsetY === null ||
    gapX === null ||
    gapY === null ||
    (form.count.trim() !== '' && count === null) ||
    frameMs === null
  ) {
    return null;
  }
  return { grid: { cellW, cellH, offsetX, offsetY, gapX, gapY, count }, frameMs };
}

/** 格子を確かめる（画像の大きさが分からなければセルの大きさだけ）。問題がなければ null と、セルの列。 */
export function checkGrid(
  grid: AssetGridDto | null,
  width: number | null,
  height: number | null,
): { error: string | null; cells: Cell[] } {
  if (!grid) {
    return { error: t.gridNoCells, cells: [] };
  }
  if (!width || !height) {
    return { error: null, cells: [] };
  }
  const cells = gridCells(width, height, grid, MAX_FRAMES);
  if (cells.length === 0) {
    return { error: t.gridNoCells, cells };
  }
  if (grid.count !== null && grid.count > cells.length) {
    return { error: t.gridTooMany(formatCount(cells.length)), cells };
  }
  return { error: null, cells: grid.count === null ? cells : cells.slice(0, grid.count) };
}

/** 元の画像に格子の線（1px の --accent）を重ねて描く。 */
export type DrawOverlay = (
  canvas: HTMLCanvasElement,
  image: CanvasImageSource,
  size: { w: number; h: number },
  cells: readonly Cell[],
) => void;

export const drawOverlay: DrawOverlay = (canvas, image, size, cells) => {
  const ctx = canvas.getContext('2d');
  if (!ctx || size.w <= 0 || size.h <= 0) return;
  const sx = canvas.width / size.w;
  const sy = canvas.height / size.h;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.imageSmoothingEnabled = sx < 1 || sy < 1;
  ctx.drawImage(image, 0, 0, canvas.width, canvas.height);
  ctx.strokeStyle = getComputedStyle(canvas).getPropertyValue('--accent').trim();
  ctx.lineWidth = 1;
  for (const c of cells) {
    ctx.strokeRect(
      Math.round(c.x * sx) + 0.5,
      Math.round(c.y * sy) + 0.5,
      Math.max(1, Math.round(c.w * sx) - 1),
      Math.max(1, Math.round(c.h * sy) - 1),
    );
  }
};

/** 元の画像と格子の線。 */
function GridOverlay({
  url,
  width,
  height,
  cells,
  draw,
  load,
  dpr,
}: {
  url: string;
  width: number;
  height: number;
  cells: readonly Cell[];
  draw: DrawOverlay;
  load: (url: string) => Promise<CanvasImageSource>;
  dpr: number;
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  const [image, setImage] = useState<CanvasImageSource | null>(null);
  useEffect(() => {
    let alive = true;
    load(url).then(
      (img) => {
        if (alive) setImage(img);
      },
      () => undefined,
    );
    return () => {
      alive = false;
    };
  }, [url, load]);
  const s = Math.min(1, PREVIEW_WIDTH / width, PREVIEW_HEIGHT / height);
  const w = Math.max(1, Math.round(width * s));
  const h = Math.max(1, Math.round(height * s));
  useEffect(() => {
    if (ref.current && image) {
      draw(ref.current, image, { w: width, h: height }, cells);
    }
  }, [image, cells, draw, width, height, w, h]);
  return (
    <canvas
      ref={ref}
      className={styles.canvas}
      width={Math.round(w * dpr)}
      height={Math.round(h * dpr)}
      style={{ width: w, height: h }}
    />
  );
}

interface GridSheetProps {
  asset: AssetDto;
  onClose: () => void;
  /** 保存した・外した（読み直す）。 */
  onSaved: () => void;
  /** テスト用：描く関数・画像を読む口・時計・装置の画素の比。 */
  draw?: DrawFrame;
  overlay?: DrawOverlay;
  load?: (url: string) => Promise<CanvasImageSource>;
  clock?: AnimClock;
  dpr?: number;
}

/**
 * 格子のシート（design.md §3.1「格子のシート」。size="sm"）：上にその場のプレビュー（元の画像に格子の線を重ねた
 * ものと、その格子で回したアニメ）、下に数の入力（2 列）。左に「格子を外す」（設定があるとき）、右に「キャンセル」
 * 「保存」。解析の推定（sprite_grid）は素材の欄にまだ出していないので「推定を使う」は置かない（M11）。
 */
export function GridSheet({
  asset,
  onClose,
  onSaved,
  draw,
  overlay = drawOverlay,
  load = loadImage,
  clock,
  dpr = typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1,
}: GridSheetProps) {
  const id = useId();
  const [form, setForm] = useState<GridForm>(() => formOf(asset));
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const parsed = useMemo(() => parseGridForm(form), [form]);
  const checked = useMemo(
    () => checkGrid(parsed?.grid ?? null, asset.width, asset.height),
    [parsed, asset.width, asset.height],
  );
  const url = fileUrl(asset.hash);
  const frameMs = parsed?.frameMs ?? 0;

  const save = (patch: Parameters<typeof setAssetSettings>[1]) => {
    setSaving(true);
    setFailure(null);
    setAssetSettings([asset.itemId], patch).then(
      () => {
        setSaving(false);
        onSaved();
        onClose();
      },
      (error: unknown) => {
        setSaving(false);
        setFailure(error instanceof Error ? error.message : String(error));
      },
    );
  };

  const field = (key: keyof GridForm, label: string, placeholder?: string) => (
    <label className={styles.field}>
      {label}
      <input
        type="text"
        inputMode="numeric"
        value={form[key]}
        placeholder={placeholder}
        onChange={(e) => setForm({ ...form, [key]: e.target.value })}
      />
    </label>
  );

  const note = failure ?? checked.error;
  return (
    <Dialog labelledBy={id} onClose={onClose} size="sm">
      <h2 id={id} className={styles.sheetTitle}>
        {t.gridTitle}
      </h2>
      {asset.width && asset.height ? (
        <div className={styles.sheetPreview} aria-label={t.gridPreview} role="group">
          <div className={styles.frame} style={{ height: PREVIEW_HEIGHT }}>
            <GridOverlay
              url={url}
              width={asset.width}
              height={asset.height}
              cells={checked.cells}
              draw={overlay}
              load={load}
              dpr={dpr}
            />
          </div>
          <div className={styles.stage}>
            {checked.error === null && checked.cells.length > 0 ? (
              <AnimPlayer
                key={JSON.stringify([parsed?.grid, frameMs])}
                source={gridSource(url, checked.cells, frameMs)}
                pixelArt={asset.effective.pixelArt}
                alpha={asset.facts?.image.alpha ?? null}
                frameHeight={PREVIEW_HEIGHT - CONTROLS_HEIGHT}
                draw={draw}
                load={load}
                clock={clock}
                dpr={dpr}
              />
            ) : (
              <div className={styles.frame} style={{ height: PREVIEW_HEIGHT }} />
            )}
          </div>
        </div>
      ) : null}
      <div className={styles.form}>
        <div className={styles.fields}>
          {field('cellW', t.cellW)}
          {field('cellH', t.cellH)}
          {field('offsetX', t.offsetX)}
          {field('offsetY', t.offsetY)}
          {field('gapX', t.gapX)}
          {field('gapY', t.gapY)}
          {field('count', t.count, t.countPlaceholder)}
          {field('frameMs', t.frameMs, t.frameMsPlaceholder)}
        </div>
        <p
          className={[styles.sheetNote, note ? styles.sheetError : ''].filter(Boolean).join(' ')}
          role={note ? 'alert' : undefined}
        >
          {note ?? (checked.cells.length > 0 ? t.gridCells(formatCount(checked.cells.length)) : '')}
        </p>
      </div>
      <div className={styles.sheetActions}>
        {asset.settings.grid && (
          <Button size="sm" disabled={saving} onClick={() => save({ grid: 'clear' })}>
            {t.gridClear}
          </Button>
        )}
        <span className={styles.spacer} />
        <Button size="sm" onClick={onClose}>
          {ja.common.cancel}
        </Button>
        <Button
          size="sm"
          variant="primary"
          disabled={saving || !parsed || checked.error !== null}
          onClick={() => parsed && save({ grid: { set: parsed.grid }, frameMs: parsed.frameMs })}
        >
          {ja.common.save}
        </Button>
      </div>
    </Dialog>
  );
}
