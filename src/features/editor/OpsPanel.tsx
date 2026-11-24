import { type ComponentType, type ReactNode, useEffect, useId, useRef, useState } from 'react';

import { ChevronDownIcon } from '../../components/icons/ChevronDownIcon';
import { ChevronUpIcon } from '../../components/icons/ChevronUpIcon';
import { CloseIcon } from '../../components/icons/CloseIcon';
import { PlusIcon } from '../../components/icons/PlusIcon';
import { WarningIcon } from '../../components/icons/WarningIcon';
import { type MenuEntry, openMenuBelow } from '../../components/Menu/menuStore';
import { Select } from '../../components/Select/Select';
import { Switch } from '../../components/Switch/Switch';
import { ja } from '../../i18n/ja';
import { getItemDetail } from '../../ipc/items';
import styles from './Editor.module.css';
import {
  addOp,
  moveNode,
  originalFormatOf,
  removeNode,
  selectNode,
  setNodeEnabled,
  setOutputFormat,
  useEditorStore,
} from './editorStore';
import { formatUnavailableText, outputChoice, unavailableNote, unavailableRow } from './formats';
import { formatsFor } from './kindTraits';
import {
  addableOps,
  hasLaterPaint,
  opName,
  opSpec,
  paramsOf,
  paramValue,
  summarizeNode,
  visibleParams,
} from './opsCatalog';
import { issueText, ParamField } from './ParamFields';

const t = ja.editor;

/** 形式の名前。 */
function formatName(format: string | null | undefined): string {
  return format ? (t.formats[format] ?? format.toUpperCase()) : '';
}

/** 操作の並びの 1 行（36px）：名前・要約・有効のスイッチ・並べ替え・消す。 */
function OpRow({ id, index, count }: { id: number; index: number; count: number }) {
  const node = useEditorStore((s) => s.doc.nodes[id]);
  const selected = useEditorStore((s) => s.selected === id);
  if (!node) return null;
  const name = opName(node.op);
  const summary = summarizeNode(node);
  return (
    <li
      className={styles.op}
      data-selected={selected || undefined}
      data-disabled={!node.enabled || undefined}
    >
      <button
        type="button"
        className={styles.opMain}
        aria-pressed={selected}
        aria-label={t.selectOp(name)}
        data-op-row={id}
        onClick={() => selectNode(selected ? null : id)}
      >
        <span className={styles.opName}>{name}</span>
        {summary && <span className={styles.opSummary}>{summary}</span>}
      </button>
      <Switch
        checked={node.enabled}
        aria-label={t.enableOp(name)}
        onChange={(on) => setNodeEnabled(id, on)}
      />
      <span className={styles.opTools}>
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.moveUp}
          title={t.moveUp}
          disabled={index === 0}
          onClick={() => moveNode(id, -1)}
        >
          <ChevronUpIcon size={13} strokeWidth={2} />
        </button>
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.moveDown}
          title={t.moveDown}
          disabled={index === count - 1}
          onClick={() => moveNode(id, 1)}
        >
          <ChevronDownIcon size={13} strokeWidth={2} />
        </button>
        <button
          type="button"
          className={styles.iconButton}
          aria-label={t.removeOp(name)}
          title={t.removeOp(name)}
          onClick={() => removeNode(id)}
        >
          <CloseIcon size={13} strokeWidth={2} />
        </button>
      </span>
    </li>
  );
}

/** 操作の並び（↑↓ で選ぶ・⌫ で消す・⌥⌘↑↓ で並べ替え。キーは editorKeys）。 */
function OpStack() {
  const order = useEditorStore((s) => s.doc.order);
  const selected = useEditorStore((s) => s.selected);
  const listRef = useRef<HTMLOListElement>(null);
  // キーで選びなおしたとき、並びの中にフォーカスがあれば選んだ行へ移す
  useEffect(() => {
    const list = listRef.current;
    if (!list || selected === null || !list.contains(document.activeElement)) return;
    list.querySelector<HTMLElement>(`[data-op-row="${selected}"]`)?.focus();
  }, [selected]);
  if (order.length === 0) return <p className={styles.opsEmpty}>{t.opsEmpty}</p>;
  return (
    <ol ref={listRef} className={styles.opsList} aria-label={t.stack}>
      {order.map((id, i) => (
        <OpRow key={id} id={id} index={i} count={order.length} />
      ))}
    </ol>
  );
}

/** 点線の「操作を追加」（開いている種類で足せる操作を分類ごとに横へ開くメニュー）。描画は描く道具で足すので出さない。 */
function AddOpButton() {
  const open = useEditorStore((s) => s.phase === 'open');
  const kind = useEditorStore((s) => s.kind);
  const entries = (): MenuEntry[] =>
    (kind ? addableOps(kind) : [])
      .map((g) => ({
        label: t.opCategories[g.category],
        submenu: g.ops
          .filter((o) => o.id !== 'paint.actions')
          .map((o) => ({ label: opName(o.id), onSelect: () => addOp(o.id) })),
      }))
      .filter((g) => g.submenu.length > 0);
  return (
    <button
      type="button"
      className={styles.addOp}
      aria-haspopup="menu"
      disabled={!open}
      onClick={(e) => openMenuBelow(e.currentTarget, t.addOpMenu, entries())}
    >
      <PlusIcon size={14} strokeWidth={2} />
      {t.addOp}
    </button>
  );
}

