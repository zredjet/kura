import { useEffect, useId, useRef, useState } from 'react';

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
  removeNode,
  selectNode,
  setNodeEnabled,
  setOutputFormat,
  useEditorStore,
} from './editorStore';
import {
  canWrite,
  effectiveFormat,
  exportName,
  FALLBACK_FORMAT,
  formatOfExt,
  outputChoice,
  savedExt,
  WRITABLE_IMAGE_FORMATS,
} from './formats';
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

/** 点線の「操作を追加」（分類ごとに横へ開くメニュー）。描画は描く道具で足すので出さない。 */
function AddOpButton() {
  const open = useEditorStore((s) => s.phase === 'open');
  const entries = (): MenuEntry[] =>
    addableOps()
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

/** 選んだ操作の引数パネル（`ParamSchema` から組む。表示の条件・「詳細」・指摘・後ろの描画の知らせ）。 */
function ParamsPanel() {
  const selected = useEditorStore((s) => s.selected);
  const node = useEditorStore((s) => (s.selected === null ? undefined : s.doc.nodes[s.selected]));
  const doc = useEditorStore((s) => s.doc);
  const issues = useEditorStore((s) => s.issues);
  const warnings = useEditorStore((s) => s.warnings);
  const view = useEditorStore((s) => s.view);
  const headingId = useId();
  if (selected === null || !node) return <p className={styles.opsHint}>{t.opsHint}</p>;
  const desc = opSpec(node.op, node.v);
  const latest = opSpec(node.op);
  const size = view ? { w: view.fullWidth, h: view.fullHeight } : null;
  const params = paramsOf(node);
  const all = [...issues, ...warnings];
  let body;
  if (!desc) {
    body = <p className={styles.opsHint}>{latest ? t.legacyOp : t.unknownOp}</p>;
  } else {
    const { basic, advanced } = visibleParams(desc, params);
    // ノードと引数で key する（別のノードを選んだら欄を作り直し、前のノードの手元の値を持ち越さない）
    const field = (spec: (typeof basic)[number]) => (
      <ParamField
        key={`${node.id}:${spec.key}`}
        node={node.id}
        op={node.op}
        spec={spec}
        value={paramValue(params, spec)}
        issues={all}
        size={size}
        count={node.log?.field === spec.key ? node.log.count : null}
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

/** 出力の形式（形式が変わると「書き出すときの名前：hero.webp」）。 */
function OutputRow() {
  const outputs = useEditorStore((s) => s.doc.outputs);
  const item = useEditorStore((s) => s.item);
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
  if (!item) return null;
  const choice = outputChoice(outputs);
  // 元のファイルの形式（source.format は画素の形式で、ファイルの形式ではない）
  const sourceFormat = formatOfExt(item.ext);
  const eff = effectiveFormat(choice, sourceFormat);
  const keepable = canWrite(sourceFormat);
  const options = [
    ...(keepable ? [{ value: 'keep', label: t.outputKeep(formatName(sourceFormat)) }] : []),
    ...WRITABLE_IMAGE_FORMATS.map((f) => ({ value: f, label: formatName(f) })),
  ];
  const value = choice.type === 'to' ? choice.format : keepable ? 'keep' : FALLBACK_FORMAT;
  const original = originalName?.id === item.id ? originalName.name : null;
  const name = original ? exportName(original, savedExt(eff.format, item.ext)) : null;
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
      {name && original && name !== original && <p className={styles.note}>{t.exportName(name)}</p>}
    </section>
  );
}

/** 右パネルの「操作」：操作の並び・「操作を追加」・選んだ操作の引数・警告・出力の形式。 */
export function OpsPanel() {
  return (
    <div className={styles.ops}>
      <h2 className={styles.panelTitle}>{t.opsTitle}</h2>
      <div className={styles.stack}>
        <OpStack />
        <AddOpButton />
      </div>
      <div className={styles.paramsScroll}>
        <ParamsPanel />
        <Warnings />
      </div>
      <OutputRow />
    </div>
  );
}