/** 引数の欄の既定値に使う大きさ（画像は今の出力の大きさ）。 */
type ParamSize = { w: number; h: number } | null;

/**
 * 標本の引数を時間の欄にするためのノードごとのレートと長さ（音声。ノードの入力 0 のもの。分からなければ null で数の欄）。
 */
export interface ParamTime {
  rate(node: number): number | null;
  frames(node: number): number | null;
}

/**
 * 選んだ操作の引数パネル（`ParamSchema` から組む。表示の条件・「詳細」・指摘・後ろの描画の知らせ）。`size` は欄の既定値
 * （矩形・点・大きさを「指定する」にしたとき・長さを px にしたとき）に使う。
 */
/**
 * 引数の欄の差し込み（種類のモジュールが渡す。核は音声のストアを読まない）：`eqBands` はイコライザのバンドの列の欄、
 * `hidden` はその操作のその引数を出さない（段 7 まで押せない「直線位相」の位相の欄など）。
 */
export interface ParamFieldOverrides {
  eqBands?: ComponentType<{ node: number }>;
  hidden?(op: string, key: string): boolean;
}

function ParamsPanel({
  size,
  time,
  fields,
}: {
  size: ParamSize;
  time?: ParamTime;
  fields?: ParamFieldOverrides;
}) {
  const selected = useEditorStore((s) => s.selected);
  const node = useEditorStore((s) => (s.selected === null ? undefined : s.doc.nodes[s.selected]));
  const doc = useEditorStore((s) => s.doc);
  const issues = useEditorStore((s) => s.issues);
  const warnings = useEditorStore((s) => s.warnings);
  const headingId = useId();
  if (selected === null || !node) return <p className={styles.opsHint}>{t.opsHint}</p>;
  const desc = opSpec(node.op, node.v);
  const latest = opSpec(node.op);
  const params = paramsOf(node);
  const all = [...issues, ...warnings];
  let body;
  if (!desc) {
    body = <p className={styles.opsHint}>{latest ? t.legacyOp : t.unknownOp}</p>;
  } else {
    const shownParams = visibleParams(desc, params);
    const keep = (spec: (typeof shownParams.basic)[number]) => !fields?.hidden?.(node.op, spec.key);
    const basic = shownParams.basic.filter(keep);
    const advanced = shownParams.advanced.filter(keep);
    const EqBands = fields?.eqBands;
    // ノードと引数で key する（別のノードを選んだら欄を作り直し、前のノードの手元の値を持ち越さない）
    const field = (spec: (typeof basic)[number]) =>
      spec.kind.type === 'eqBands' && EqBands ? (
        <EqBands key={`${node.id}:${spec.key}`} node={node.id} />
      ) : (
        <ParamField
          key={`${node.id}:${spec.key}`}
          node={node.id}
          op={node.op}
          spec={spec}
          value={paramValue(params, spec)}
          issues={all}
          size={size}
          count={node.log?.field === spec.key ? node.log.count : null}
          time={time ? { rate: time.rate(node.id), frames: time.frames(node.id) } : undefined}
        />
      );
    body =
      basic.length + advanced.length === 0 ? (
        <p className={styles.opsHint}>{t.noParams}</p>
      ) : (
        <>
          {basic.map(field)}
          {advanced.length > 0 && (
            <details className={styles.advanced}>
              <summary>{t.advanced}</summary>
              {advanced.map(field)}
            </details>
          )}
        </>
      );
  }
  // 指摘のうち、引数を指さないもの（ノードの指摘）
  const nodeIssues = issues.filter((i) => i.node === node.id && i.param === null);
  return (
    <section className={styles.params} aria-labelledby={headingId}>
      <h3 id={headingId} className={styles.paramsTitle}>
        {opName(node.op)}
        {!node.enabled && <span className={styles.badge}>{t.disabledOp}</span>}
      </h3>
      {body}
      {nodeIssues.map((issue, i) => (
        <p key={i} className={styles.paramIssue} role="alert">
          {issueText(issue)}
        </p>
      ))}
      {node.op !== 'paint.actions' && hasLaterPaint(doc, node.id) && (
        <p className={styles.note}>{t.laterPaint}</p>
      )}
    </section>
  );
}

/** 表示の警告（メタデータを落とした、など）。 */
function Warnings() {
  const warnings = useEditorStore((s) => s.warnings);
  const nodes = useEditorStore((s) => s.doc.nodes);
  if (warnings.length === 0) return null;
  return (
    <section className={styles.warnings} aria-label={t.warnings}>
      <h3 className={styles.paramsTitle}>{t.warnings}</h3>
      <ul className={styles.warningList}>
        {warnings.map((w, i) => {
          const node = w.node === null ? undefined : nodes[w.node];
          return (
            <li key={i} className={styles.warning}>
              <WarningIcon size={13} />
              <span>{t.warningLine(node ? opName(node.op) : null, issueText(w))}</span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

/**
 * 出力の形式（アイテムの種類の形式の表から組む。形式が変わると「書き出すときの名前：hero.webp」）。種類ごとに別の行に
 * するときは `OpsPanel` の `output` で差し替える。この環境で書けない形式（開いたときに読む `edit_formats`。ffmpeg の
 * ない MP3 など）は押せず「ffmpeg が要ります」「この環境では書けません」を添え、今の出力の形式が書けなければ形式を選ぶ
 * シートと同じ理由の文を出す。一覧を読めていなければ形式の表だけ（M13 段 7）。
 */
export function FormatOutputRow() {
  const outputs = useEditorStore((s) => s.doc.outputs);
  const item = useEditorStore((s) => s.item);
  const kind = useEditorStore((s) => s.kind);
  const rows = useEditorStore((s) => s.writableFormats);
  // 元のファイルの形式（Rust の originalFormat。今の拡張子ではない。source.format は画素の形式で、ファイルの形式ではない）
  const sourceFormat = useEditorStore(originalFormatOf);
  const open = useEditorStore((s) => s.phase === 'open');
  const id = useId();
  const [originalName, setOriginalName] = useState<{ id: number; name: string } | null>(null);
  const itemId = item?.id ?? null;
  useEffect(() => {
    if (itemId === null) return;
    let alive = true;
    getItemDetail(itemId).then(
      (d) => {
        if (alive) setOriginalName({ id: itemId, name: d.originalName });
      },
      () => undefined,
    );
    return () => {
      alive = false;
    };
  }, [itemId]);
  const formats = formatsFor(item);
  if (!item || !formats) return null;
  const choice = outputChoice(outputs);
  const eff = formats.effectiveFormat(choice, sourceFormat);
  const keepable = formats.canWrite(sourceFormat);
  const keepRow = unavailableRow(rows, sourceFormat);
  const keepName = formatName(sourceFormat);
  const options = [
    ...(keepable
      ? [
          keepRow
            ? {
                value: 'keep',
                label: t.outputKeepUnavailable(keepName, unavailableNote(keepRow)),
                disabled: true,
              }
            : { value: 'keep', label: t.outputKeep(keepName) },
        ]
      : []),
    ...formats.writable.map((f) => {
      const row = unavailableRow(rows, f);
      return row
        ? {
            value: f,
            label: t.outputOptionUnavailable(formatName(f), unavailableNote(row)),
            disabled: true,
          }
        : { value: f, label: formatName(f) };
    }),
  ];
  // 今の出力の形式が書けない（保存すると形式を選ぶシートが出る）
  const effRow = unavailableRow(rows, eff.format);
  const value = choice.type === 'to' ? choice.format : keepable ? 'keep' : formats.fallback;
  const original = originalName?.id === item.id ? originalName.name : null;
  // 保存したあとの拡張子は今の拡張子から（形式が今と同じなら今の拡張子のまま）
  const name = original
    ? formats.exportName(original, formats.savedExt(eff.format, item.ext))
    : null;
  return (
    <section className={styles.output} aria-labelledby={id}>
      <div className={styles.paramRow}>
        <span id={id} className={styles.paramLabel}>
          {t.output}
        </span>
        <Select
          aria-labelledby={id}
          value={value}
          options={options}
          disabled={!open}
          onChange={(v) =>
            setOutputFormat(v === 'keep' ? { type: 'keep' } : { type: 'to', format: v })
          }
        />
      </div>
      {eff.forced && (
        <p className={styles.note}>
          {t.outputUnwritable(formatName(sourceFormat), formatName(eff.format))}
        </p>
      )}
      {effRow && (
        <p className={styles.note}>{formatUnavailableText(formatName(eff.format), effRow, kind)}</p>
      )}
      {name && original && name !== original && <p className={styles.note}>{t.exportName(name)}</p>}
    </section>
  );
}

/**
 * 右パネルの「操作」：操作の並び・「操作を追加」・選んだ操作の引数・警告・出力の形式（種類ごとの部品 `EditorKindUi.Ops`
 * から使う）。`size` は引数の欄の既定値に使う大きさ、`time` は標本の引数を時間の欄にするレートと長さ（音声）、`output` は
 * 出力の行の差し替え（省けば `FormatOutputRow`）。
 */
export function OpsPanel({
  size = null,
  time,
  output,
  fields,
}: {
  size?: ParamSize;
  time?: ParamTime;
  output?: ReactNode;
  fields?: ParamFieldOverrides;
}) {
  return (
    <div className={styles.ops}>
      <h2 className={styles.panelTitle}>{t.opsTitle}</h2>
      <div className={styles.stack}>
        <OpStack />
        <AddOpButton />
      </div>
      <div className={styles.paramsScroll}>
        <ParamsPanel size={size} time={time} fields={fields} />
        <Warnings />
      </div>
      {output ?? <FormatOutputRow />}
    </div>
  );
}
